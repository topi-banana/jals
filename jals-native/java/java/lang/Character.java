package java.lang;

/**
 * A {@code char}, as an object, and the classification a {@code char} is asked for.
 *
 * <p>Everything below answers over the ASCII range and says so. A full Unicode general-category
 * table is tens of kilobytes that every module selecting this package would carry, and a method
 * that consulted half a table would answer wrongly for exactly the inputs a caller could not
 * predict — so the boundary is stated in each method's name and documentation instead of hidden
 * inside one.
 */
public final class Character implements Comparable<Character> {

    /** The lowest {@code char}. */
    public static final char MIN_VALUE = '\u0000';

    /** The highest {@code char}. */
    public static final char MAX_VALUE = '\uFFFF';

    /** How many bits a {@code char} has. */
    public static final int SIZE = 16;

    /** How many bytes a {@code char} has. */
    public static final int BYTES = 2;

    /** The largest radix {@link #digit} and {@link #forDigit} understand. */
    public static final int MAX_RADIX = 36;

    /** The smallest radix {@link #digit} and {@link #forDigit} understand. */
    public static final int MIN_RADIX = 2;

    /** {@code "char"}, the only way this target can spell a constant string. */
    private static final char[] TYPE_CHARS = {'c', 'h', 'a', 'r'};

    /** The identity of the primitive this class wraps. */
    public static final Class TYPE = new Class(new String(TYPE_CHARS));

    /** The wrapped value. */
    private final char value;

    /** A wrapper around {@code value}. */
    public Character(char value) {
        this.value = value;
    }

    /** A wrapper around {@code value}. */
    public static Character valueOf(char value) {
        return new Character(value);
    }

    /** The wrapped value. */
    public char charValue() {
        return this.value;
    }

    /** The wrapped value, which is what the JDK's {@code Character.hashCode} answers. */
    @Override
    public int hashCode() {
        return this.value;
    }

    /** Whether {@code other} is a {@code Character} wrapping the same value. */
    @Override
    public boolean equals(Object other) {
        if (!(other instanceof Character)) {
            return false;
        }
        return ((Character) other).value == this.value;
    }

    @Override
    public int compareTo(Character other) {
        return this.value - other.value;
    }

    /** A one-code-unit string. */
    @Override
    public String toString() {
        return String.valueOf(this.value);
    }

    /** A one-code-unit string. */
    public static String toString(char value) {
        return String.valueOf(value);
    }

    /** Whether {@code unit} is an ASCII decimal digit. */
    public static boolean isDigit(char unit) {
        return unit >= '0' && unit <= '9';
    }

    /** Whether {@code unit} is an ASCII letter. */
    public static boolean isLetter(char unit) {
        return (unit >= 'a' && unit <= 'z') || (unit >= 'A' && unit <= 'Z');
    }

    /** Whether {@code unit} is an ASCII letter or an ASCII decimal digit. */
    public static boolean isLetterOrDigit(char unit) {
        return isLetter(unit) || isDigit(unit);
    }

    /** Whether {@code unit} is an ASCII upper-case letter. */
    public static boolean isUpperCase(char unit) {
        return unit >= 'A' && unit <= 'Z';
    }

    /** Whether {@code unit} is an ASCII lower-case letter. */
    public static boolean isLowerCase(char unit) {
        return unit >= 'a' && unit <= 'z';
    }

    /** Whether {@code unit} is a space, a tab, a line break, a form feed, or a carriage return. */
    public static boolean isWhitespace(char unit) {
        return unit == ' ' || unit == '\t' || unit == '\n' || unit == '\f' || unit == '\r';
    }

    /** {@code unit} folded to upper case, over the ASCII range only. */
    public static char toUpperCase(char unit) {
        if (isLowerCase(unit)) {
            return (char) (unit - 32);
        }
        return unit;
    }

    /** {@code unit} folded to lower case, over the ASCII range only. */
    public static char toLowerCase(char unit) {
        if (isUpperCase(unit)) {
            return (char) (unit + 32);
        }
        return unit;
    }

    /**
     * The value {@code unit} has as a digit in {@code radix}, or {@code -1}.
     *
     * <p>The one method here every parser in this package goes through, which is why it is written
     * once: {@link Integer#parseInt} and {@link Long#parseLong} both read a digit through it, and
     * a second reading would be a second answer for {@code 'A'} in base sixteen.
     */
    public static int digit(char unit, int radix) {
        if (radix < MIN_RADIX || radix > MAX_RADIX) {
            return -1;
        }
        int value = -1;
        if (isDigit(unit)) {
            value = unit - '0';
        } else if (unit >= 'a' && unit <= 'z') {
            value = unit - 'a' + 10;
        } else if (unit >= 'A' && unit <= 'Z') {
            value = unit - 'A' + 10;
        }
        if (value < 0 || value >= radix) {
            return -1;
        }
        return value;
    }

    /** The code unit that spells {@code digit} in {@code radix}, or {@link #MIN_VALUE}. */
    public static char forDigit(int digit, int radix) {
        if (radix < MIN_RADIX || radix > MAX_RADIX || digit < 0 || digit >= radix) {
            return MIN_VALUE;
        }
        if (digit < 10) {
            return (char) ('0' + digit);
        }
        return (char) ('a' + digit - 10);
    }
}
