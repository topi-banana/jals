package java.lang;

/**
 * The {@code int} wrapper.
 *
 * <p>Every text conversion here delegates to {@link Long}'s and narrows. That is one radix loop in
 * the package instead of two that agree until somebody fixes one — and the narrowing is checked,
 * so {@code Integer.parseInt("2147483648")} refuses rather than wrapping to {@link #MIN_VALUE}.
 */
public final class Integer extends Number implements Comparable<Integer> {

    /** The most negative {@code int}. */
    public static final int MIN_VALUE = -2147483648;

    /** The largest {@code int}. */
    public static final int MAX_VALUE = 2147483647;

    /** How many bits an {@code int} occupies. */
    public static final int SIZE = 32;

    /** How many bytes an {@code int} occupies. */
    public static final int BYTES = 4;

    private final int value;

    public Integer(int value) {
        this.value = value;
    }

    /** A wrapper holding {@code value}. */
    public static Integer valueOf(int value) {
        return new Integer(value);
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
    public static String toString(int value) {
        return Long.toString(value, 10);
    }

    /** {@code value} in {@code radix}. */
    public static String toString(int value, int radix) {
        return Long.toString(value, radix);
    }

    /** {@code value} in base sixteen, unsigned and at {@code int} width. */
    public static String toHexString(int value) {
        return Long.toHexString(value & 0xFFFFFFFFL);
    }

    /** {@code value} in base eight, unsigned and at {@code int} width. */
    public static String toOctalString(int value) {
        return Long.toOctalString(value & 0xFFFFFFFFL);
    }

    /** {@code value} in base two, unsigned and at {@code int} width. */
    public static String toBinaryString(int value) {
        return Long.toBinaryString(value & 0xFFFFFFFFL);
    }

    /** The {@code int} {@code text} spells in base ten. */
    public static int parseInt(String text) {
        return parseInt(text, 10);
    }

    /**
     * The {@code int} {@code text} spells in {@code radix}.
     *
     * @throws NumberFormatException if {@code text} does not spell one, or spells one too wide
     */
    public static int parseInt(String text, int radix) {
        long wide = Long.parseLong(text, radix);
        if (wide < MIN_VALUE || wide > MAX_VALUE) {
            throw new NumberFormatException(text);
        }
        return (int) wide;
    }

    /** {@code value}'s hash, which is {@code value}. */
    public static int hashCode(int value) {
        return value;
    }

    /** Negative, zero or positive as {@code left} orders before, with, or after {@code right}. */
    public static int compare(int left, int right) {
        if (left < right) {
            return -1;
        }
        return left > right ? 1 : 0;
    }

    /** The larger of two values. */
    public static int max(int left, int right) {
        return left >= right ? left : right;
    }

    /** The smaller of two values. */
    public static int min(int left, int right) {
        return left <= right ? left : right;
    }

    @Override
    public boolean equals(Object other) {
        if (other == this) {
            return true;
        }
        if (!(other instanceof Integer)) {
            return false;
        }
        return ((Integer) other).value == this.value;
    }

    @Override
    public int hashCode() {
        return this.value;
    }

    @Override
    public int compareTo(Integer other) {
        return compare(this.value, other.value);
    }

    @Override
    public String toString() {
        return toString(this.value);
    }
}
