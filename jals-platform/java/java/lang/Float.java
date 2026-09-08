package java.lang;

/**
 * The {@code float} wrapper, and the four operations on one that Java cannot express.
 *
 * <p>The same four bindings as {@link Double}, at {@code float} width, and the width is the point.
 * Rendering a {@code float} by widening it to a {@code double} first prints
 * {@code 0.10000000149011612} where Java prints {@code 0.1}, and parsing one that way rounds twice.
 * So each has its own binding rather than one shared with the wider type.
 */
public final class Float extends Number implements Comparable<Float> {

    /** The largest finite {@code float}. */
    public static final float MAX_VALUE = 3.4028235E38f;

    /** The smallest positive non-zero {@code float}. */
    public static final float MIN_VALUE = 1.4E-45f;

    /** Positive infinity. */
    public static final float POSITIVE_INFINITY = 1.0f / 0.0f;

    /** Negative infinity. */
    public static final float NEGATIVE_INFINITY = -1.0f / 0.0f;

    /** The canonical not-a-number value. */
    public static final float NaN = 0.0f / 0.0f;

    /** How many bits a {@code float} occupies. */
    public static final int SIZE = 32;

    /** How many bytes a {@code float} occupies. */
    public static final int BYTES = 4;

    /** How many characters a rendering can take; see {@link Double}. */
    private static final int RENDERING_LIMIT = 32;

    private final float value;

    public Float(float value) {
        this.value = value;
    }

    /** A wrapper holding {@code value}. */
    public static Float valueOf(float value) {
        return new Float(value);
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
        return this.value;
    }

    @Override
    public double doubleValue() {
        return (double) this.value;
    }

    /** {@code value}'s IEEE 754 bits, not collapsing a signalling NaN. */
    public static native int floatToRawIntBits(float value);

    /** The {@code float} whose IEEE 754 bits are {@code bits}. */
    public static native float intBitsToFloat(int bits);

    /** Render {@code value} into {@code out}; how many characters were written. */
    private static native int toChars(float value, char[] out);

    /** The {@code float} that {@code count} characters of {@code text} at {@code offset} spell. */
    private static native float parseChars(char[] text, int offset, int count);

    /** {@code value} in Java's decimal layout, at {@code float} width. */
    public static String toString(float value) {
        char[] out = new char[RENDERING_LIMIT];
        int written = toChars(value, out);
        return new String(out, 0, written);
    }

    /**
     * The {@code float} {@code text} spells.
     *
     * @throws NumberFormatException if {@code text} does not spell one
     */
    public static float parseFloat(String text) {
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
    public static boolean isNaN(float value) {
        return value != value;
    }

    /** Whether {@code value} is an infinity. */
    public static boolean isInfinite(float value) {
        return value == POSITIVE_INFINITY || value == NEGATIVE_INFINITY;
    }

    /** Whether {@code value} is neither infinite nor a NaN. */
    public static boolean isFinite(float value) {
        return !isNaN(value) && !isInfinite(value);
    }

    /** Total order over every {@code float}, NaN and signed zeroes included; see {@link Double}. */
    public static int compare(float left, float right) {
        if (left < right) {
            return -1;
        }
        if (left > right) {
            return 1;
        }
        return Integer.compare(floatToRawIntBits(left), floatToRawIntBits(right));
    }

    /** {@code value}'s hash, which is its bit pattern. */
    public static int hashCode(float value) {
        return floatToRawIntBits(value);
    }

    /** The larger of two values, under {@link #compare}'s order. */
    public static float max(float left, float right) {
        if (isNaN(left) || isNaN(right)) {
            return NaN;
        }
        return compare(left, right) >= 0 ? left : right;
    }

    /** The smaller of two values, under {@link #compare}'s order. */
    public static float min(float left, float right) {
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
        if (!(other instanceof Float)) {
            return false;
        }
        return compare(((Float) other).value, this.value) == 0;
    }

    @Override
    public int hashCode() {
        return hashCode(this.value);
    }

    @Override
    public int compareTo(Float other) {
        return compare(this.value, other.value);
    }

    @Override
    public String toString() {
        return toString(this.value);
    }
}
