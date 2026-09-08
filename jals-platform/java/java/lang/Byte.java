package java.lang;

/**
 * The {@code byte} wrapper.
 *
 * <p>Text conversion delegates to {@link Integer}'s and range-checks, so a spelling too wide for a
 * {@code byte} refuses rather than wrapping.
 */
public final class Byte extends Number implements Comparable<Byte> {

    /** The most negative {@code byte}. */
    public static final byte MIN_VALUE = -128;

    /** The largest {@code byte}. */
    public static final byte MAX_VALUE = 127;

    /** How many bits a {@code byte} occupies. */
    public static final int SIZE = 8;

    /** How many bytes a {@code byte} occupies. */
    public static final int BYTES = 1;

    private final byte value;

    public Byte(byte value) {
        this.value = value;
    }

    /** A wrapper holding {@code value}. */
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
        return (float) this.value;
    }

    @Override
    public double doubleValue() {
        return (double) this.value;
    }

    /** {@code value} in base ten. */
    public static String toString(byte value) {
        return Integer.toString(value);
    }

    /**
     * The {@code byte} {@code text} spells in base ten.
     *
     * @throws NumberFormatException if {@code text} does not spell one, or spells one too wide
     */
    public static byte parseByte(String text) {
        int wide = Integer.parseInt(text);
        if (wide < MIN_VALUE || wide > MAX_VALUE) {
            throw new NumberFormatException(text);
        }
        return (byte) wide;
    }

    /** Negative, zero or positive as {@code left} orders before, with, or after {@code right}. */
    public static int compare(byte left, byte right) {
        return left - right;
    }

    @Override
    public boolean equals(Object other) {
        if (other == this) {
            return true;
        }
        if (!(other instanceof Byte)) {
            return false;
        }
        return ((Byte) other).value == this.value;
    }

    @Override
    public int hashCode() {
        return this.value;
    }

    @Override
    public int compareTo(Byte other) {
        return compare(this.value, other.value);
    }

    @Override
    public String toString() {
        return toString(this.value);
    }
}
