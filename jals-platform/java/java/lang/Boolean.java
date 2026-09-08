package java.lang;

/** The {@code boolean} wrapper. */
public final class Boolean implements Comparable<Boolean> {

    private static final char[] TRUE_TEXT = {'t', 'r', 'u', 'e'};

    private static final char[] FALSE_TEXT = {'f', 'a', 'l', 's', 'e'};

    private static final String TRUE_STRING = new String(TRUE_TEXT, 0, TRUE_TEXT.length);

    private static final String FALSE_STRING = new String(FALSE_TEXT, 0, FALSE_TEXT.length);

    /** The wrapper holding {@code true}. */
    public static final Boolean TRUE = new Boolean(true);

    /** The wrapper holding {@code false}. */
    public static final Boolean FALSE = new Boolean(false);

    private final boolean value;

    public Boolean(boolean value) {
        this.value = value;
    }

    /** The shared wrapper holding {@code value}. */
    public static Boolean valueOf(boolean value) {
        return value ? TRUE : FALSE;
    }

    /** The wrapped {@code boolean}. */
    public boolean booleanValue() {
        return this.value;
    }

    /** {@code "true"} or {@code "false"}. */
    public static String toString(boolean value) {
        return value ? TRUE_STRING : FALSE_STRING;
    }

    /** Whether {@code text} spells {@code "true"}, ignoring case. */
    public static boolean parseBoolean(String text) {
        return text != null && text.toLowerCase().equals(TRUE_STRING);
    }

    @Override
    public boolean equals(Object other) {
        if (other == this) {
            return true;
        }
        if (!(other instanceof Boolean)) {
            return false;
        }
        return ((Boolean) other).value == this.value;
    }

    @Override
    public int hashCode() {
        return this.value ? 1231 : 1237;
    }

    @Override
    public int compareTo(Boolean other) {
        if (this.value == other.value) {
            return 0;
        }
        return this.value ? 1 : -1;
    }

    @Override
    public String toString() {
        return toString(this.value);
    }
}
