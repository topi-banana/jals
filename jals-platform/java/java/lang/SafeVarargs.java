package java.lang;

/**
 * Asserts that a varargs parameter of a generic type is used safely.
 *
 * <p>Declared so the annotation is a type that resolves. Nothing here interprets it: what a
 * {@code @SuppressWarnings} silences is `jals-lint`'s question, and it reads the name off the
 * syntax rather than the resolved type.
 */
public @interface SafeVarargs {
}
