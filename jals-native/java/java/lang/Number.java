package java.lang;

/**
 * The common supertype of the numeric wrappers.
 *
 * <p>Abstract in the JDK and abstract here: it declares the four conversions and implements none,
 * so a subclass that forgets one does not compile rather than answering zero.
 */
public abstract class Number {

    /** This value as an {@code int}, narrowing if it does not fit. */
    public abstract int intValue();

    /** This value as a {@code long}, narrowing if it does not fit. */
    public abstract long longValue();

    /** This value as a {@code float}, rounding if it does not fit. */
    public abstract float floatValue();

    /** This value as a {@code double}, rounding if it does not fit. */
    public abstract double doubleValue();

    /** This value as a {@code byte}. */
    public byte byteValue() {
        return (byte) intValue();
    }

    /** This value as a {@code short}. */
    public short shortValue() {
        return (short) intValue();
    }
}
