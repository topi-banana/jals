package java.lang;

/**
 * A {@code float}, as an object.
 *
 * <p>{@link Double}'s twin, down to the four {@code native} methods. Rendering and parsing are
 * this class's own rather than {@link Double}'s, and the reason is the one place the shortcut
 * would have been visible: the shortest decimal that reads back as the same {@code float} is
 * {@code "0.1"}, and the shortest that reads back as the same *{@code double}* — which is what
 * widening first would ask for — is {@code "0.10000000149011612"}. Two renderers, because they
 * answer two questions.
 */
public final class Float extends Number implements Comparable<Float> {

    /** The largest finite {@code float}. */
    public static final float MAX_VALUE = 3.4028235E38f;

    /** The smallest positive normal {@code float}. */
    public static final float MIN_NORMAL = 1.17549435E-38f;

    /** The smallest positive {@code float}, which is subnormal. */
    public static final float MIN_VALUE = 1.4E-45f;

    /** How many bits a {@code float} has. */
    public static final int SIZE = 32;

    /** How many bytes a {@code float} has. */
    public static final int BYTES = 4;

    /** A quiet not-a-number. */
    public static final float NaN = 0.0f / 0.0f;

    /** Positive infinity. */
    public static final float POSITIVE_INFINITY = 1.0f / 0.0f;

    /** Negative infinity. */
    public static final float NEGATIVE_INFINITY = -1.0f / 0.0f;

    /** {@code "float"}, the only way this target can spell a constant string. */
    private static final char[] TYPE_CHARS = {'f', 'l', 'o', 'a', 't'};

    /** The identity of the primitive this class wraps. */
    public static final Class TYPE = new Class(new String(TYPE_CHARS));

    /** Room for the longest rendering {@link #toChars} produces. */
    private static final int RENDERING_LIMIT = 32;

    /** The wrapped value. */
    private final float value;

    /** A wrapper around {@code value}. */
    public Float(float value) {
        this.value = value;
    }

    /** A wrapper around {@code value}. */
    public static Float valueOf(float value) {
        return new Float(value);
    }

    /** The value {@code text} spells, wrapped. */
    public static Float valueOf(String text) {
        return new Float(parseFloat(text));
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
        return this.value;
    }

    /** The bits, which is what the JDK's {@code Float.hashCode} answers. */
    @Override
    public int hashCode() {
        return floatToIntBits(this.value);
    }

    /** Whether {@code other} is a {@code Float} whose bits are the same. */
    @Override
    public boolean equals(Object other) {
        if (!(other instanceof Float)) {
            return false;
        }
        return floatToIntBits(((Float) other).value) == floatToIntBits(this.value);
    }

    @Override
    public int compareTo(Float other) {
        return compare(this.value, other.value);
    }

    /** The wrapped value, rendered. */
    @Override
    public String toString() {
        return toString(this.value);
    }

    /** {@code value} as the shortest decimal that reads back as itself. */
    public static String toString(float value) {
        char[] rendered = new char[RENDERING_LIMIT];
        int length = toChars(value, rendered);
        return new String(rendered, 0, length);
    }

    /** The {@code float} {@code text} spells. */
    public static float parseFloat(String text) {
        if (text == null) {
            throw new NullPointerException();
        }
        String trimmed = text.trim();
        if (trimmed.isEmpty()) {
            throw new NumberFormatException(text);
        }
        char[] units = trimmed.toCharArray();
        float parsed = parseChars(units, 0, units.length);
        if (parsed != parsed && !spellsNaN(units)) {
            throw new NumberFormatException(text);
        }
        return parsed;
    }

    /** Total order over {@code float}, as the JDK defines it. */
    public static int compare(float left, float right) {
        if (left < right) {
            return -1;
        }
        if (left > right) {
            return 1;
        }
        int leftBits = floatToIntBits(left);
        int rightBits = floatToIntBits(right);
        if (leftBits == rightBits) {
            return 0;
        }
        return leftBits < rightBits ? -1 : 1;
    }

    /** Whether {@code value} is not a number. */
    public static boolean isNaN(float value) {
        return value != value;
    }

    /** Whether {@code value} is one of the two infinities. */
    public static boolean isInfinite(float value) {
        return value == POSITIVE_INFINITY || value == NEGATIVE_INFINITY;
    }

    /** Whether {@code value} is neither infinite nor not-a-number. */
    public static boolean isFinite(float value) {
        return !isNaN(value) && !isInfinite(value);
    }

    /** The bits of {@code value}, with every not-a-number collapsed onto one. */
    public static int floatToIntBits(float value) {
        if (isNaN(value)) {
            return 0x7FC00000;
        }
        return floatToRawIntBits(value);
    }

    /** The bits of {@code value}, verbatim. */
    public static native int floatToRawIntBits(float value);

    /** The {@code float} whose bits are {@code bits}. */
    public static native float intBitsToFloat(int bits);

    /**
     * Write {@code value}'s rendering into {@code destination} and answer how many code units it
     * took.
     *
     * <p>An out-parameter for the reason {@link Double}'s is: a host function cannot allocate a
     * Java object, so the module allocates the array and the host fills it.
     */
    private static native int toChars(float value, char[] destination);

    /** Read {@code count} code units of {@code text} from {@code offset} as a {@code float}. */
    private static native float parseChars(char[] text, int offset, int count);

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
