// A minimal driver for the Eclipse JDT code formatter, used to generate the
// `openjdk-eclipse` golden corpus. It is deliberately not part of the Rust workspace:
// `jals-tests/scripts/gen-openjdk-corpus.sh` compiles it against the JDT classpath that
// `fetch-eclipse-jdt.sh` resolves, runs it, and throws the class files away.
//
// The formatter runs outside OSGi. `ToolFactory.createCodeFormatter` needs nothing from
// the Eclipse runtime beyond the settings map, which is why the classpath can be a plain
// list of jars from Maven Central rather than an Eclipse installation.
//
//   java -cp <jdt-classpath>:<out> EclipseFormat <profile.prefs> <file.java>...
//       formats each file in place with the profile's settings.
//
//   java -cp <jdt-classpath>:<out> EclipseFormat --dump-defaults
//       writes JDT's own built-in default profile to stdout as a .prefs file. This is how
//       `jals-tests/config/eclipse-jals.prefs` was produced; regenerate it when the pinned
//       JDT version moves.

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Map;
import java.util.Properties;
import java.util.TreeMap;
import org.eclipse.jdt.core.ToolFactory;
import org.eclipse.jdt.core.formatter.CodeFormatter;
import org.eclipse.jdt.core.formatter.DefaultCodeFormatterConstants;
import org.eclipse.jface.text.Document;
import org.eclipse.jface.text.IDocument;
import org.eclipse.text.edits.TextEdit;

public final class EclipseFormat {

    /** Format whole compilation units, comments included. */
    private static final int KIND =
            CodeFormatter.K_COMPILATION_UNIT | CodeFormatter.F_INCLUDE_COMMENTS;

    private EclipseFormat() {}

    public static void main(String[] args) throws IOException {
        if (args.length == 1 && args[0].equals("--dump-defaults")) {
            dumpDefaults();
            return;
        }
        if (args.length < 2) {
            System.err.println("usage: EclipseFormat <profile.prefs> <file.java>...");
            System.err.println("       EclipseFormat --dump-defaults");
            System.exit(2);
        }

        Map<String, String> options = readProfile(Path.of(args[0]));
        CodeFormatter formatter = ToolFactory.createCodeFormatter(options);

        // A file the formatter declines is left untouched rather than failing the batch:
        // the corpus generator treats "came back byte-identical" as "skip this pair", and
        // the OpenJDK tree legitimately contains sources JDT cannot parse.
        int failed = 0;
        for (int i = 1; i < args.length; i++) {
            Path file = Path.of(args[i]);
            try {
                String source = Files.readString(file, StandardCharsets.UTF_8);
                String formatted = format(formatter, source);
                if (formatted != null && !formatted.equals(source)) {
                    Files.writeString(file, formatted, StandardCharsets.UTF_8);
                }
            } catch (IOException | RuntimeException e) {
                System.err.println("skipped " + file + ": " + e);
                failed++;
            }
        }
        if (failed > 0) {
            System.err.println("EclipseFormat: " + failed + " file(s) skipped");
        }
    }

    /**
     * Format one compilation unit, or return {@code null} when JDT produced no edit — which
     * is what it does for a source it cannot parse.
     */
    private static String format(CodeFormatter formatter, String source) {
        // The line separator is taken from the source itself (`null`), so a CRLF file is not
        // silently rewritten to LF and counted as a formatting difference.
        TextEdit edit = formatter.format(KIND, source, 0, source.length(), 0, null);
        if (edit == null) {
            return null;
        }
        IDocument document = new Document(source);
        try {
            edit.apply(document);
        } catch (org.eclipse.text.edits.MalformedTreeException
                | org.eclipse.jface.text.BadLocationException e) {
            return null;
        }
        return document.get();
    }

    /**
     * Read a `.prefs`/`.properties` profile into the settings map JDT expects.
     *
     * <p>Keys that are not formatter settings (a compliance level, say) are passed through
     * untouched: JDT reads what it recognizes, and so does `jals_fmt::import::eclipse`, which
     * is handed this very same file.
     */
    private static Map<String, String> readProfile(Path path) throws IOException {
        Properties properties = new Properties();
        try (var in = Files.newInputStream(path)) {
            properties.load(in);
        }
        Map<String, String> options = new TreeMap<>();
        for (String name : properties.stringPropertyNames()) {
            options.put(name, properties.getProperty(name));
        }
        return options;
    }

    /** Print JDT's built-in default profile in `.prefs` form, sorted for a stable diff. */
    private static void dumpDefaults() {
        Map<String, String> defaults =
                new TreeMap<>(DefaultCodeFormatterConstants.getEclipseDefaultSettings());
        StringBuilder out = new StringBuilder();
        for (Map.Entry<String, String> entry : defaults.entrySet()) {
            out.append(entry.getKey()).append('=').append(entry.getValue()).append('\n');
        }
        System.out.print(out);
    }
}
