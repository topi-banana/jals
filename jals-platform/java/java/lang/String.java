package java.lang;

/**
 * An immutable sequence of UTF-16 code units.
 *
 * <p>This is the class a compiled module's string literals are built from. The compiler emits the
 * characters as module data, copies them into a {@code char[]} with {@code array.new_data}, and
 * calls the constructor below; the factory the ABI exports is that constructor, so a literal in one
 * module and a {@code new String(chars)} in another become the same object by the same code.
 *
 * <p>What is written here is the contract the JDK documents rather than the implementation it
 * happens to have. The array is copied on the way in — a string is immutable, and a constructor
 * that kept the caller's array would make that a promise the caller could break. There is no
 * compaction, no cached hash, and no interning: all three are invisible to Java, and a build script
 * needs none of them. The surface is the standard library stub's set, so code that checked against
 * the stub resolves against the real class, plus the two {@code Object} methods a stub cannot
 * usefully implement.
 */
public class String extends Object implements CharSequence, Comparable {

    /** The code units, never null and never the array a caller still holds. */
    private char[] value;

    /** The empty string. */
    public String() {
        this.value = new char[0];
    }

    /**
     * A string with the code units of {@code value}, copied.
     *
     * <p>This is the constructor the compiler's literal path resolves: the signature is the one
     * {@code array.new_data} can fill, and the copy is what makes the result immutable.
     */
    public String(char[] value) {
        this.value = new char[value.length];
        int i = 0;
        while (i < this.value.length) {
            this.value[i] = value[i];
            i = i + 1;
        }
    }

    /** The number of code units — not, for a string with surrogate pairs, the number of characters. */
    public int length() {
        return this.value.length;
    }

    /** The code unit at {@code index}, as {@code char} has always meant "a UTF-16 code unit". */
    public char charAt(int index) {
        return this.value[index];
    }

    /** Whether the string has no code units. */
    public boolean isEmpty() {
        return this.value.length == 0;
    }

    /** The code units from {@code beginIndex} to the end. */
    public String substring(int beginIndex) {
        return this.substring(beginIndex, this.value.length);
    }

    /**
     * The code units from {@code beginIndex} to {@code endIndex}.
     *
     * <p>The copy is the whole method: the JDK's substring used to share the array and that was a
     * memory leak, and sharing is not available anyway once the array is this class's own.
     */
    public String substring(int beginIndex, int endIndex) {
        char[] copy = new char[endIndex - beginIndex];
        int i = 0;
        while (i < copy.length) {
            copy[i] = this.value[beginIndex + i];
            i = i + 1;
        }
        return new String(copy);
    }

    /**
     * Whether {@code o} is a string with the same code units.
     *
     * <p>{@code instanceof} rather than a class comparison: a {@code String} is final in the JDK
     * and every subclass would still be a string, so the test is the one that stays right if a
     * later platform ever adds a sibling.
     */
    public boolean equals(Object o) {
        if (!(o instanceof String)) {
            return false;
        }
        String other = (String) o;
        if (other.value.length != this.value.length) {
            return false;
        }
        int i = 0;
        while (i < this.value.length) {
            if (this.value[i] != other.value[i]) {
                return false;
            }
            i = i + 1;
        }
        return true;
    }

    /** The JDK's hash: {@code s[0]*31^(n-1) + ... + s[n-1]}, so it is stable across host JVMs. */
    public int hashCode() {
        int hash = 0;
        int i = 0;
        while (i < this.value.length) {
            hash = hash * 31 + this.value[i];
            i = i + 1;
        }
        return hash;
    }

    /** The string itself. A string *is* its own text representation. */
    public String toString() {
        return this;
    }

    /** The index of the first occurrence of {@code ch}, or {@code -1}. */
    public int indexOf(int ch) {
        int i = 0;
        while (i < this.value.length) {
            if (this.value[i] == ch) {
                return i;
            }
            i = i + 1;
        }
        return -1;
    }

    /**
     * A new string with this one's code units followed by {@code s}'s.
     *
     * <p>Not what {@code +} lowers to — that is a {@link StringBuilder} chain, so a loop of
     * concatenations stays linear — but it is the method the stub declares and the one user code
     * calls by name.
     */
    public String concat(String s) {
        int left = this.value.length;
        int right = s.value.length;
        char[] copy = new char[left + right];
        int i = 0;
        while (i < left) {
            copy[i] = this.value[i];
            i = i + 1;
        }
        i = 0;
        while (i < right) {
            copy[left + i] = s.value[i];
            i = i + 1;
        }
        return new String(copy);
    }
}
