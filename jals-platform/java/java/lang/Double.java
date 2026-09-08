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

    /** The largest finite {@code double}. */
    public static final double MAX_VALUE = 1.7976931348623157E308;

    /** The smallest positive non-zero {@code double}. */
    public static final double MIN_VALUE = 4.9E-324;

    /** Positive infinity. */
    public static final double POSITIVE_INFINITY = 1.0 / 0.0;

    /** Negative infinity. */
    public static final double NEGATIVE_INFINITY = -1.0 / 0.0;

    /** The canonical not-a-number value. */
    public static final double NaN = 0.0 / 0.0;

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

    /** The {@code double} whose IEEE 754 bits are {@code bits}. */
    public static native double longBitsToDouble(long bits);

    /** Render {@code value} into {@code out}; how many characters were written. */
    private static native int toChars(double value, char[] out);

    /** The {@code double} that {@code count} characters of {@code text} at {@code offset} spell. */
    private static native double parseChars(char[] text, int offset, int count);

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
        return parseChars(chars, 0, chars.length);
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
        long leftBits = doubleToRawLongBits(left);
        long rightBits = doubleToRawLongBits(right);
        return Long.compare(leftBits, rightBits);
    }

    /** {@code value}'s hash: its bit pattern's two halves folded together. */
    public static int hashCode(double value) {
        return Long.hashCode(doubleToRawLongBits(value));
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
