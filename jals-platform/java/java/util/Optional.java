package java.util;

/** A container that holds a value or does not. */
public class Optional<T> {

    /** The value, if present. */
    public T get();

    /** Whether a value is present. */
    public boolean isPresent();

    /** Whether no value is present. */
    public boolean isEmpty();

    /** The value if present, otherwise {@code other}. */
    public T orElse(T other);
}
