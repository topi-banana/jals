package java.lang;

/** The supertype of every numeric wrapper. */
public abstract class Number {

    /** This value as an {@code int}, narrowing if it must. */
    public abstract int intValue();

    /** This value as a {@code long}, narrowing or widening as it must. */
    public abstract long longValue();

    /** This value as a {@code float}. */
    public abstract float floatValue();

    /** This value as a {@code double}. */
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
