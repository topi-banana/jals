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

    /** {@code "float"}, as a {@code char[]}. */
    private static final char[] PRIMITIVE_NAME = {'f', 'l', 'o', 'a', 't'};

    /** The {@code Class} standing for the primitive type {@code float}. */
    public static final Class<Float> TYPE = new Class<Float>(new String(PRIMITIVE_NAME));

    /** The largest finite {@code float}. */
    public static final float MAX_VALUE = 3.4028235E38f;

    /** The smallest positive non-zero {@code float}. */
    public static final float MIN_VALUE = 1.4E-45f;

    /** Positive infinity. */
    public static final float POSITIVE_INFINITY = 1.0f / 0.0f;

    /** Negative infinity. */
    public static final float NEGATIVE_INFINITY = -1.0f / 0.0f;

    /** The canonical not-a-number value. */
    // `NaN`, `out` and `err` are names the JDK fixed; a program spells them as written or
    // it does not compile against a real one.
    @SuppressWarnings("naming-convention")
    public static final float NaN = 0.0f / 0.0f;

    /** The one bit pattern {@link #floatToIntBits} answers for every NaN. */
    private static final int CANONICAL_NAN_BITS = 0x7fc00000;

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
    // `valueOf` is where the allocation is: this class *is* the wrapper, so the constructor it
    // would be told to call instead is this method.
    @SuppressWarnings("boxed-primitive-constructor")
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

    /**
     * {@code value}'s IEEE 754 bits, with every NaN collapsed to one pattern; see {@link
     * Double#doubleToLongBits}.
     */
    public static int floatToIntBits(float value) {
        if (isNaN(value)) {
            return CANONICAL_NAN_BITS;
        }
        return floatToRawIntBits(value);
    }

    /** The {@code float} whose IEEE 754 bits are {@code bits}. */
    public static native float intBitsToFloat(int bits);

    /** Render {@code value} into {@code out}; how many characters were written. */
    private static native int toChars(float value, char[] out);

    /**
     * Decode {@code count} characters of {@code text} at {@code offset} into {@code out[0]}.
     *
     * <p>Returns whether they spelled a {@code float}. See {@link Double#parseChars} for why a
     * failure crosses as a value: a binding that refuses becomes a trap, and a trap is not
     * something {@link #parseFloat}'s caller can catch.
     */
    private static native boolean parseChars(char[] text, int offset, int count, float[] out);

    /** {@code value} in Java's decimal layout, at {@code float} width. */
    public static String toString(float value) {
        char[] out = new char[RENDERING_LIMIT];
        int written = toChars(value, out);
        return new String(out, 0, written);
    }

    /**
     * The {@code float} {@code text} spells.
     *
     * <p>A {@code null} is a {@link NullPointerException} and not a {@link NumberFormatException},
     * which is the one place this method and {@link Integer#parseInt} disagree: the JDK reaches
     * {@code text.trim()} before it looks at anything, so the dereference is what fails. Getting
     * this wrong is silent — a {@code catch (NumberFormatException)} recovers here and propagates
     * on a JVM.
     *
     * @throws NullPointerException if {@code text} is {@code null}
     * @throws NumberFormatException if {@code text} does not spell one
     */
    public static float parseFloat(String text) {
        if (text == null) {
            throw new NullPointerException();
        }
        String trimmed = text.trim();
        if (trimmed.isEmpty()) {
            throw new NumberFormatException(text);
        }
        char[] chars = trimmed.toCharArray();
        float[] out = new float[1];
        if (!parseChars(chars, 0, chars.length, out)) {
            throw new NumberFormatException(text);
        }
        return out[0];
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
        return Integer.compare(floatToIntBits(left), floatToIntBits(right));
    }

    /** {@code value}'s hash, which is its bit pattern. */
    public static int hashCode(float value) {
        return floatToIntBits(value);
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
