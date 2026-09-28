package java.lang;

/**
 * The boxed {@code float}.
 *
 * <p>The {@code double}'s shape in half the width: the narrowing accessors cast, the widening ones
 * are exact, and {@code compare} keeps the JDK's order — {@code NaN} above every number and equal
 * to itself, {@code +0.0f} above {@code -0.0f}. The reciprocal trick is a {@code float} division
 * here, so it produces an infinity of the right width.
 */
public class Float extends Number implements Comparable {

    private float value;

    private Float(float value) {
        this.value = value;
    }

    public static Float valueOf(float f) {
        return new Float(f);
    }

    public float floatValue() {
        return this.value;
    }

    public double doubleValue() {
        return this.value;
    }

    public int intValue() {
        return (int) this.value;
    }

    public long longValue() {
        return (long) this.value;
    }

    public static int compare(float a, float b) {
        if (a < b) {
            return -1;
        }
        if (a > b) {
            return 1;
        }
        if (a == b) {
            if (a == 0.0f) {
                float reciprocal_a = 1.0f / a;
                float reciprocal_b = 1.0f / b;
                if (reciprocal_a > reciprocal_b) {
                    return 1;
                }
                if (reciprocal_a < reciprocal_b) {
                    return -1;
                }
            }
            return 0;
        }
        if (a != a) {
            return b != b ? 0 : 1;
        }
        return -1;
    }

    /** Whether {@code o} is a {@code Float} with the same bits, which is what {@code NaN} needs. */
    public boolean equals(Object o) {
        if (!(o instanceof Float)) {
            return false;
        }
        float other = ((Float) o).value;
        if (this.value == other) {
            if (this.value == 0.0f) {
                return 1.0f / this.value == 1.0f / other;
            }
            return true;
        }
        return this.value != this.value && other != other;
    }
}
