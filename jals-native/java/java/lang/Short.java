package java.lang;

/** A {@code short}, as an object. */
public final class Short extends Number implements Comparable<Short> {

    /** {@code -32768}. */
    public static final short MIN_VALUE = -32768;

    /** {@code 32767}. */
    public static final short MAX_VALUE = 32767;

    /** How many bits a {@code short} has. */
    public static final int SIZE = 16;

    /** How many bytes a {@code short} has. */
    public static final int BYTES = 2;

    /** {@code "short"}, the only way this target can spell a constant string. */
    private static final char[] TYPE_CHARS = {'s', 'h', 'o', 'r', 't'};

    /** The identity of the primitive this class wraps. */
    public static final Class TYPE = new Class(new String(TYPE_CHARS));

    /** The wrapped value. */
    private final short value;

    /** A wrapper around {@code value}. */
    public Short(short value) {
        this.value = value;
    }

    /** A wrapper around {@code value}. */
    public static Short valueOf(short value) {
        return new Short(value);
    }

    @Override
    public int intValue() {
        return this.value;
    }

    @Override
    public long longValue() {
        return this.value;
    }

    @Override
    public float floatValue() {
        return this.value;
    }

    @Override
    public double doubleValue() {
        return this.value;
    }

    @Override
    public short shortValue() {
        return this.value;
    }

    /** The wrapped value. */
    @Override
    public int hashCode() {
        return this.value;
    }

    /** Whether {@code other} is a {@code Short} wrapping the same value. */
    @Override
    public boolean equals(Object other) {
        if (!(other instanceof Short)) {
            return false;
        }
        return ((Short) other).value == this.value;
    }

    @Override
    public int compareTo(Short other) {
        return this.value - other.value;
    }

    /** The wrapped value in decimal. */
    @Override
    public String toString() {
        return Integer.toString(this.value);
    }

    /** {@code value} in decimal. */
    public static String toString(short value) {
        return Integer.toString(value);
    }

    /** The {@code short} {@code text} spells in decimal. */
    public static short parseShort(String text) {
        int parsed = Integer.parseInt(text, 10);
        if (parsed < MIN_VALUE || parsed > MAX_VALUE) {
            throw new NumberFormatException(text);
        }
        return (short) parsed;
    }

    /** A negative number, zero, or a positive number as {@code left} sorts before, with, or after {@code right}. */
    public static int compare(short left, short right) {
        return left - right;
    }

    /** {@code value} with its two bytes swapped. */
    public static short reverseBytes(short value) {
        return (short) (((value & 0xFF00) >> 8) | (value << 8));
    }
}
