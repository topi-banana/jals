package java.lang;

/**
 * The {@code long} wrapper, and this package's integral text conversions.
 *
 * <p>{@link #toString(long, int)} and {@link #parseLong(String, int)} are where the digit work
 * lives; {@link Integer}, {@link Short} and {@link Byte} all reach them and narrow, so there is one
 * radix loop in the package rather than four that agree until one is fixed.
 */
public final class Long extends Number implements Comparable<Long> {

    /** The most negative {@code long}. */
    public static final long MIN_VALUE = -9223372036854775808L;

    /** The largest {@code long}. */
    public static final long MAX_VALUE = 9223372036854775807L;

    /** How many bits a {@code long} occupies. */
    public static final int SIZE = 64;

    /** How many bytes a {@code long} occupies. */
    public static final int BYTES = 8;

    /** The widest a base-two rendering of a {@code long} can be, with a sign. */
    private static final int MAX_DIGITS = 65;

    private final long value;

    public Long(long value) {
        this.value = value;
    }

    /** A wrapper holding {@code value}. */
    public static Long valueOf(long value) {
        return new Long(value);
    }

    @Override
    public int intValue() {
        return (int) this.value;
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
    public static String toString(long value) {
        return toString(value, 10);
    }

    /**
     * {@code value} in {@code radix}, or in base ten when {@code radix} is out of range.
     *
     * <p>The digits are accumulated on the <em>negative</em> side. {@link #MIN_VALUE} has no
     * positive counterpart, so negating it to make the loop uniform would wrap it back to itself
     * and print the wrong number; every other approach needs a special case for exactly that value.
     */
    public static String toString(long value, int radix) {
        int base = radix < Character.MIN_RADIX || radix > Character.MAX_RADIX ? 10 : radix;
        char[] digits = new char[MAX_DIGITS];
        int at = MAX_DIGITS;
        boolean negative = value < 0;
        long remaining = negative ? value : -value;
        do {
            long digit = -(remaining % base);
            remaining = remaining / base;
            at--;
            digits[at] = Character.forDigit((int) digit, base);
        } while (remaining != 0);
        if (negative) {
            at--;
            digits[at] = '-';
        }
        return new String(digits, at, MAX_DIGITS - at);
    }

    /** {@code value} in base sixteen, unsigned. */
    public static String toHexString(long value) {
        return toUnsignedString(value, 16);
    }

    /** {@code value} in base eight, unsigned. */
    public static String toOctalString(long value) {
        return toUnsignedString(value, 8);
    }

    /** {@code value} in base two, unsigned. */
    public static String toBinaryString(long value) {
        return toUnsignedString(value, 2);
    }

    /**
     * {@code value} as an unsigned number in a power-of-two {@code radix}.
     *
     * <p>Only powers of two, and that is why it can shift rather than divide: an unsigned division
     * of a negative {@code long} is the one arithmetic this package would have to write by hand.
     */
    private static String toUnsignedString(long value, int radix) {
        int shift = radix == 16 ? 4 : (radix == 8 ? 3 : 1);
        long mask = radix - 1;
        char[] digits = new char[MAX_DIGITS];
        int at = MAX_DIGITS;
        long remaining = value;
        do {
            at--;
            digits[at] = Character.forDigit((int) (remaining & mask), radix);
            remaining = remaining >>> shift;
        } while (remaining != 0);
        return new String(digits, at, MAX_DIGITS - at);
    }

    /** The {@code long} {@code text} spells in base ten. */
    public static long parseLong(String text) {
        return parseLong(text, 10);
    }

    /**
     * The {@code long} {@code text} spells in {@code radix}.
     *
     * <p>Accumulated negatively for the reason {@link #toString(long, int)} builds negatively:
     * {@code "-9223372036854775808"} is a legal spelling with no positive counterpart.
     *
     * @throws NumberFormatException if {@code text} does not spell one
     */
    public static long parseLong(String text, int radix) {
        if (text == null || text.isEmpty()) {
            throw new NumberFormatException(text);
        }
        int base = radix < Character.MIN_RADIX || radix > Character.MAX_RADIX ? 10 : radix;
        int at = 0;
        boolean negative = text.charAt(0) == '-';
        if (negative || text.charAt(0) == '+') {
            at = 1;
        }
        if (at == text.length()) {
            throw new NumberFormatException(text);
        }
        long accumulated = 0;
        while (at < text.length()) {
            int digit = Character.digit(text.charAt(at), base);
            if (digit < 0) {
                throw new NumberFormatException(text);
            }
            long shifted = accumulated * base;
            if (shifted / base != accumulated) {
                throw new NumberFormatException(text);
            }
            accumulated = shifted - digit;
            if (accumulated > 0) {
                throw new NumberFormatException(text);
            }
            at++;
        }
        if (negative) {
            return accumulated;
        }
        if (accumulated == MIN_VALUE) {
            throw new NumberFormatException(text);
        }
        return -accumulated;
    }

    /** {@code value}'s hash: its two halves folded together. */
    public static int hashCode(long value) {
        return (int) (value ^ (value >>> 32));
    }

    /** Negative, zero or positive as {@code left} orders before, with, or after {@code right}. */
    public static int compare(long left, long right) {
        if (left < right) {
            return -1;
        }
        return left > right ? 1 : 0;
    }

    /** The larger of two values. */
    public static long max(long left, long right) {
        return left >= right ? left : right;
    }

    /** The smaller of two values. */
    public static long min(long left, long right) {
        return left <= right ? left : right;
    }

    @Override
    public boolean equals(Object other) {
        if (other == this) {
            return true;
        }
        if (!(other instanceof Long)) {
            return false;
        }
        return ((Long) other).value == this.value;
    }

    @Override
    public int hashCode() {
        return hashCode(this.value);
    }

    @Override
    public int compareTo(Long other) {
        return compare(this.value, other.value);
    }

    @Override
    public String toString() {
        return toString(this.value);
    }
}
