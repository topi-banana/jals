// The JVM rung of the `jals-compile` corpus harness: link every class file a corpus run emitted
// and report what the bytecode verifier said about it.
//
// Run in source-file mode (JEP 330), so nothing has to build or vendor a jar:
//
//     java jals-tests/scripts/verify/Verify.java <cases.tsv>
//
// Input, one line per case:  <staging dir> \t <case path> \t <binary name>,<binary name>,...
// Output, one line per class: OK|BAD|ERR \t <case path> \t <binary name> [\t <detail>]
//
//   OK   the verifier accepted the class
//   BAD  the verifier rejected it (VerifyError / ClassFormatError) — the class file is wrong
//   ERR  loading failed for a reason that is not about this class file's shape: a type it
//        references that this loader cannot see, or a static initializer that threw
//
// # Why the classes are initialized
//
// `Class.forName(name, true, loader)` is what actually runs the verifier. Verification is part of
// linking, and the JVM is free to defer linking until first use (JVMS §5.4), so the gentler
// alternatives do not check anything: `ClassLoader.resolveClass` was tried here first and passed
// class files that this same driver rejects when it initializes them.
//
// Initializing means running the corpus's static initializers, which in a corpus of *compiler
// tests* is arbitrary code. Two guards follow from that, and neither is optional:
//
//   - each case runs on a daemon thread with a timeout, so a static initializer that blocks or
//     loops does not hang the whole corpus; and
//   - every verdict is flushed as it is produced, and again from a shutdown hook, so a static
//     initializer that calls `System.exit` costs its own case rather than every case after it.
import java.io.BufferedWriter;
import java.io.IOException;
import java.io.OutputStreamWriter;
import java.io.UncheckedIOException;
import java.io.Writer;
import java.net.URL;
import java.net.URLClassLoader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;

public final class Verify {
    /** How long one case's classes may take to load before it is abandoned. */
    private static final long CASE_TIMEOUT_MILLIS = 20_000;

    public static void main(String[] args) throws IOException, InterruptedException {
        if (args.length != 1) {
            System.err.println("usage: Verify <cases.tsv>");
            System.exit(2);
        }
        List<String> lines = Files.readAllLines(Path.of(args[0]));
        Writer out = new BufferedWriter(new OutputStreamWriter(System.out, StandardCharsets.UTF_8));
        // A static initializer that calls System.exit skips every `finally`; the hook is what
        // keeps the verdicts already produced from going down with it.
        Runtime.getRuntime().addShutdownHook(new Thread(() -> flush(out)));

        for (String line : lines) {
            String[] parts = line.split("\t", 3);
            if (parts.length < 3) {
                continue;
            }
            verifyCase(out, parts[0], parts[1], parts[2].split(","));
            out.flush();
        }
        out.flush();
    }

    /**
     * Link every class one case emitted, through a loader that sees only that case.
     *
     * <p>On its own daemon thread: the classes are initialized, so this is running the corpus's
     * own static initializers, and one that blocks would otherwise stop the whole run. A thread
     * that outlives its timeout is left behind deliberately — it cannot be killed safely, and as
     * a daemon it does not keep the JVM alive.
     */
    private static void verifyCase(Writer out, String directory, String rel, String[] names)
            throws IOException, InterruptedException {
        Thread worker = new Thread(() -> {
            try (Linker linker = new Linker(directory)) {
                for (String name : names) {
                    if (name.isEmpty()) {
                        continue;
                    }
                    try {
                        linker.link(name);
                        write(out, "OK\t" + rel + "\t" + name);
                    } catch (VerifyError | ClassFormatError e) {
                        write(out, "BAD\t" + rel + "\t" + name + "\t" + describe(e));
                    } catch (Throwable t) {
                        write(out, "ERR\t" + rel + "\t" + name + "\t" + describe(t));
                    }
                }
            } catch (IOException e) {
                write(out, "ERR\t" + rel + "\t-\t" + describe(e));
            }
        }, "verify");
        worker.setDaemon(true);
        worker.start();
        worker.join(CASE_TIMEOUT_MILLIS);
        if (worker.isAlive()) {
            out.write("ERR\t" + rel + "\t-\ttimeout after " + CASE_TIMEOUT_MILLIS + "ms\n");
        }
    }

    /**
     * One line naming the throwable and what it said.
     *
     * <p>A {@code VerifyError} carries a whole report — the offending location, the reason, the
     * current frame, a hex dump of the method and its stack map. The location and the reason are
     * the two parts that say what is wrong; the dump belongs in a debugger session, not in a row
     * of a table, so only those two are kept.
     */
    private static String describe(Throwable t) {
        String message = t.getMessage();
        if (message == null) {
            return t.getClass().getName();
        }
        StringBuilder text = new StringBuilder(t.getClass().getName())
                .append(": ")
                .append(message.split("\\R", 2)[0].trim());
        String location = section(message, "Location:");
        if (!location.isEmpty()) {
            text.append(" @ ").append(location);
        }
        String reason = section(message, "Reason:");
        if (!reason.isEmpty()) {
            text.append(" — ").append(reason);
        }
        String flattened = text.toString().replace('\n', ' ').replace('\r', ' ').replace('\t', ' ');
        return flattened.length() > 300 ? flattened.substring(0, 300) + "…" : flattened;
    }

    /** The first line of the {@code heading:} section of a `VerifyError` report, or empty. */
    private static String section(String message, String heading) {
        int at = message.indexOf(heading);
        if (at < 0) {
            return "";
        }
        for (String line : message.substring(at + heading.length()).split("\\R")) {
            if (!line.isBlank()) {
                return line.trim();
            }
        }
        return "";
    }

    /** Append one verdict line. The writer is shared with the timeout thread, so it is guarded. */
    private static void write(Writer out, String line) {
        synchronized (out) {
            try {
                out.write(line);
                out.write('\n');
            } catch (IOException e) {
                throw new UncheckedIOException(e);
            }
        }
    }

    private static void flush(Writer out) {
        synchronized (out) {
            try {
                out.flush();
            } catch (IOException e) {
                // Nothing useful is left to do while the JVM is shutting down.
            }
        }
    }

    /**
     * A loader over one case's staging directory.
     *
     * <p>Its parent is the platform loader, so the JDK's own modules are visible — the verifier
     * resolves the types an instruction names against them — while the class path this driver was
     * launched with is not. A generated class must never be shadowed by one that is already
     * loaded: that would score the class file as accepted without its bytes ever being read.
     */
    private static final class Linker extends URLClassLoader {
        Linker(String directory) throws IOException {
            super(
                    new URL[] {Path.of(directory).toUri().toURL()},
                    ClassLoader.getPlatformClassLoader());
        }

        /** Load, link and initialize one class, so the bytecode verifier runs over it. */
        void link(String binaryName) throws ClassNotFoundException {
            Class.forName(binaryName, true, this);
        }
    }
}
