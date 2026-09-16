package java.lang;

/**
 * The {@code double} wrapper, and the four operations on one that Java cannot express.
 *
 * <p>The {@code native} methods here are the whole reason this package has a Rust half. A bit cast
 * between a {@code double} and a {@code long} is not something Java arithmetic can do, and the
 * shortest decimal that round-trips to a given {@code double} is a well-known hard problem whose
 * answer Rust's own formatter already has. Everything else in this class is written in Java on top
 * of those four.
 *
 * <p>{@link #toChars} writes into an array <em>this</em> class allocates, because a host binding
 * cannot allocate a Java object: a wasm embedder has no {@code struct.new} of its own. It returns
 * how many characters it wrote.
 */
public final class Double extends Number implements Comparable<Double> {

    /** {@code "double"}, as a {@code char[]}. */
    private static final char[] PRIMITIVE_NAME = {'d', 'o', 'u', 'b', 'l', 'e'};

    /** The {@code Class} standing for the primitive type {@code double}. */
    public static final Class<Double> TYPE = new Class<Double>(new String(PRIMITIVE_NAME));

    /** The largest finite {@code double}. */
    public static final double MAX_VALUE = 1.7976931348623157E308;

    /** The smallest positive non-zero {@code double}. */
    public static final double MIN_VALUE = 4.9E-324;

    /** Positive infinity. */
    public static final double POSITIVE_INFINITY = 1.0 / 0.0;

    /** Negative infinity. */
    public static final double NEGATIVE_INFINITY = -1.0 / 0.0;

    /** The canonical not-a-number value. */
    // `NaN`, `out` and `err` are names the JDK fixed; a program spells them as written or
    // it does not compile against a real one.
    @SuppressWarnings("naming-convention")
    public static final double NaN = 0.0 / 0.0;

    /** The one bit pattern {@link #doubleToLongBits} answers for every NaN. */
    private static final long CANONICAL_NAN_BITS = 0x7ff8000000000000L;

    /** How many bits a {@code double} occupies. */
    public static final int SIZE = 64;

    /** How many bytes a {@code double} occupies. */
    public static final int BYTES = 8;

    /**
     * How many characters a rendering can take.
     *
     * <p>The host refuses to write past this rather than truncating: a truncated number is a wrong
     * number, and a wrong number that looks right is worse than a refusal.
     */
    private static final int RENDERING_LIMIT = 32;

    private final double value;

    public Double(double value) {
        this.value = value;
    }

    /** A wrapper holding {@code value}. */
    // `valueOf` is where the allocation is: this class *is* the wrapper, so the constructor it
    // would be told to call instead is this method.
    @SuppressWarnings("boxed-primitive-constructor")
    public static Double valueOf(double value) {
        return new Double(value);
    }

    @Override
    public int intValue() {
        return (int) this.value;
    }

    @Override
    public long longValue() {
        return (long) this.value;
    }

    @Override
    public float floatValue() {
        return (float) this.value;
    }

    @Override
    public double doubleValue() {
        return this.value;
    }

    /** {@code value}'s IEEE 754 bits, not collapsing a signalling NaN. */
    public static native long doubleToRawLongBits(double value);

    /**
     * {@code value}'s IEEE 754 bits, with every NaN collapsed to one pattern.
     *
     * <p>Java in this package rather than a thirteenth binding, because it is a {@code double}
     * comparison and a constant — the kind of thing this package does in Java by rule. It is the
     * form {@link #compare}, {@link #equals} and {@link #hashCode} are specified in terms of: a
     * NaN carries a payload and a sign that arithmetic is free to choose, so reading the raw bits
     * there makes two NaNs unequal and differently hashed for a reason no program wrote down.
     */
    public static long doubleToLongBits(double value) {
        if (isNaN(value)) {
            return CANONICAL_NAN_BITS;
        }
        return doubleToRawLongBits(value);
    }

    /** The {@code double} whose IEEE 754 bits are {@code bits}. */
    public static native double longBitsToDouble(long bits);

    /** Render {@code value} into {@code out}; how many characters were written. */
    private static native int toChars(double value, char[] out);

    /**
     * Decode {@code count} characters of {@code text} at {@code offset} into {@code out[0]}.
     *
     * <p>Returns whether they spelled a {@code double}, rather than refusing when they did not. A
     * binding that refuses becomes a <em>trap</em>, and a trap is not a Java exception: it stops
     * the module, and no {@code catch} in the program it stopped ever runs. The one thing
     * {@link #parseDouble} owes its caller is a catchable {@link NumberFormatException}, so the
     * failure has to cross the boundary as a value — through the same out-array shape
     * {@link #toChars} uses, and for the same reason.
     */
    private static native boolean parseChars(char[] text, int offset, int count, double[] out);

    /** {@code value} in Java's decimal layout. */
    public static String toString(double value) {
        char[] out = new char[RENDERING_LIMIT];
        int written = toChars(value, out);
        return new String(out, 0, written);
    }

    /**
     * The {@code double} {@code text} spells.
     *
     * @throws NumberFormatException if {@code text} does not spell one
     */
    public static double parseDouble(String text) {
        if (text == null) {
            throw new NumberFormatException(text);
        }
        String trimmed = text.trim();
        if (trimmed.isEmpty()) {
            throw new NumberFormatException(text);
        }
        char[] chars = trimmed.toCharArray();
        double[] out = new double[1];
        if (!parseChars(chars, 0, chars.length, out)) {
            throw new NumberFormatException(text);
        }
        return out[0];
    }

    /** Whether {@code value} is not a number. */
    public static boolean isNaN(double value) {
        return value != value;
    }

    /** Whether {@code value} is an infinity. */
    public static boolean isInfinite(double value) {
        return value == POSITIVE_INFINITY || value == NEGATIVE_INFINITY;
    }

    /** Whether {@code value} is neither infinite nor a NaN. */
    public static boolean isFinite(double value) {
        return !isNaN(value) && !isInfinite(value);
    }

    /**
     * Total order over every {@code double}, NaN and signed zeroes included.
     *
     * <p>This is not {@code <}. A record's synthesised {@code equals} compares {@code double}
     * components with {@code compare(a, b) == 0}, which is what makes two NaN components equal and
     * {@code 0.0} and {@code -0.0} different (JLS 8.10.3).
     */
    public static int compare(double left, double right) {
        if (left < right) {
            return -1;
        }
        if (left > right) {
            return 1;
        }
        long leftBits = doubleToLongBits(left);
        long rightBits = doubleToLongBits(right);
        return Long.compare(leftBits, rightBits);
    }

    /** {@code value}'s hash: its bit pattern's two halves folded together. */
    public static int hashCode(double value) {
        return Long.hashCode(doubleToLongBits(value));
    }

    /**
     * The larger of two values, ordering NaN above everything and {@code 0.0} above
     * {@code -0.0}.
     */
    public static double max(double left, double right) {
        if (isNaN(left) || isNaN(right)) {
            return NaN;
        }
        return compare(left, right) >= 0 ? left : right;
    }

    /**
     * The smaller of two values, ordering NaN above everything and {@code -0.0} below
     * {@code 0.0}.
     */
    public static double min(double left, double right) {
        if (isNaN(left) || isNaN(right)) {
            return NaN;
        }
        return compare(left, right) <= 0 ? left : right;
    }

    @Override
    public boolean equals(Object other) {
        if (other == this) {
            return true;
        }
        if (!(other instanceof Double)) {
            return false;
        }
        return compare(((Double) other).value, this.value) == 0;
    }

    @Override
    public int hashCode() {
        return hashCode(this.value);
    }

    @Override
    public int compareTo(Double other) {
        return compare(this.value, other.value);
    }

    @Override
    public String toString() {
        return toString(this.value);
    }
}
