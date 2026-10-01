package java.lang;

/**
 * The boxed {@code int}.
 *
 * <p>{@code valueOf} is the way in — the constructor is private, so a caller gets the method the
 * JDK documents rather than the constructor it happens to have — and the {@code Number} accessors
 * widen: {@code longValue} is exact, and {@code floatValue} and {@code doubleValue} round to
 * nearest as the JLS says.
 *
 * <p>{@code parseInt} is the JDK's decimal parser: an optional minus sign, ASCII digits, and no
 * tolerance for anything else. It accumulates through the *negative* range, so
 * {@code Integer.MIN_VALUE} — which has no positive counterpart to negate — parses like any other
 * value, and it throws {@link NumberFormatException} rather than truncating or wrapping, which is
 * exactly what it was waiting for. {@code TYPE} is still absent for a reason of its own shape: this
 * platform has no {@code Class} value to put in it, and a field that could only ever hold
 * {@code null} is a promise no code can keep.
 */
public class Integer extends Number implements Comparable {

    private int value;

    private Integer(int value) {
        this.value = value;
    }

    /** The box, for the widening conversions a call site used to do implicitly. */
    public static Integer valueOf(int i) {
        return new Integer(i);
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
     * The {@code int} that {@code s} spells in decimal.
     *
     * <p>JDK semantics: an optional leading {@code -} — not {@code +} — one or more ASCII digits,
     * and a value that fits. Anything else, a null included, is a {@link NumberFormatException}.
     *
     * <p>The accumulation runs through the *negative* range, which is what lets
     * {@code -2147483648} parse: its magnitude has no {@code int} to be built in, so the parser
     * builds the value itself and negates only a result that is not negative. The limit is one
     * more for a negative result, and the two comparisons against it are the whole overflow check —
     * a ten-digit string never has to be compared as a number.
     */
    public static int parseInt(String s) {
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
        int limit = -2147483647;
        if (negative) {
            limit = -2147483647 - 1;
        }
        int multmin = limit / 10;
        int result = 0;
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

    public int intValue() {
        return this.value;
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

    /** The decimal text — the same one a {@code "" + i} builds, by the same builder. */
    public String toString() {
        return new StringBuilder().append(this.value).toString();
    }

    /** Whether {@code o} is an {@code Integer} holding the same {@code int}. */
    public boolean equals(Object o) {
        if (!(o instanceof Integer)) {
            return false;
        }
        return ((Integer) o).value == this.value;
    }

    /** The value itself, which is the JDK's definition. */
    public int hashCode() {
        return this.value;
    }
}
