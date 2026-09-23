package java.lang;

/**
 * The {@code char} wrapper, and the character classification this package carries.
 *
 * <p>Classification and case mapping are <strong>ASCII only</strong>, stated rather than implied:
 * full Unicode needs tables this package does not ship, and a half-Unicode answer that looked
 * general would be worse than a narrow one that says so. A character outside ASCII is returned
 * unchanged by both case mappings and classifies as neither letter nor digit.
 */
public final class Character implements Comparable<Character> {

    /** {@code "char"}, as a {@code char[]}. */
    private static final char[] PRIMITIVE_NAME = {'c', 'h', 'a', 'r'};

    /** The {@code Class} standing for the primitive type {@code char}. */
    public static final Class<Character> TYPE = new Class<Character>(new String(PRIMITIVE_NAME));

    /** The smallest radix the digit conversions accept. */
    public static final int MIN_RADIX = 2;

    /** The largest radix the digit conversions accept. */
    public static final int MAX_RADIX = 36;

    private final char value;

    public Character(char value) {
        this.value = value;
    }

    /** A wrapper holding {@code value}. */
    // `valueOf` is where the allocation is: this class *is* the wrapper, so the constructor it
    // would be told to call instead is this method.
    @SuppressWarnings("boxed-primitive-constructor")
    public static Character valueOf(char value) {
        return new Character(value);
    }

    /** The wrapped {@code char}. */
    public char charValue() {
        return this.value;
    }

    @Override
    public boolean equals(Object other) {
        if (other == this) {
            return true;
        }
        if (!(other instanceof Character)) {
            return false;
        }
        return ((Character) other).value == this.value;
    }

    @Override
    public int hashCode() {
        return this.value;
    }

    @Override
    public int compareTo(Character other) {
        return this.value - other.value;
    }

    @Override
    public String toString() {
        return String.valueOf(this.value);
    }

    /** Whether {@code c} is an ASCII decimal digit. */
    public static boolean isDigit(char c) {
        return c >= '0' && c <= '9';
    }

    /** Whether {@code c} is an ASCII letter. */
    public static boolean isLetter(char c) {
        return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z');
    }

    /** Whether {@code c} is an ASCII letter or decimal digit. */
    public static boolean isLetterOrDigit(char c) {
        return isLetter(c) || isDigit(c);
    }

    /** Whether {@code c} is an ASCII upper-case letter. */
    public static boolean isUpperCase(char c) {
        return c >= 'A' && c <= 'Z';
    }

    /** Whether {@code c} is an ASCII lower-case letter. */
    public static boolean isLowerCase(char c) {
        return c >= 'a' && c <= 'z';
    }

    /**
     * Whether {@code c} is ASCII whitespace.
     *
     * <p>Every ASCII character the JDK calls whitespace, which is more than the five with escapes:
     * the vertical tab and the four information separators are C0 controls the JDK admits, and
     * leaving them out is a divergence the class's "ASCII only" caveat does not cover. The
     * non-ASCII ones — {@code U+1680}, the {@code U+2000} block, {@code U+2028}/{@code U+2029},
     * {@code U+205F}, {@code U+3000} — are what that caveat is about.
     */
    public static boolean isWhitespace(char c) {
        if (c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f') {
            return true;
        }
        // The vertical tab, and the file, group, record and unit separators.
        return c == '\u000B' || (c >= '\u001C' && c <= '\u001F');
    }

    /** {@code c} upper-cased if it is an ASCII lower-case letter, else {@code c}. */
    public static char toUpperCase(char c) {
        if (isLowerCase(c)) {
            return (char) (c - ('a' - 'A'));
        }
        return c;
    }

    /** {@code c} lower-cased if it is an ASCII upper-case letter, else {@code c}. */
    public static char toLowerCase(char c) {
        if (isUpperCase(c)) {
            return (char) (c + ('a' - 'A'));
        }
        return c;
    }

    /** The value {@code c} denotes in {@code radix}, or {@code -1}. */
    public static int digit(char c, int radix) {
        if (radix < MIN_RADIX || radix > MAX_RADIX) {
            return -1;
        }
        int value = -1;
        if (isDigit(c)) {
            value = c - '0';
        } else if (c >= 'a' && c <= 'z') {
            value = c - 'a' + 10;
        } else if (c >= 'A' && c <= 'Z') {
            value = c - 'A' + 10;
        }
        if (value < 0 || value >= radix) {
            return -1;
        }
        return value;
    }

    /**
     * The character denoting {@code digit} in a radix that admits it, or the null character.
     *
     * <p>Code point zero and not a space, which is what the JDK returns and what the idiom
     * {@code forDigit(d, r) == 0} tests for. A space is a character a caller would write out.
     */
    public static char forDigit(int digit, int radix) {
        if (radix < MIN_RADIX || radix > MAX_RADIX || digit < 0 || digit >= radix) {
            return '\0';
        }
        if (digit < 10) {
            return (char) ('0' + digit);
        }
        return (char) ('a' + digit - 10);
    }
}
