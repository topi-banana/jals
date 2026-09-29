package java.lang;

/**
 * The boxed {@code double}.
 *
 * <p>{@code compare} and {@code equals} are here in full, because the JDK's answers are not the
 * ones {@code <} and {@code ==} give: {@code NaN} is greater than every number and equal to itself,
 * and {@code +0.0} is greater than {@code -0.0}. Both facts follow from the sign of a zero, which
 * {@code 1.0 / 0.0} exposes as an infinity without needing the bit pattern.
 *
 * <p>{@code toString} is here in full, and its digits are the one algorithmic thing this platform
 * asks its host for: almost always the shortest decimal that reads back as the same {@code double},
 * and among those the nearest — which needs exact arithmetic over the significand, a big-integer
 * core in Java's own terms, rather than anything a loop over {@code double}s can decide. Everything
 * the JDK's notation adds to those digits, and every special value, is this class's. {@code
 * doubleToLongBits} and {@code hashCode} remain absent until there is a way to see the bits: a hash
 * that ignored {@code -0.0} and {@code NaN} would put two values in the same bucket that
 * {@code equals} calls different.
 */
public class Double extends Number implements Comparable {

    private double value;

    private Double(double value) {
        this.value = value;
    }

    public static Double valueOf(double d) {
        return new Double(d);
    }

    /**
     * The JDK's rendering of {@code d}: almost always the shortest decimal that reads back as the
     * same {@code double}, and among those the nearest, in the JDK's notation.
     */
    public static String toString(double d) {
        if (d != d) {
            return "NaN";
        }
        if (d - d != 0.0) {
            // An infinity is the one value finite arithmetic cannot make a zero out of: `i - i`
            // is NaN there, while every finite value subtracts to zero.
            if (d > 0.0) {
                return "Infinity";
            }
            return "-Infinity";
        }
        if (d == 0.0) {
            // The sign of a zero, which `==` cannot see, through the sign of its reciprocal.
            if (1.0 / d > 0.0) {
                return "0.0";
            }
            return "-0.0";
        }
        boolean negative = d < 0.0;
        double magnitude = d;
        if (negative) {
            magnitude = -d;
        }
        char[] digits = new char[32];
        int[] point = new int[1];
        int length = writeDigits(magnitude, digits, point);
        return FloatingDecimal.toJavaFormatString(negative, digits, length, point[0]);
    }

    /** The JDK's rendering of this value. */
    public String toString() {
        return toString(this.value);
    }

    public double doubleValue() {
        return this.value;
    }

    public float floatValue() {
        return (float) this.value;
    }

    public int intValue() {
        return (int) this.value;
    }

    public long longValue() {
        return (long) this.value;
    }

    /** The JDK's total order: numbers, then {@code +0.0} above {@code -0.0}, then {@code NaN}. */
    public static int compare(double a, double b) {
        if (a < b) {
            return -1;
        }
        if (a > b) {
            return 1;
        }
        if (a == b) {
            if (a == 0.0) {
                // Same zero, possibly different signs. The reciprocal of `+0.0` is `+infinity` and
                // of `-0.0` is `-infinity`, so the comparison is the sign test the bits would give.
                double reciprocal_a = 1.0 / a;
                double reciprocal_b = 1.0 / b;
                if (reciprocal_a > reciprocal_b) {
                    return 1;
                }
                if (reciprocal_a < reciprocal_b) {
                    return -1;
                }
            }
            return 0;
        }
        // Neither `==` nor the orderings answered: at least one side is `NaN`.
        if (a != a) {
            return b != b ? 0 : 1;
        }
        return -1;
    }

    /** Whether {@code o} is a {@code Double} with the same bits, which is what {@code NaN} needs. */
    public boolean equals(Object o) {
        if (!(o instanceof Double)) {
            return false;
        }
        double other = ((Double) o).value;
        if (this.value == other) {
            if (this.value == 0.0) {
                return 1.0 / this.value == 1.0 / other;
            }
            return true;
        }
        return this.value != this.value && other != other;
    }

    /**
     * Write the significant digits of {@code value} into {@code out}, the power of ten of the first
     * one into {@code point[0]}, and return how many digits there are.
     *
     * <p>The one thing this class cannot compute by itself, and the reason it is a binding rather
     * than a loop: the digits are almost always the shortest that read back as {@code value}, and
     * among those the nearest, which takes exact arithmetic over the significand. The values that
     * are not digits — the zeros, the infinities, the NaN — never reach here;
     * {@link #toString(double)} answers those first, and the host refuses one that somehow does
     * anyway.
     */
    private static native int writeDigits(double value, char[] out, int[] point);
}
