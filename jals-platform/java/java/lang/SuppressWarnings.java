package java.lang;

/**
 * Silences the named diagnostics inside the annotated declaration.
 *
 * <p>Declared so the annotation is a type that resolves. Nothing here interprets it: what a
 * {@code @SuppressWarnings} silences is `jals-lint`'s question, and it reads the name off the
 * syntax rather than the resolved type.
 */
public @interface SuppressWarnings {

    /** The diagnostic names to silence. */
    String[] value();
}
