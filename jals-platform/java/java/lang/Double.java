package java.lang;

/**
 * The boxed {@code double}.
 *
 * <p>{@code compare} and {@code equals} are here in full, because the JDK's answers are not the
 * ones {@code <} and {@code ==} give: {@code NaN} is greater than every number and equal to itself,
 * and {@code +0.0} is greater than {@code -0.0}. Both facts follow from the sign of a zero, which
 * {@code 1.0 / 0.0} exposes as an infinity without needing the bit pattern.
 *
 * <p>{@code doubleToLongBits}, {@code hashCode} and {@code toString} are absent until there is a
 * way to see the bits or render the decimals. A hash that ignored {@code -0.0} and {@code NaN}
 * would put two values in the same bucket that {@code equals} calls different, and a decimal
 * renderer that printed the shortest text that reads back is a format this platform has not
 * chosen yet. The calls are refused by name until then, rather than answered with something almost
 * right.
 */
public class Double extends Number implements Comparable {

    private double value;

    private Double(double value) {
        this.value = value;
    }

    public static Double valueOf(double d) {
        return new Double(d);
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
}
