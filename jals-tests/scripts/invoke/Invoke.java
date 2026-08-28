// The agreement rung of the `jals-wasm` corpus harness: call the very methods the emitted module
// exports, on javac's own class files, and print what a real JVM answers.
//
// Run in source-file mode (JEP 330), so nothing has to build or vendor a jar:
//
//     java jals-tests/scripts/invoke/Invoke.java <calls.tsv>
//
// Input, one line per call:
//     <key> \t <expected dir> \t <binary class name> \t <method> \t <descriptor> \t <arg,arg,...>
//
// Output, one line per call:
//     VAL \t <key> \t <canonical result>   the call returned; the result in the shared spelling
//     EXC \t <key> \t <throwable>          the call threw
//     ERR \t <key> \t <detail>             the call never happened — no such class or method
//
// # The canonical spelling
//
// The wasm side reads its answer out of `wasmtime`, which knows only i32/i64/f32/f64. What a Java
// `byte` or `char` result *means* lives in javac's descriptor, so the descriptor decides the
// spelling on both sides:
//
//   - Z becomes 0 or 1, which is what the module returns.
//   - B, S, I and C become a decimal int — a `char` as its code point, and a `byte` as its own
//     signed value, so a narrowing the backend skipped is a difference rather than a formatting
//     quirk.
//   - J becomes a decimal long.
//   - F and D become their bit patterns, through floatToIntBits/doubleToLongBits. Neither side's
//     printed decimal is the value (`-0` against `-0.0`), and those two methods canonicalise NaN,
//     which is the only sane answer when NaN payloads are free to differ.
//   - V prints nothing: completing is the whole of the answer.
//
// # One class loader per call
//
// `wasmtime run --invoke` is a fresh process, so the module's globals start over on every call. A
// driver that invoked every method in one loader would let a `static` field written by one call be
// read by the next on the JVM side only, and every method that reads mutable static state would
// disagree for a reason that is not the compiler. So each call gets a loader of its own, and the
// two sides start from the same place.
//
// The loader's parent is the platform loader, exactly as the verifier driver's is: the JDK's own
// modules are visible, this driver's class path is not.
import java.io.BufferedWriter;
import java.io.IOException;
import java.io.OutputStreamWriter;
import java.io.UncheckedIOException;
import java.io.Writer;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.net.URL;
import java.net.URLClassLoader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;

public final class Invoke {
    /** How long one call may run before it is abandoned. Mirrors the engine's own timeout. */
    private static final long CALL_TIMEOUT_MILLIS = 10_000;

    public static void main(String[] args) throws IOException, InterruptedException {
        if (args.length != 1) {
            System.err.println("usage: Invoke <calls.tsv>");
            System.exit(2);
        }
        List<String> lines = Files.readAllLines(Path.of(args[0]));
        Writer out = new BufferedWriter(new OutputStreamWriter(System.out, StandardCharsets.UTF_8));
        // A corpus method that calls System.exit skips every `finally`; the hook is what keeps the
        // answers already produced from going down with it.
        Runtime.getRuntime().addShutdownHook(new Thread(() -> flush(out)));

        for (String line : lines) {
            String[] parts = line.split("\t", 6);
            if (parts.length < 5) {
                continue;
            }
            String key = parts[0];
            String arguments = parts.length > 5 ? parts[5] : "";
            call(out, key, parts[1], parts[2], parts[3], parts[4], arguments);
            out.flush();
        }
        out.flush();
    }

    /**
     * Make one call on its own daemon thread, through a class loader of its own.
     *
     * <p>The thread is what bounds a corpus method that does not terminate; a thread that outlives
     * its timeout is left behind deliberately, since it cannot be killed safely and as a daemon it
     * does not keep the JVM alive.
     */
    private static void call(
            Writer out,
            String key,
            String directory,
            String owner,
            String name,
            String descriptor,
            String arguments)
            throws IOException, InterruptedException {
        Thread worker = new Thread(() -> {
            try (URLClassLoader loader = loader(directory)) {
                Class<?> type = Class.forName(owner, true, loader);
                Class<?>[] parameters = parameterTypes(descriptor, loader);
                Method method = type.getDeclaredMethod(name, parameters);
                // The backend exports every static method whatever its visibility, so the oracle
                // has to reach every static method too.
                method.setAccessible(true);
                Object[] values = parse(descriptor, arguments);
                Object result = method.invoke(null, values);
                write(out, "VAL\t" + key + "\t" + canonical(descriptor, result));
            } catch (InvocationTargetException e) {
                write(out, "EXC\t" + key + "\t" + describe(e.getCause()));
            } catch (ExceptionInInitializerError e) {
                write(out, "EXC\t" + key + "\t" + describe(e.getCause() == null ? e : e.getCause()));
            } catch (Throwable t) {
                write(out, "ERR\t" + key + "\t" + describe(t));
            }
        }, "invoke");
        worker.setDaemon(true);
        worker.start();
        worker.join(CALL_TIMEOUT_MILLIS);
        if (worker.isAlive()) {
            out.write("ERR\t" + key + "\ttimeout after " + CALL_TIMEOUT_MILLIS + "ms\n");
        }
    }

    /** A loader over one case's `expected/` directory, parented at the platform loader. */
    private static URLClassLoader loader(String directory) throws IOException {
        return new URLClassLoader(
                new URL[] {Path.of(directory).toUri().toURL()}, ClassLoader.getPlatformClassLoader());
    }

    /** The parameter types a descriptor names, which are primitives throughout by construction. */
    private static Class<?>[] parameterTypes(String descriptor, ClassLoader loader) {
        String params = descriptor.substring(1, descriptor.indexOf(')'));
        Class<?>[] types = new Class<?>[params.length()];
        for (int i = 0; i < params.length(); i++) {
            types[i] = primitive(params.charAt(i));
        }
        return types;
    }

    /** The `Class` of one primitive descriptor letter. */
    private static Class<?> primitive(char letter) {
        return switch (letter) {
            case 'Z' -> boolean.class;
            case 'B' -> byte.class;
            case 'C' -> char.class;
            case 'S' -> short.class;
            case 'I' -> int.class;
            case 'J' -> long.class;
            case 'F' -> float.class;
            case 'D' -> double.class;
            default -> throw new IllegalArgumentException("not a primitive: " + letter);
        };
    }

    /** The comma-separated decimal arguments, read as the types the descriptor names. */
    private static Object[] parse(String descriptor, String arguments) {
        String params = descriptor.substring(1, descriptor.indexOf(')'));
        if (params.isEmpty()) {
            return new Object[0];
        }
        String[] fields = arguments.split(",", -1);
        List<Object> values = new ArrayList<>(params.length());
        for (int i = 0; i < params.length(); i++) {
            String text = i < fields.length ? fields[i].trim() : "0";
            values.add(switch (params.charAt(i)) {
                case 'Z' -> !text.equals("0");
                case 'B' -> (byte) Long.parseLong(text);
                case 'C' -> (char) Long.parseLong(text);
                case 'S' -> (short) Long.parseLong(text);
                case 'I' -> (int) Long.parseLong(text);
                case 'J' -> Long.parseLong(text);
                case 'F' -> Float.parseFloat(text);
                case 'D' -> Double.parseDouble(text);
                default -> throw new IllegalArgumentException("not a primitive: " + text);
            });
        }
        return values.toArray();
    }

    /** One returned value in the spelling the wasm side is normalised into. */
    private static String canonical(String descriptor, Object result) {
        char letter = descriptor.charAt(descriptor.indexOf(')') + 1);
        return switch (letter) {
            case 'V' -> "";
            case 'Z' -> ((Boolean) result) ? "1" : "0";
            case 'B' -> Long.toString((Byte) result);
            case 'C' -> Long.toString((Character) result);
            case 'S' -> Long.toString((Short) result);
            case 'I' -> Long.toString((Integer) result);
            case 'J' -> Long.toString((Long) result);
            case 'F' -> String.format("0x%08x", Float.floatToIntBits((Float) result));
            case 'D' -> String.format("0x%016x", Double.doubleToLongBits((Double) result));
            default -> throw new IllegalArgumentException("not a primitive: " + letter);
        };
    }

    /** One line naming the throwable and what it said. */
    private static String describe(Throwable t) {
        if (t == null) {
            return "null";
        }
        String message = t.getMessage();
        String text = message == null
                ? t.getClass().getName()
                : t.getClass().getName() + ": " + message.split("\\R", 2)[0].trim();
        String flattened = text.replace('\n', ' ').replace('\r', ' ').replace('\t', ' ');
        return flattened.length() > 200 ? flattened.substring(0, 200) + "…" : flattened;
    }

    /** Append one answer. The writer is shared with the timeout thread, so it is guarded. */
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
}
