package java.lang;

/**
 * Marks a declaration that should no longer be used.
 *
 * <p>Declared so the annotation is a type that resolves. Nothing here interprets it: what a
 * {@code @SuppressWarnings} silences is `jals-lint`'s question, and it reads the name off the
 * syntax rather than the resolved type.
 */
public @interface Deprecated {
}
