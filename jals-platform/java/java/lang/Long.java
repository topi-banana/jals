package java.lang;

/**
 * The boxed {@code long}.
 *
 * <p>The same shape as {@link Integer}: a private constructor behind {@code valueOf}, the widening
 * accessors, and a decimal {@code toString} built by the same {@link StringBuilder} overload that
 * {@code "" + l} resolves to. {@code parseLong} is the same decimal parser one width up — the
 * negative-range accumulation is what makes {@code Long.MIN_VALUE} parse — and it throws
 * {@link NumberFormatException} for the same reason {@code parseInt} does: a parser with no way to
 * report bad input would have to invent an answer.
 */
public class Long extends Number implements Comparable {

    private long value;

    private Long(long value) {
        this.value = value;
    }

    public static Long valueOf(long l) {
        return new Long(l);
    }

    /** The digit a code unit spells, or {@code -1} when it spells none. */
    private static int digitOf(char c) {
        int value = c - '0';
        if (value < 0 || value > 9) {
            return -1;
        }
        return value;
    }

    /**
     * The {@code long} that {@code s} spells in decimal.
     *
     * <p>The same algorithm as {@link Integer#parseInt}, at the width where the negative range is
     * the point: {@code -9223372036854775808} has no positive magnitude an {@code i64} can hold,
     * and the accumulation never builds one. The overflow check is two comparisons against the
     * limit the sign chooses.
     */
    public static long parseLong(String s) {
        if (s == null) {
            throw new NumberFormatException("Cannot parse null string");
        }
        int length = s.length();
        if (length == 0) {
            throw new NumberFormatException("For input string: \"" + s + "\"");
        }
        int index = 0;
        boolean negative = false;
        if (s.charAt(0) == '-') {
            if (length == 1) {
                throw new NumberFormatException("For input string: \"" + s + "\"");
            }
            negative = true;
            index = 1;
        }
        long limit = -9223372036854775807L;
        if (negative) {
            limit = -9223372036854775807L - 1;
        }
        long multmin = limit / 10;
        long result = 0;
        while (index < length) {
            int digit = digitOf(s.charAt(index));
            if (digit < 0 || result < multmin) {
                throw new NumberFormatException("For input string: \"" + s + "\"");
            }
            result = result * 10;
            if (result < limit + digit) {
                throw new NumberFormatException("For input string: \"" + s + "\"");
            }
            result = result - digit;
            index = index + 1;
        }
        if (negative) {
            return result;
        }
        return -result;
    }

    /** The narrowing conversion, exactly as a cast would do it. */
    public int intValue() {
        return (int) this.value;
    }

    public long longValue() {
        return this.value;
    }

    public float floatValue() {
        return this.value;
    }

    public double doubleValue() {
        return this.value;
    }

    public String toString() {
        return new StringBuilder().append(this.value).toString();
    }

    public boolean equals(Object o) {
        if (!(o instanceof Long)) {
            return false;
        }
        return ((Long) o).value == this.value;
    }

    /** The JDK's fold: the two halves of the value, exclusive-ored. */
    public int hashCode() {
        return (int) (this.value ^ (this.value >>> 32));
    }
}
