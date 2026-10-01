package java.lang;

/**
 * The boxed {@code float}.
 *
 * <p>The {@code double}'s shape in half the width: the narrowing accessors cast, the widening ones
 * are exact, and {@code compare} keeps the JDK's order — {@code NaN} above every number and equal
 * to itself, {@code +0.0f} above {@code -0.0f}. The reciprocal trick is a {@code float} division
 * here, so it produces an infinity of the right width.
 *
 * <p>{@code toString} shares the {@code double}'s shape — the special values here, the notation in
 * {@link FloatingDecimal} — but not its digits: the shortest decimal that reads back as a
 * {@code float} is a property of the width, so {@code 0.1f} prints {@code 0.1} while the
 * {@code double} nearest to it prints {@code 0.10000000149011612}.
 */
public class Float extends Number implements Comparable {

    private float value;

    private Float(float value) {
        this.value = value;
    }

    public static Float valueOf(float f) {
        return new Float(f);
    }

    /**
     * The JDK's rendering of {@code f}: almost always the shortest decimal that reads back as the
     * same {@code float}, and among those the nearest, in the JDK's notation.
     */
    public static String toString(float f) {
        if (f != f) {
            return "NaN";
        }
        if (f - f != 0.0f) {
            if (f > 0.0f) {
                return "Infinity";
            }
            return "-Infinity";
        }
        if (f == 0.0f) {
            if (1.0f / f > 0.0f) {
                return "0.0";
            }
            return "-0.0";
        }
        boolean negative = f < 0.0f;
        float magnitude = f;
        if (negative) {
            magnitude = -f;
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

    /** See {@link Double#toString(double)}'s binding, which this is the half-width twin of. */
    private static native int writeDigits(float value, char[] out, int[] point);
}
