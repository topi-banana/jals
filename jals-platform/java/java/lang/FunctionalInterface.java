package java.lang;

/**
 * Marks an interface with exactly one abstract method.
 *
 * <p>Declared so the annotation is a type that resolves. Nothing here interprets it: what a
 * {@code @SuppressWarnings} silences is `jals-lint`'s question, and it reads the name off the
 * syntax rather than the resolved type.
 */
public @interface FunctionalInterface {
}
