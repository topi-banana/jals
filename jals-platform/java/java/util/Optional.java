package java.util;

/**
 * A container that holds a value or does not.
 *
 * <p>{@code final}, and with no declared constructor that a program could reach: the JDK's is
 * private, and the only ways to obtain one are the three factories below. A class here that left
 * them out would be a type correct code cannot construct and incorrect code can.
 */
public final class Optional<T> {

    /** An {@code Optional} holding {@code value}, which must not be {@code null}. */
    public static <T> Optional<T> of(T value);

    /** An {@code Optional} holding {@code value}, or an empty one when it is {@code null}. */
    public static <T> Optional<T> ofNullable(T value);

    /** An {@code Optional} holding nothing. */
    public static <T> Optional<T> empty();

    /** The value, if present. */
    public T get();

    /** Whether a value is present. */
    public boolean isPresent();

    /** Whether no value is present. */
    public boolean isEmpty();

    /** The value if present, otherwise {@code other}. */
    public T orElse(T other);
}
