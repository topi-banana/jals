package java.lang;

/**
 * Arithmetic Java's operators do not spell.
 *
 * <p>Nothing here is {@code native}. Every function is written on top of the operators and of
 * {@link Double}'s bit casts, which is possible because none of them is transcendental: this
 * package deliberately carries no {@code sin}, {@code exp} or {@code log}. Those need either a
 * polynomial table this package would have to be trusted about, or a host binding each. A square
 * root is the one worth doing by hand: it is exact, and every distance calculation reaches for it.
 *
 * <p>{@link #sqrt} agrees with the JDK on every input tried. {@link #hypot} does not, and that is
 * stated rather than hidden: it rounds three times where the JDK rounds once, so it is within about
 * two units in the last place rather than the one the JDK's javadoc promises. The alternative is a
 * claim of exactness this package cannot make.
 */
public final class Math {

    /** The ratio of a circle's circumference to its diameter. */
    public static final double PI = 3.141592653589793;

    /** The base of the natural logarithms. */
    public static final double E = 2.718281828459045;

    /** {@code 2^53}: above this every {@code double} is already an integer. */
    private static final double INTEGRAL_ABOVE = 9.007199254740992E15;

    /** How many Newton passes {@link #sqrt} takes; five is exact at {@code double} width. */
    private static final int SQRT_PASSES = 5;

    /** {@code 2^27 + 1}, the constant Dekker's splitting multiplies by. */
    private static final double SPLIT = 134217729.0;

    /** {@code 2^54}, which lifts a subnormal into the normal range before reduction. */
    private static final double SUBNORMAL_LIFT = 1.8014398509481984E16;

    private Math() {}

    /** The absolute value of {@code value}. */
    public static int abs(int value) {
        return value < 0 ? -value : value;
    }

    /** The absolute value of {@code value}. */
    public static long abs(long value) {
        return value < 0 ? -value : value;
    }

    /**
     * The absolute value of {@code value}.
     *
     * <p>A subtraction from zero rather than a negation, so {@code -0.0} answers {@code 0.0}: the
     * comparison has to admit {@code -0.0} first, which {@code <= 0.0} does and {@code < 0.0} does
     * not.
     */
    public static double abs(double value) {
        return value <= 0.0 ? 0.0 - value : value;
    }

    /** The absolute value of {@code value}. */
    public static float abs(float value) {
        return value <= 0.0f ? 0.0f - value : value;
    }

    /** The larger of two values. */
    public static int max(int left, int right) {
        return left >= right ? left : right;
    }

    /** The larger of two values. */
    public static long max(long left, long right) {
        return left >= right ? left : right;
    }

    /** The larger of two values, under {@link Double#compare}'s order. */
    public static double max(double left, double right) {
        return Double.max(left, right);
    }

    /** The larger of two values, under {@link Float#compare}'s order. */
    public static float max(float left, float right) {
        return Float.max(left, right);
    }

    /** The smaller of two values. */
    public static int min(int left, int right) {
        return left <= right ? left : right;
    }

    /** The smaller of two values. */
    public static long min(long left, long right) {
        return left <= right ? left : right;
    }

    /** The smaller of two values, under {@link Double#compare}'s order. */
    public static double min(double left, double right) {
        return Double.min(left, right);
    }

    /** The smaller of two values, under {@link Float#compare}'s order. */
    public static float min(float left, float right) {
        return Float.min(left, right);
    }

    /** {@code value}'s sign as {@code -1}, {@code 0} or {@code 1}; NaN yields NaN. */
    public static double signum(double value) {
        if (Double.isNaN(value) || value == 0.0) {
            return value;
        }
        return value > 0.0 ? 1.0 : -1.0;
    }

    /**
     * The largest integral value not above {@code value}.
     *
     * <p>{@code ±0.0} is returned as written, sign and all. The {@code long} round trip below
     * cannot carry a sign onto a zero — {@code (double) (long) -0.0} is {@code 0.0} — and
     * {@code floor(-0.0)} is {@code -0.0}, so the zero is answered before the round trip rather than
     * repaired after it.
     */
    public static double floor(double value) {
        if (Double.isNaN(value) || Double.isInfinite(value) || abs(value) >= INTEGRAL_ABOVE) {
            return value;
        }
        if (value == 0.0) {
            return value;
        }
        double truncated = (double) (long) value;
        return truncated > value ? truncated - 1.0 : truncated;
    }

    /**
     * The smallest integral value not below {@code value}.
     *
     * <p>Two zeroes, not one. {@code ceil} of anything in {@code (-1.0, 0.0)} is {@code -0.0} and
     * not {@code 0.0}: the answer is a zero the argument's own sign reaches, which is why the
     * negative branch is spelled rather than left to the round trip, which loses it.
     */
    public static double ceil(double value) {
        if (Double.isNaN(value) || Double.isInfinite(value) || abs(value) >= INTEGRAL_ABOVE) {
            return value;
        }
        if (value == 0.0) {
            return value;
        }
        double truncated = (double) (long) value;
        double raised = truncated < value ? truncated + 1.0 : truncated;
        return raised == 0.0 && value < 0.0 ? -0.0 : raised;
    }

    /**
     * {@code value} rounded to the nearest {@code long}, halves rounding towards positive infinity.
     *
     * <p>Deliberately <em>not</em> {@code (long) floor(value + 0.5)}. That expression rounds twice
     * — once in the addition and once in the floor — and the first rounding can carry a
     * value across the halfway point that was below it: {@code 0.49999999999999994 + 0.5} is
     * exactly {@code 1.0}, so the naive form answers {@code 1} where the nearest {@code long} is
     * {@code 0}. It is the bug the JDK carried until Java 7 (JDK-6430675). Comparing the fraction
     * against a half instead adds nothing, so nothing rounds but the floor.
     */
    public static long round(double value) {
        if (Double.isNaN(value)) {
            return 0L;
        }
        double floor = floor(value);
        return (long) (value - floor >= 0.5 ? floor + 1.0 : floor);
    }

    /**
     * {@code value} rounded to the nearest {@code int}, halves rounding towards positive infinity.
     *
     * <p>Widened first, which is exact, and then rounded once — see {@link #round(double)} for
     * why the halfway comparison is not an addition.
     */
    public static int round(float value) {
        if (Float.isNaN(value)) {
            return 0;
        }
        double widened = (double) value;
        double floor = floor(widened);
        return (int) (widened - floor >= 0.5 ? floor + 1.0 : floor);
    }

    /** The remainder of {@code left / right} with the sign of {@code right}. */
    public static int floorMod(int left, int right) {
        int remainder = left % right;
        if (remainder != 0 && (remainder ^ right) < 0) {
            return remainder + right;
        }
        return remainder;
    }

    /** The remainder of {@code left / right} with the sign of {@code right}. */
    public static long floorMod(long left, long right) {
        long remainder = left % right;
        if (remainder != 0 && (remainder ^ right) < 0) {
            return remainder + right;
        }
        return remainder;
    }

    /** {@code left / right}, rounded towards negative infinity. */
    public static int floorDiv(int left, int right) {
        int quotient = left / right;
        if ((left ^ right) < 0 && quotient * right != left) {
            return quotient - 1;
        }
        return quotient;
    }

    /** {@code left / right}, rounded towards negative infinity. */
    public static long floorDiv(long left, long right) {
        long quotient = left / right;
        if ((left ^ right) < 0 && quotient * right != left) {
            return quotient - 1;
        }
        return quotient;
    }

    /**
     * The non-negative square root of {@code value}.
     *
     * <p>Three steps. The argument is reduced into {@code [1, 4)} by stripping an even power of two
     * from its exponent, which keeps Newton's iteration in the range where it converges fastest.
     * Five passes from a seed built by halving the exponent bits then land within one unit in the
     * last place. The last step is what makes it exact: Dekker's splitting computes the residual
     * {@code value - guess*guess} without rounding it away, and one correction term from that
     * residual moves the guess to the correctly rounded result.
     */
    public static double sqrt(double value) {
        if (Double.isNaN(value) || value < 0.0) {
            return Double.NaN;
        }
        if (value == 0.0 || Double.isInfinite(value)) {
            return value;
        }
        double lifted = value;
        int scale = 0;
        if (lifted < Double.MIN_VALUE * SUBNORMAL_LIFT) {
            lifted = lifted * SUBNORMAL_LIFT;
            scale = -27;
        }
        long bits = Double.doubleToRawLongBits(lifted);
        int exponent = (int) ((bits >>> 52) & 0x7FF) - 1023;
        int even = exponent - (exponent & 1);
        scale = scale + (even >> 1);
        double mantissa = Double.longBitsToDouble(bits - ((long) even << 52));
        double guess = seed(mantissa);
        for (int pass = 0; pass < SQRT_PASSES; pass++) {
            guess = 0.5 * (guess + mantissa / guess);
        }
        guess = guess + productError(guess, mantissa) / (2.0 * guess);
        // The reduction put {@code mantissa} in [1, 4), so its root is in [1, 2) and the correctly
        // rounded answer is never 2.0 itself. The correction step can still land there: for the
        // largest double of an odd binade the true root sits within half an ulp of the boundary,
        // where the ulp doubles, and rounding across it returns a value whose square exceeds the
        // argument. The answer there is always the largest double below the boundary.
        if (guess >= 2.0) {
            guess = Double.longBitsToDouble(Double.doubleToRawLongBits(2.0) - 1L);
        }
        return scaleByPowerOfTwo(guess, scale);
    }

    /** The length of the hypotenuse of a right triangle with the given sides. */
    public static double hypot(double left, double right) {
        double a = abs(left);
        double b = abs(right);
        if (Double.isInfinite(a) || Double.isInfinite(b)) {
            return Double.POSITIVE_INFINITY;
        }
        if (a == 0.0) {
            return b;
        }
        if (b == 0.0) {
            return a;
        }
        double larger = a >= b ? a : b;
        double smaller = a >= b ? b : a;
        double ratio = smaller / larger;
        return larger * sqrt(1.0 + ratio * ratio);
    }

    /** A first approximation of {@code sqrt(mantissa)} from half its exponent. */
    private static double seed(double mantissa) {
        long bits = Double.doubleToRawLongBits(mantissa);
        long halved = ((bits >>> 52) & 0x7FF) - 1023;
        long seeded = ((halved >> 1) + 1023) << 52;
        return Double.longBitsToDouble(seeded | (bits & 0x000FFFFFFFFFFFFFL) >>> 1);
    }

    /**
     * {@code target - guess*guess}, computed exactly.
     *
     * <p>Dekker's splitting: each operand is broken into a high half with 26 significant bits and a
     * low remainder, so the four partial products are each exact and their sum is the product with
     * no rounding at all. Subtracting that from {@code target} recovers a residual the naive
     * expression would have thrown away.
     */
    private static double productError(double guess, double target) {
        double split = guess * SPLIT;
        double high = split - (split - guess);
        double low = guess - high;
        double square = guess * guess;
        double error = ((high * high - square) + 2.0 * high * low) + low * low;
        return (target - square) - error;
    }

    /** {@code value} multiplied by {@code 2^exponent}, in at most two exact steps. */
    private static double scaleByPowerOfTwo(double value, int exponent) {
        if (exponent == 0) {
            return value;
        }
        int half = exponent / 2;
        double first = Double.longBitsToDouble((long) (half + 1023) << 52);
        double second = Double.longBitsToDouble((long) (exponent - half + 1023) << 52);
        return value * first * second;
    }
}
