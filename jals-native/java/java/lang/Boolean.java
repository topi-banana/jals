package java.lang;

/** A {@code boolean}, as an object. */
public final class Boolean implements Comparable<Boolean> {

    /** {@code "boolean"}, the only way this target can spell a constant string. */
    private static final char[] TYPE_CHARS = {'b', 'o', 'o', 'l', 'e', 'a', 'n'};

    /** The identity of the primitive this class wraps. */
    public static final Class TYPE = new Class(new String(TYPE_CHARS));

    /** {@code "true"}. */
    private static final char[] TRUE_CHARS = {'t', 'r', 'u', 'e'};

    /** {@code "false"}. */
    private static final char[] FALSE_CHARS = {'f', 'a', 'l', 's', 'e'};

    /** {@link #TRUE_CHARS}, built once — every {@code toString(true)} answers with this. */
    private static final String TRUE_TEXT = new String(TRUE_CHARS);

    /** {@link #FALSE_CHARS}, built once. */
    private static final String FALSE_TEXT = new String(FALSE_CHARS);

    /** A wrapper around {@code true}. */
    public static final Boolean TRUE = new Boolean(true);

    /** A wrapper around {@code false}. */
    public static final Boolean FALSE = new Boolean(false);

    /** The wrapped value. */
    private final boolean value;

    /** A wrapper around {@code value}. */
    public Boolean(boolean value) {
        this.value = value;
    }

    /** {@link #TRUE} or {@link #FALSE} — the two instances are all there are. */
    public static Boolean valueOf(boolean value) {
        return value ? TRUE : FALSE;
    }

    /** {@link #TRUE} when {@code text} spells {@code "true"} ignoring case, else {@link #FALSE}. */
    public static Boolean valueOf(String text) {
        return valueOf(parseBoolean(text));
    }

    /** The wrapped value. */
    public boolean booleanValue() {
        return this.value;
    }

    /** {@code 1231} or {@code 1237}, as the JDK's {@code Boolean.hashCode} answers. */
    @Override
    public int hashCode() {
        return this.value ? 1231 : 1237;
    }

    /** Whether {@code other} is a {@code Boolean} wrapping the same value. */
    @Override
    public boolean equals(Object other) {
        if (!(other instanceof Boolean)) {
            return false;
        }
        return ((Boolean) other).value == this.value;
    }

    @Override
    public int compareTo(Boolean other) {
        return compare(this.value, other.value);
    }

    /** {@code "true"} or {@code "false"}. */
    @Override
    public String toString() {
        return toString(this.value);
    }

    /** {@code "true"} or {@code "false"}. */
    public static String toString(boolean value) {
        return value ? TRUE_TEXT : FALSE_TEXT;
    }

    /** Whether {@code text} spells {@code "true"}, ignoring case. */
    public static boolean parseBoolean(String text) {
        return text != null && text.equalsIgnoreCase(TRUE_TEXT);
    }

    /** {@code false} sorts before {@code true}. */
    public static int compare(boolean left, boolean right) {
        if (left == right) {
            return 0;
        }
        return left ? 1 : -1;
    }

    /** {@code left && right}, named so it can be passed where a method is wanted. */
    public static boolean logicalAnd(boolean left, boolean right) {
        return left && right;
    }

    /** {@code left || right}, named so it can be passed where a method is wanted. */
    public static boolean logicalOr(boolean left, boolean right) {
        return left || right;
    }

    /** {@code left ^ right}, named so it can be passed where a method is wanted. */
    public static boolean logicalXor(boolean left, boolean right) {
        return left ^ right;
    }
}
