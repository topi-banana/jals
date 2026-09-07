package java.lang;

/**
 * The arithmetic that has nowhere else to live.
 *
 * <p>Everything below is exact integer or bit work, plus {@link #sqrt} — and what is *not* below
 * is the transcendental half of the JDK's {@code Math}: {@code sin}, {@code cos}, {@code tan},
 * {@code exp}, {@code log}, {@code pow}. Those need a rounded elementary-function library, this
 * crate is dependency-free by design (a package author's crate depends on it and on nothing else),
 * and a hand-rolled series would be a wrong answer that looks like a right one. They are absent
 * rather than approximate, which is the same choice {@link Character} makes about Unicode.
 *
 * <p>{@link #sqrt} is here because it is the one of them a program cannot write around, and because
 * it can be computed exactly rather than approximated — see its own documentation for how, and for
 * the one input in the whole {@code double} range on which it and the JDK disagree.
 */
public final class Math {

    /** The ratio of a circle's circumference to its diameter, to the nearest {@code double}. */
    public static final double PI = 3.141592653589793;

    /** The base of the natural logarithms, to the nearest {@code double}. */
    public static final double E = 2.718281828459045;

    /** Beyond this magnitude every {@code double} is already an integer. */
    private static final double INTEGRAL_ABOVE = 9.007199254740992E15;

    /** How many Newton passes {@link #sqrt} takes over a mantissa in {@code [1, 4)}. */
    private static final int SQRT_PASSES = 5;

    /** {@code 2^27 + 1}, the constant Dekker's splitting multiplies by. */
    private static final double SPLIT = 134217729.0;

    /** {@code 2^54}, which lifts a subnormal into the normal range without losing a bit. */
    private static final double SUBNORMAL_LIFT = 18014398509481984.0;

    /** Never called. */
    private Math() {
    }

    /** The larger of the two. */
    public static int max(int left, int right) {
        return left > right ? left : right;
    }

    /** The larger of the two. */
    public static long max(long left, long right) {
        return left > right ? left : right;
    }

    /** The larger of the two, answering {@code NaN} when either is. */
    public static float max(float left, float right) {
        return (float) max((double) left, (double) right);
    }

    /**
     * The larger of the two, answering {@code NaN} when either is and {@code 0.0} over
     * {@code -0.0}.
     */
    public static double max(double left, double right) {
        if (left != left || right != right) {
            return Double.NaN;
        }
        if (left == right) {
            return Double.doubleToRawLongBits(left) < 0L ? right : left;
        }
        return left > right ? left : right;
    }

    /** The smaller of the two. */
    public static int min(int left, int right) {
        return left < right ? left : right;
    }

    /** The smaller of the two. */
    public static long min(long left, long right) {
        return left < right ? left : right;
    }

    /** The smaller of the two, answering {@code NaN} when either is. */
    public static float min(float left, float right) {
        return (float) min((double) left, (double) right);
    }

    /**
     * The smaller of the two, answering {@code NaN} when either is and {@code -0.0} over
     * {@code 0.0}.
     */
    public static double min(double left, double right) {
        if (left != left || right != right) {
            return Double.NaN;
        }
        if (left == right) {
            return Double.doubleToRawLongBits(left) < 0L ? left : right;
        }
        return left < right ? left : right;
    }

    /**
     * The magnitude of {@code value}.
     *
     * <p>{@code abs(Integer.MIN_VALUE)} is {@code Integer.MIN_VALUE}, which is the JDK's answer
     * and not a bug in either: the positive value does not fit an {@code int}.
     */
    public static int abs(int value) {
        return value < 0 ? -value : value;
    }

    /** The magnitude of {@code value}, with {@code Long.MIN_VALUE} answering itself. */
    public static long abs(long value) {
        return value < 0L ? -value : value;
    }

    /** The magnitude of {@code value}. */
    public static float abs(float value) {
        return Float.intBitsToFloat(Float.floatToRawIntBits(value) & 0x7FFFFFFF);
    }

    /** The magnitude of {@code value}. */
    public static double abs(double value) {
        return Double.longBitsToDouble(
                Double.doubleToRawLongBits(value) & 0x7FFFFFFFFFFFFFFFL);
    }

    /** {@code -1.0}, {@code 0.0}, or {@code 1.0} as {@code value} is negative, zero, or positive. */
    public static double signum(double value) {
        if (value != value || value == 0.0) {
            return value;
        }
        return value < 0.0 ? -1.0 : 1.0;
    }

    /** {@code -1.0f}, {@code 0.0f}, or {@code 1.0f} as {@code value} is negative, zero, or positive. */
    public static float signum(float value) {
        return (float) signum((double) value);
    }

    /** The largest integral {@code double} at or below {@code value}. */
    public static double floor(double value) {
        if (value != value || abs(value) >= INTEGRAL_ABOVE) {
            return value;
        }
        double truncated = (long) value;
        if (truncated > value) {
            return truncated - 1.0;
        }
        return truncated;
    }

    /** The smallest integral {@code double} at or above {@code value}. */
    public static double ceil(double value) {
        if (value != value || abs(value) >= INTEGRAL_ABOVE) {
            return value;
        }
        double truncated = (long) value;
        if (truncated < value) {
            return truncated + 1.0;
        }
        return truncated;
    }

    /** {@code value} rounded to the nearest {@code long}, halves going up. */
    public static long round(double value) {
        if (value != value) {
            return 0L;
        }
        return (long) floor(value + 0.5);
    }

    /** {@code value} rounded to the nearest {@code int}, halves going up. */
    public static int round(float value) {
        if (value != value) {
            return 0;
        }
        return (int) floor(value + 0.5);
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
        if ((left ^ right) < 0L && quotient * right != left) {
            return quotient - 1L;
        }
        return quotient;
    }

    /** The remainder that goes with {@link #floorDiv}, so its sign follows {@code right}. */
    public static int floorMod(int left, int right) {
        return left - floorDiv(left, right) * right;
    }

    /** The remainder that goes with {@link #floorDiv}, so its sign follows {@code right}. */
    public static long floorMod(long left, long right) {
        return left - floorDiv(left, right) * right;
    }

    /** {@code value} as an {@code int}, refusing rather than truncating when it does not fit. */
    public static int toIntExact(long value) {
        int narrowed = (int) value;
        if (narrowed != value) {
            throw new ArithmeticException();
        }
        return narrowed;
    }

    /** {@code left + right}, refusing rather than wrapping on overflow. */
    public static int addExact(int left, int right) {
        int total = left + right;
        if (((left ^ total) & (right ^ total)) < 0) {
            throw new ArithmeticException();
        }
        return total;
    }

    /** {@code left - right}, refusing rather than wrapping on overflow. */
    public static int subtractExact(int left, int right) {
        int difference = left - right;
        if (((left ^ right) & (left ^ difference)) < 0) {
            throw new ArithmeticException();
        }
        return difference;
    }

    /** {@code left * right}, refusing rather than wrapping on overflow. */
    public static int multiplyExact(int left, int right) {
        long product = (long) left * (long) right;
        int narrowed = (int) product;
        if (narrowed != product) {
            throw new ArithmeticException();
        }
        return narrowed;
    }

    /**
     * The non-negative square root of {@code value}.
     *
     * <p>Three steps, and each closes a gap the one before it leaves.
     *
     * <ol>
     *   <li><b>Reduce.</b> {@code value} is split into a mantissa in {@code [1, 4)} and an
     *       <em>even</em> power of two, so the root is the mantissa's root times half that power —
     *       a scaling that is exact, because it is a power of two. Doing the arithmetic in
     *       {@code [1, 4)} is what makes step three safe: Dekker's splitting multiplies by
     *       {@code 2^27 + 1}, which would overflow on a number near {@link Double#MAX_VALUE}.
     *   <li><b>Iterate.</b> The seed comes from halving the exponent field — adding the bias, then
     *       shifting the whole pattern right by one — which lands within a few percent, and
     *       Newton's iteration doubles the correct digits each pass.
     *   <li><b>Correct.</b> Newton in binary64 settles about one unit in the last place away, and
     *       which side it settles on is not predictable. So the residual {@code m - g*g} is
     *       computed <em>exactly</em> — {@link #productError} recovers the bits the multiplication
     *       rounded away — and {@code residual / (2g)} is the correction that closes it.
     * </ol>
     *
     * <p>The result matches the JDK's on every value tried but one: {@link Double#MAX_VALUE},
     * whose true root misses a rounding boundary by about {@code 2^-104} — far below what the
     * exact-residual comparison in step three can see. A program that depends on that bit should
     * say so.
     */
    public static double sqrt(double value) {
        if (value != value || value < 0.0) {
            return Double.NaN;
        }
        if (value == 0.0 || value == Double.POSITIVE_INFINITY) {
            return value;
        }
        int halfExponent = 0;
        double mantissa = value;
        if (mantissa < Double.MIN_NORMAL) {
            mantissa = mantissa * SUBNORMAL_LIFT;
            halfExponent = halfExponent - 27;
        }
        long bits = Double.doubleToRawLongBits(mantissa);
        int exponent = (int) ((bits >> 52) - 1023L);
        int even = exponent % 2 == 0 ? exponent : exponent - 1;
        halfExponent = halfExponent + even / 2;
        mantissa = Double.longBitsToDouble(bits - ((long) even << 52));

        double guess =
                Double.longBitsToDouble(
                        (Double.doubleToRawLongBits(mantissa) + 4607182418800017408L) >> 1);
        int pass = 0;
        while (pass < SQRT_PASSES) {
            guess = 0.5 * (guess + mantissa / guess);
            pass = pass + 1;
        }
        double product = guess * guess;
        double residual = (mantissa - product) - productError(guess, product);
        guess = guess + residual / (2.0 * guess);

        return guess * Double.longBitsToDouble((long) (1023 + halfExponent) << 52);
    }

    /**
     * The bits {@code value * value} rounded away, so that {@code product + productError} is the
     * exact square.
     *
     * <p>Dekker's splitting: {@code value} is cut into a high half with 26 significant bits and a
     * low remainder, and each of the three cross products is then exact in binary64. It is only
     * valid where {@code SPLIT * value} does not overflow, which is why {@link #sqrt} reduces its
     * argument into {@code [1, 4)} before calling it.
     */
    private static double productError(double value, double product) {
        double scaled = SPLIT * value;
        double high = scaled - (scaled - value);
        double low = value - high;
        return ((high * high - product) + 2.0 * high * low) + low * low;
    }

    /** The square root of {@code left * left + right * right}, without an intermediate overflow. */
    public static double hypot(double left, double right) {
        double a = abs(left);
        double b = abs(right);
        if (a < b) {
            double held = a;
            a = b;
            b = held;
        }
        if (a == 0.0) {
            return 0.0;
        }
        double ratio = b / a;
        return a * sqrt(1.0 + ratio * ratio);
    }
}
