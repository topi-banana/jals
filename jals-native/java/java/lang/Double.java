package java.lang;

/**
 * A {@code double}, as an object, plus the four operations on one that Java cannot express.
 *
 * <p>{@link #doubleToRawLongBits} and {@link #longBitsToDouble} are {@code native} because there is
 * no reinterpreting cast in Java and no way to write one; {@link #toChars} and {@link #parseChars}
 * are {@code native} because rendering and reading a binary64 in decimal is a rounding problem
 * whose answer this package would otherwise have to approximate.
 *
 * <p>All four are the *whole* of the Rust half: {@link #toString} and {@link #parseDouble} are
 * ordinary Java on top of them, and so is everything else here.
 *
 * <h2>What {@code toString} renders</h2>
 *
 * <p>The shortest decimal that reads back as the same {@code double}, which is what the JDK has
 * rendered since JDK 19 (JDK-4511638) and what Rust's own formatter produces. On a JDK 17 or
 * earlier this and the JDK differ for the handful of values that release rendered with a
 * superfluous digit.
 */
public final class Double extends Number implements Comparable<Double> {

    /** The largest finite {@code double}. */
    public static final double MAX_VALUE = 1.7976931348623157E308;

    /** The smallest positive normal {@code double}. */
    public static final double MIN_NORMAL = 2.2250738585072014E-308;

    /** The smallest positive {@code double}, which is subnormal. */
    public static final double MIN_VALUE = 4.9E-324;

    /** How many bits a {@code double} has. */
    public static final int SIZE = 64;

    /** How many bytes a {@code double} has. */
    public static final int BYTES = 8;

    /** A quiet not-a-number. */
    public static final double NaN = 0.0d / 0.0d;

    /** Positive infinity. */
    public static final double POSITIVE_INFINITY = 1.0d / 0.0d;

    /** Negative infinity. */
    public static final double NEGATIVE_INFINITY = -1.0d / 0.0d;

    /** {@code "double"}, the only way this target can spell a constant string. */
    private static final char[] TYPE_CHARS = {'d', 'o', 'u', 'b', 'l', 'e'};

    /** The identity of the primitive this class wraps. */
    public static final Class TYPE = new Class(new String(TYPE_CHARS));

    /**
     * Room for the longest rendering {@link #toChars} produces.
     *
     * <p>A shortest round-trip binary64 is at most 17 significant digits, and the sign, the point
     * and a three-digit exponent fit inside the rest.
     */
    private static final int RENDERING_LIMIT = 32;

    /** The wrapped value. */
    private final double value;

    /** A wrapper around {@code value}. */
    public Double(double value) {
        this.value = value;
    }

    /** A wrapper around {@code value}. */
    public static Double valueOf(double value) {
        return new Double(value);
    }

    /** The value {@code text} spells, wrapped. */
    public static Double valueOf(String text) {
        return new Double(parseDouble(text));
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

    /** The bits, folded in half, which is what the JDK's {@code Double.hashCode} answers. */
    @Override
    public int hashCode() {
        long bits = doubleToLongBits(this.value);
        return (int) (bits ^ (bits >>> 32));
    }

    /**
     * Whether {@code other} is a {@code Double} whose bits are the same.
     *
     * <p>Bits and not {@code ==}, which is the JDK's rule and the reason it exists: two
     * {@code NaN}s are equal here and {@code 0.0} and {@code -0.0} are not, so a collection keyed
     * on a {@code Double} behaves.
     */
    @Override
    public boolean equals(Object other) {
        if (!(other instanceof Double)) {
            return false;
        }
        return doubleToLongBits(((Double) other).value) == doubleToLongBits(this.value);
    }

    @Override
    public int compareTo(Double other) {
        return compare(this.value, other.value);
    }

    /** The wrapped value, rendered. */
    @Override
    public String toString() {
        return toString(this.value);
    }

    /** {@code value} as the shortest decimal that reads back as itself. */
    public static String toString(double value) {
        char[] rendered = new char[RENDERING_LIMIT];
        int length = toChars(value, rendered);
        return new String(rendered, 0, length);
    }

    /** The {@code double} {@code text} spells. */
    public static double parseDouble(String text) {
        if (text == null) {
            throw new NullPointerException();
        }
        String trimmed = text.trim();
        if (trimmed.isEmpty()) {
            throw new NumberFormatException(text);
        }
        char[] units = trimmed.toCharArray();
        double parsed = parseChars(units, 0, units.length);
        if (parsed != parsed && !spellsNaN(units)) {
            throw new NumberFormatException(text);
        }
        return parsed;
    }

    /**
     * Total order over {@code double}, as the JDK defines it.
     *
     * <p>Not {@code <} and {@code >}: those leave {@code NaN} unordered and {@code -0.0} equal to
     * {@code 0.0}, and a sort built on them does not terminate on an array holding either.
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
        if (leftBits == rightBits) {
            return 0;
        }
        return leftBits < rightBits ? -1 : 1;
    }

    /** Whether {@code value} is not a number. */
    public static boolean isNaN(double value) {
        return value != value;
    }

    /** Whether {@code value} is one of the two infinities. */
    public static boolean isInfinite(double value) {
        return value == POSITIVE_INFINITY || value == NEGATIVE_INFINITY;
    }

    /** Whether {@code value} is neither infinite nor not-a-number. */
    public static boolean isFinite(double value) {
        return !isNaN(value) && !isInfinite(value);
    }

    /** The larger of the two. */
    public static double max(double left, double right) {
        return Math.max(left, right);
    }

    /** The smaller of the two. */
    public static double min(double left, double right) {
        return Math.min(left, right);
    }

    /** {@code left + right}, named so it can be passed where a method is wanted. */
    public static double sum(double left, double right) {
        return left + right;
    }

    /**
     * The bits of {@code value}, with every not-a-number collapsed onto one.
     *
     * <p>The JDK's {@code doubleToLongBits}, and the difference from
     * {@link #doubleToRawLongBits} is the whole reason both exist: {@link #equals} and
     * {@link #hashCode} must answer the same for two {@code NaN}s, and the raw bits of two of them
     * differ.
     */
    public static long doubleToLongBits(double value) {
        if (isNaN(value)) {
            return 0x7FF8000000000000L;
        }
        return doubleToRawLongBits(value);
    }

    /** The bits of {@code value}, verbatim. */
    public static native long doubleToRawLongBits(double value);

    /** The {@code double} whose bits are {@code bits}. */
    public static native double longBitsToDouble(long bits);

    /**
     * Write {@code value}'s rendering into {@code destination} and answer how many code units it
     * took.
     *
     * <p>An out-parameter rather than a returned string because a host function cannot allocate a
     * Java object — a wasm embedder has no {@code struct.new} — so the module allocates the array
     * and the host fills it. {@link #toString} is the wrapper that hides it.
     */
    private static native int toChars(double value, char[] destination);

    /**
     * Read {@code count} code units of {@code text} from {@code offset} as a {@code double}.
     *
     * <p>Answers {@link #NaN} for text that spells no number at all, which {@link #parseDouble}
     * turns into the {@link NumberFormatException} a caller expects — text that spells
     * {@code "NaN"} is the one input where that reading would be wrong, and it is checked for
     * separately.
     */
    private static native double parseChars(char[] text, int offset, int count);

    /** Whether {@code units} spells {@code "NaN"}, optionally signed. */
    private static boolean spellsNaN(char[] units) {
        int at = 0;
        if (at < units.length && (units[at] == '+' || units[at] == '-')) {
            at = at + 1;
        }
        return units.length - at == 3
                && units[at] == 'N'
                && units[at + 1] == 'a'
                && units[at + 2] == 'N';
    }
}
