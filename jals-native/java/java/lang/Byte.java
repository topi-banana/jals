package java.lang;

/** A {@code byte}, as an object. */
public final class Byte extends Number implements Comparable<Byte> {

    /** {@code -128}. */
    public static final byte MIN_VALUE = -128;

    /** {@code 127}. */
    public static final byte MAX_VALUE = 127;

    /** How many bits a {@code byte} has. */
    public static final int SIZE = 8;

    /** How many bytes a {@code byte} has. */
    public static final int BYTES = 1;

    /** {@code "byte"}, the only way this target can spell a constant string. */
    private static final char[] TYPE_CHARS = {'b', 'y', 't', 'e'};

    /** The identity of the primitive this class wraps. */
    public static final Class TYPE = new Class(new String(TYPE_CHARS));

    /** The wrapped value. */
    private final byte value;

    /** A wrapper around {@code value}. */
    public Byte(byte value) {
        this.value = value;
    }

    /** A wrapper around {@code value}. */
    public static Byte valueOf(byte value) {
        return new Byte(value);
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
    public byte byteValue() {
        return this.value;
    }

    /** The wrapped value. */
    @Override
    public int hashCode() {
        return this.value;
    }

    /** Whether {@code other} is a {@code Byte} wrapping the same value. */
    @Override
    public boolean equals(Object other) {
        if (!(other instanceof Byte)) {
            return false;
        }
        return ((Byte) other).value == this.value;
    }

    @Override
    public int compareTo(Byte other) {
        return this.value - other.value;
    }

    /** The wrapped value in decimal. */
    @Override
    public String toString() {
        return Integer.toString(this.value);
    }

    /** {@code value} in decimal. */
    public static String toString(byte value) {
        return Integer.toString(value);
    }

    /** The {@code byte} {@code text} spells in decimal. */
    public static byte parseByte(String text) {
        int parsed = Integer.parseInt(text, 10);
        if (parsed < MIN_VALUE || parsed > MAX_VALUE) {
            throw new NumberFormatException(text);
        }
        return (byte) parsed;
    }

    /** A negative number, zero, or a positive number as {@code left} sorts before, with, or after {@code right}. */
    public static int compare(byte left, byte right) {
        return left - right;
    }
}
