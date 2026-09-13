package java.lang;

/**
 * The {@code short} wrapper.
 *
 * <p>Text conversion delegates to {@link Integer}'s and range-checks, so a spelling too wide for a
 * {@code short} refuses rather than wrapping.
 */
public final class Short extends Number implements Comparable<Short> {

    /** The most negative {@code short}. */
    public static final short MIN_VALUE = -32768;

    /** The largest {@code short}. */
    public static final short MAX_VALUE = 32767;

    /** How many bits a {@code short} occupies. */
    public static final int SIZE = 16;

    /** How many bytes a {@code short} occupies. */
    public static final int BYTES = 2;

    private final short value;

    public Short(short value) {
        this.value = value;
    }

    /** A wrapper holding {@code value}. */
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
        return (float) this.value;
    }

    @Override
    public double doubleValue() {
        return (double) this.value;
    }

    /** {@code value} in base ten. */
    public static String toString(short value) {
        return Integer.toString(value);
    }

    /**
     * The {@code short} {@code text} spells in base ten.
     *
     * @throws NumberFormatException if {@code text} does not spell one, or spells one too wide
     */
    public static short parseShort(String text) {
        int wide = Integer.parseInt(text);
        if (wide < MIN_VALUE || wide > MAX_VALUE) {
            throw new NumberFormatException(text);
        }
        return (short) wide;
    }

    /** Negative, zero or positive as {@code left} orders before, with, or after {@code right}. */
    public static int compare(short left, short right) {
        return left - right;
    }

    @Override
    public boolean equals(Object other) {
        if (other == this) {
            return true;
        }
        if (!(other instanceof Short)) {
            return false;
        }
        return ((Short) other).value == this.value;
    }

    @Override
    public int hashCode() {
        return this.value;
    }

    @Override
    public int compareTo(Short other) {
        return compare(this.value, other.value);
    }

    @Override
    public String toString() {
        return toString(this.value);
    }
}
