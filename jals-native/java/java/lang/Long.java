package java.lang;

/**
 * A {@code long}, as an object, and the {@code long} operations that have nowhere else to live.
 *
 * <p>{@link Integer}'s twin, and written out rather than delegated: every accumulator below is a
 * {@code long}, and a version that computed in {@code int} and widened would be wrong in the top
 * half of the range for every one of them.
 */
public final class Long extends Number implements Comparable<Long> {

    /** {@code -9223372036854775808}. */
    public static final long MIN_VALUE = -9223372036854775808L;

    /** {@code 9223372036854775807}. */
    public static final long MAX_VALUE = 9223372036854775807L;

    /** How many bits a {@code long} has. */
    public static final int SIZE = 64;

    /** How many bytes a {@code long} has. */
    public static final int BYTES = 8;

    /** {@code "long"}, the only way this target can spell a constant string. */
    private static final char[] TYPE_CHARS = {'l', 'o', 'n', 'g'};

    /** The identity of the primitive this class wraps. */
    public static final Class TYPE = new Class(new String(TYPE_CHARS));

    /** The digits of every radix this class renders in, indexed by digit value. */
    private static final char[] DIGITS = {
        '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h',
        'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z'
    };

    /** The wrapped value. */
    private final long value;

    /** A wrapper around {@code value}. */
    public Long(long value) {
        this.value = value;
    }

    /** A wrapper around {@code value}. */
    public static Long valueOf(long value) {
        return new Long(value);
    }

    /** The value {@code text} spells in decimal, wrapped. */
    public static Long valueOf(String text) {
        return new Long(parseLong(text, 10));
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
        return this.value;
    }

    @Override
    public double doubleValue() {
        return this.value;
    }

    /** The two halves exclusive-ored together, which is what the JDK's {@code Long.hashCode} answers. */
    @Override
    public int hashCode() {
        return (int) (this.value ^ (this.value >>> 32));
    }

    /** Whether {@code other} is a {@code Long} wrapping the same value. */
    @Override
    public boolean equals(Object other) {
        if (!(other instanceof Long)) {
            return false;
        }
        return ((Long) other).value == this.value;
    }

    @Override
    public int compareTo(Long other) {
        return compare(this.value, other.value);
    }

    /** The wrapped value in decimal. */
    @Override
    public String toString() {
        return toString(this.value);
    }

    /** {@code value} in decimal. */
    public static String toString(long value) {
        return toString(value, 10);
    }

    /**
     * {@code value} in {@code radix}, or in decimal when {@code radix} is outside 2..36.
     *
     * <p>Rendered from the negative side, for the reason {@link Integer#toString(int, int)} is:
     * {@code -Long.MIN_VALUE} does not fit a {@code long}.
     */
    public static String toString(long value, int radix) {
        int base = radix;
        if (base < 2 || base > 36) {
            base = 10;
        }
        if (value == 0L) {
            return String.valueOf('0');
        }
        char[] digits = new char[65];
        int at = digits.length;
        boolean negative = value < 0L;
        long rest = value;
        if (!negative) {
            rest = -value;
        }
        while (rest != 0L) {
            at = at - 1;
            digits[at] = DIGITS[(int) -(rest % base)];
            rest = rest / base;
        }
        if (negative) {
            at = at - 1;
            digits[at] = '-';
        }
        return new String(digits, at, digits.length - at);
    }

    /** {@code value} in base two, with no sign and no leading zeros. */
    public static String toBinaryString(long value) {
        return toUnsignedString(value, 1);
    }

    /** {@code value} in base eight, with no sign and no leading zeros. */
    public static String toOctalString(long value) {
        return toUnsignedString(value, 3);
    }

    /** {@code value} in base sixteen, with no sign and no leading zeros. */
    public static String toHexString(long value) {
        return toUnsignedString(value, 4);
    }

    /** The {@code long} {@code text} spells in decimal. */
    public static long parseLong(String text) {
        return parseLong(text, 10);
    }

    /** The {@code long} {@code text} spells in {@code radix}. */
    public static long parseLong(String text, int radix) {
        if (text == null || radix < 2 || radix > 36) {
            throw new NumberFormatException(text);
        }
        int length = text.length();
        if (length == 0) {
            throw new NumberFormatException(text);
        }
        int at = 0;
        boolean negative = false;
        char first = text.charAt(0);
        if (first == '-' || first == '+') {
            negative = first == '-';
            at = 1;
            if (length == 1) {
                throw new NumberFormatException(text);
            }
        }
        long limit = negative ? MIN_VALUE : -MAX_VALUE;
        long cutoff = limit / radix;
        long total = 0L;
        while (at < length) {
            int digit = Character.digit(text.charAt(at), radix);
            if (digit < 0 || total < cutoff) {
                throw new NumberFormatException(text);
            }
            total = total * radix;
            if (total < limit + digit) {
                throw new NumberFormatException(text);
            }
            total = total - digit;
            at = at + 1;
        }
        return negative ? total : -total;
    }

    /** A negative number, zero, or a positive number as {@code left} sorts before, with, or after {@code right}. */
    public static int compare(long left, long right) {
        if (left < right) {
            return -1;
        }
        if (left > right) {
            return 1;
        }
        return 0;
    }

    /** The larger of the two. */
    public static long max(long left, long right) {
        return left > right ? left : right;
    }

    /** The smaller of the two. */
    public static long min(long left, long right) {
        return left < right ? left : right;
    }

    /** {@code left + right}, named so it can be passed where a method is wanted. */
    public static long sum(long left, long right) {
        return left + right;
    }

    /** {@code -1}, {@code 0}, or {@code 1} as {@code value} is negative, zero, or positive. */
    public static int signum(long value) {
        return (int) ((value >> 63) | (-value >>> 63));
    }

    /** How many bits of {@code value} are one. */
    public static int bitCount(long value) {
        long rest = value;
        int count = 0;
        while (rest != 0L) {
            rest = rest & (rest - 1L);
            count = count + 1;
        }
        return count;
    }

    /** How many zero bits precede the highest one bit, or 64 when there is none. */
    public static int numberOfLeadingZeros(long value) {
        if (value == 0L) {
            return 64;
        }
        long rest = value;
        int count = 0;
        while (rest > 0L) {
            rest = rest << 1;
            count = count + 1;
        }
        return count;
    }

    /** How many zero bits follow the lowest one bit, or 64 when there is none. */
    public static int numberOfTrailingZeros(long value) {
        if (value == 0L) {
            return 64;
        }
        long rest = value;
        int count = 0;
        while ((rest & 1L) == 0L) {
            rest = rest >>> 1;
            count = count + 1;
        }
        return count;
    }

    /** {@code value} with every bit but the highest one cleared. */
    public static long highestOneBit(long value) {
        if (value == 0L) {
            return 0L;
        }
        return 1L << (63 - numberOfLeadingZeros(value));
    }

    /** {@code value} with every bit but the lowest one cleared. */
    public static long lowestOneBit(long value) {
        return value & -value;
    }

    /** {@code value} with its bits rotated left by {@code distance}. */
    public static long rotateLeft(long value, int distance) {
        return (value << distance) | (value >>> -distance);
    }

    /** {@code value} with its bits rotated right by {@code distance}. */
    public static long rotateRight(long value, int distance) {
        return (value >>> distance) | (value << -distance);
    }

    /** {@code value} with its bits in the opposite order. */
    public static long reverse(long value) {
        long reversed = 0L;
        long rest = value;
        int at = 0;
        while (at < 64) {
            reversed = (reversed << 1) | (rest & 1L);
            rest = rest >>> 1;
            at = at + 1;
        }
        return reversed;
    }

    /** {@code value} read as unsigned, in a radix that is a power of two. */
    private static String toUnsignedString(long value, int shift) {
        if (value == 0L) {
            return String.valueOf('0');
        }
        long mask = (1L << shift) - 1L;
        char[] digits = new char[64];
        int at = digits.length;
        long rest = value;
        while (rest != 0L) {
            at = at - 1;
            digits[at] = DIGITS[(int) (rest & mask)];
            rest = rest >>> shift;
        }
        return new String(digits, at, digits.length - at);
    }
}
