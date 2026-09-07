package java.lang;

/**
 * An {@code int}, as an object, and the {@code int} operations that have nowhere else to live.
 *
 * <p>Most of what is below is {@code static}: the JDK puts the whole of {@code int}'s vocabulary
 * on this class — decimal and radix rendering, parsing, and the bit operations a processor has and
 * an operator does not. None of it needs an instance and none of it is reachable another way.
 *
 * <p>The instance half is the wrapper, and on this target it is only ever reached by writing
 * {@link #valueOf} out: the backend has no boxing conversion, so {@code Integer x = 1;} does not
 * compile and {@code Integer x = Integer.valueOf(1);} does. That is a gap in the backend rather
 * than in this class, and this class is what the gap will be closed against.
 */
public final class Integer extends Number implements Comparable<Integer> {

    /** {@code -2147483648}. */
    public static final int MIN_VALUE = -2147483648;

    /** {@code 2147483647}. */
    public static final int MAX_VALUE = 2147483647;

    /** How many bits an {@code int} has. */
    public static final int SIZE = 32;

    /** How many bytes an {@code int} has. */
    public static final int BYTES = 4;

    /** {@code "int"}, the only way this target can spell a constant string. */
    private static final char[] TYPE_CHARS = {'i', 'n', 't'};

    /** The identity of the primitive this class wraps. */
    public static final Class TYPE = new Class(new String(TYPE_CHARS));

    /** The digits of every radix this class renders in, indexed by digit value. */
    private static final char[] DIGITS = {
        '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h',
        'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z'
    };

    /** The wrapped value. */
    private final int value;

    /**
     * A wrapper around {@code value}.
     *
     * <p>Deprecated in the JDK in favour of {@link #valueOf}, and kept for the same reason it is
     * kept there: a call written against the JDK compiles.
     */
    public Integer(int value) {
        this.value = value;
    }

    /** A wrapper around {@code value}. */
    public static Integer valueOf(int value) {
        return new Integer(value);
    }

    /** The value {@code text} spells in decimal, wrapped. */
    public static Integer valueOf(String text) {
        return new Integer(parseInt(text, 10));
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

    /** The wrapped value, which is what the JDK's {@code Integer.hashCode} answers. */
    @Override
    public int hashCode() {
        return this.value;
    }

    /** Whether {@code other} is an {@code Integer} wrapping the same value. */
    @Override
    public boolean equals(Object other) {
        if (!(other instanceof Integer)) {
            return false;
        }
        return ((Integer) other).value == this.value;
    }

    @Override
    public int compareTo(Integer other) {
        return compare(this.value, other.value);
    }

    /** The wrapped value in decimal. */
    @Override
    public String toString() {
        return toString(this.value);
    }

    /** {@code value} in decimal. */
    public static String toString(int value) {
        return toString(value, 10);
    }

    /**
     * {@code value} in {@code radix}, or in decimal when {@code radix} is outside 2..36.
     *
     * <p>The digits come off the *negative* side of {@code value}: {@code -Integer.MIN_VALUE} does
     * not fit an {@code int}, so negating first would be wrong for exactly one input, and it is
     * the input a test never has and a program eventually does.
     */
    public static String toString(int value, int radix) {
        int base = radix;
        if (base < 2 || base > 36) {
            base = 10;
        }
        if (value == 0) {
            return String.valueOf('0');
        }
        char[] digits = new char[33];
        int at = digits.length;
        boolean negative = value < 0;
        int rest = value;
        if (!negative) {
            rest = -value;
        }
        while (rest != 0) {
            at = at - 1;
            digits[at] = DIGITS[-(rest % base)];
            rest = rest / base;
        }
        if (negative) {
            at = at - 1;
            digits[at] = '-';
        }
        return new String(digits, at, digits.length - at);
    }

    /** {@code value} in base two, with no sign and no leading zeros. */
    public static String toBinaryString(int value) {
        return toUnsignedString(value, 1);
    }

    /** {@code value} in base eight, with no sign and no leading zeros. */
    public static String toOctalString(int value) {
        return toUnsignedString(value, 3);
    }

    /** {@code value} in base sixteen, with no sign and no leading zeros. */
    public static String toHexString(int value) {
        return toUnsignedString(value, 4);
    }

    /** The {@code int} {@code text} spells in decimal. */
    public static int parseInt(String text) {
        return parseInt(text, 10);
    }

    /**
     * The {@code int} {@code text} spells in {@code radix}.
     *
     * <p>Accumulated on the negative side for the reason {@link #toString(int, int)} renders from
     * it: {@code "-2147483648"} is a valid {@code int} whose positive form is not.
     */
    public static int parseInt(String text, int radix) {
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
        int limit = negative ? MIN_VALUE : -MAX_VALUE;
        int cutoff = limit / radix;
        int total = 0;
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
    public static int compare(int left, int right) {
        if (left < right) {
            return -1;
        }
        if (left > right) {
            return 1;
        }
        return 0;
    }

    /** The larger of the two. */
    public static int max(int left, int right) {
        return left > right ? left : right;
    }

    /** The smaller of the two. */
    public static int min(int left, int right) {
        return left < right ? left : right;
    }

    /** {@code left + right}, named so it can be passed where a method is wanted. */
    public static int sum(int left, int right) {
        return left + right;
    }

    /** {@code -1}, {@code 0}, or {@code 1} as {@code value} is negative, zero, or positive. */
    public static int signum(int value) {
        return (value >> 31) | (-value >>> 31);
    }

    /** How many bits of {@code value} are one. */
    public static int bitCount(int value) {
        int rest = value;
        int count = 0;
        while (rest != 0) {
            rest = rest & (rest - 1);
            count = count + 1;
        }
        return count;
    }

    /** How many zero bits precede the highest one bit, or 32 when there is none. */
    public static int numberOfLeadingZeros(int value) {
        if (value == 0) {
            return 32;
        }
        int rest = value;
        int count = 0;
        while (rest > 0) {
            rest = rest << 1;
            count = count + 1;
        }
        return count;
    }

    /** How many zero bits follow the lowest one bit, or 32 when there is none. */
    public static int numberOfTrailingZeros(int value) {
        if (value == 0) {
            return 32;
        }
        int rest = value;
        int count = 0;
        while ((rest & 1) == 0) {
            rest = rest >>> 1;
            count = count + 1;
        }
        return count;
    }

    /** {@code value} with every bit but the highest one cleared. */
    public static int highestOneBit(int value) {
        if (value == 0) {
            return 0;
        }
        return 1 << (31 - numberOfLeadingZeros(value));
    }

    /** {@code value} with every bit but the lowest one cleared. */
    public static int lowestOneBit(int value) {
        return value & -value;
    }

    /** {@code value} with its bits rotated left by {@code distance}. */
    public static int rotateLeft(int value, int distance) {
        return (value << distance) | (value >>> -distance);
    }

    /** {@code value} with its bits rotated right by {@code distance}. */
    public static int rotateRight(int value, int distance) {
        return (value >>> distance) | (value << -distance);
    }

    /** {@code value} with its bits in the opposite order. */
    public static int reverse(int value) {
        int reversed = 0;
        int rest = value;
        int at = 0;
        while (at < 32) {
            reversed = (reversed << 1) | (rest & 1);
            rest = rest >>> 1;
            at = at + 1;
        }
        return reversed;
    }

    /** {@code value} with its bytes in the opposite order. */
    public static int reverseBytes(int value) {
        return (value >>> 24)
                | ((value >> 8) & 0x0000FF00)
                | ((value << 8) & 0x00FF0000)
                | (value << 24);
    }

    /** {@code value} read as unsigned, in a radix that is a power of two. */
    private static String toUnsignedString(int value, int shift) {
        if (value == 0) {
            return String.valueOf('0');
        }
        int mask = (1 << shift) - 1;
        char[] digits = new char[32];
        int at = digits.length;
        int rest = value;
        while (rest != 0) {
            at = at - 1;
            digits[at] = DIGITS[rest & mask];
            rest = rest >>> shift;
        }
        return new String(digits, at, digits.length - at);
    }
}
