package java.lang;

/**
 * The boxed {@code char} — one UTF-16 code unit, boxed.
 *
 * <p>No {@code isDigit} and no {@code toUpperCase}: this is the value and its text, not the Unicode
 * tables. The ones that matter to a build script — comparing and appending — need no table at all,
 * and a table that were wrong would be worse than one that is absent.
 */
public class Character extends Object implements Comparable {

    private char value;

    private Character(char value) {
        this.value = value;
    }

    public static Character valueOf(char c) {
        return new Character(c);
    }

    public char charValue() {
        return this.value;
    }

    /** The one code unit as a one-unit string. */
    public String toString() {
        return new StringBuilder().append(this.value).toString();
    }

    public boolean equals(Object o) {
        if (!(o instanceof Character)) {
            return false;
        }
        return ((Character) o).value == this.value;
    }

    /** The code unit itself, which is the JDK's definition. */
    public int hashCode() {
        return this.value;
    }
}
