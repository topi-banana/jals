package java.lang;

/**
 * A sequence of {@code char} values.
 *
 * <p>A {@code String} here is a {@code char[]} and nothing else — no offset, no shared backing
 * array, no interning. Every instance owns its characters, so {@link #substring} copies. That is
 * the simple implementation rather than the JDK's, and on a target whose collector is the
 * embedder's it is also the one with no aliasing to reason about.
 *
 * <p>There is no string literal on this target yet: the backend refuses one. A constant this class
 * needs is therefore written as a {@code char[]} initialiser and wrapped once in a {@code static}
 * field, which is what every constant in this package looks like.
 *
 * <p>{@link #toUpperCase} and {@link #toLowerCase} map ASCII only, and say so rather than
 * pretending otherwise: full Unicode case mapping is a table this package does not carry.
 */
public final class String implements CharSequence, Comparable<String> {

    /** {@code "null"}, for the one place a null reference is rendered rather than refused. */
    private static final char[] NULL_TEXT = {'n', 'u', 'l', 'l'};

    private static final String NULL = new String(NULL_TEXT, 0, NULL_TEXT.length);

    private final char[] value;

    /** An empty string. */
    public String() {
        this.value = new char[0];
    }

    /** A string holding a copy of every character in {@code chars}. */
    public String(char[] chars) {
        this(chars, 0, chars.length);
    }

    /** A string holding a copy of {@code count} characters from {@code chars} at {@code offset}. */
    public String(char[] chars, int offset, int count) {
        if (offset < 0 || count < 0 || offset + count > chars.length) {
            throw new StringIndexOutOfBoundsException();
        }
        char[] copied = new char[count];
        for (int i = 0; i < count; i++) {
            copied[i] = chars[offset + i];
        }
        this.value = copied;
    }

    /** A string with the same characters as {@code other}. */
    public String(String other) {
        this(other.value, 0, other.value.length);
    }

    @Override
    public int length() {
        return this.value.length;
    }

    /** Whether this string has no characters. */
    public boolean isEmpty() {
        return this.value.length == 0;
    }

    @Override
    public char charAt(int index) {
        if (index < 0 || index >= this.value.length) {
            throw new StringIndexOutOfBoundsException();
        }
        return this.value[index];
    }

    /** A copy of this string's characters. */
    public char[] toCharArray() {
        char[] out = new char[this.value.length];
        for (int i = 0; i < this.value.length; i++) {
            out[i] = this.value[i];
        }
        return out;
    }

    /** The characters from {@code begin} to the end. */
    public String substring(int begin) {
        return substring(begin, this.value.length);
    }

    /** The characters in {@code [begin, end)}. */
    public String substring(int begin, int end) {
        if (begin < 0 || end > this.value.length || begin > end) {
            throw new StringIndexOutOfBoundsException();
        }
        return new String(this.value, begin, end - begin);
    }

    /** This string followed by {@code other}. */
    public String concat(String other) {
        if (other == null || other.value.length == 0) {
            return this;
        }
        char[] joined = new char[this.value.length + other.value.length];
        for (int i = 0; i < this.value.length; i++) {
            joined[i] = this.value[i];
        }
        for (int i = 0; i < other.value.length; i++) {
            joined[this.value.length + i] = other.value[i];
        }
        return new String(joined, 0, joined.length);
    }

    /** The index of the first {@code ch} at or after {@code from}, or {@code -1}. */
    public int indexOf(int ch, int from) {
        int start = from < 0 ? 0 : from;
        for (int i = start; i < this.value.length; i++) {
            if (this.value[i] == (char) ch) {
                return i;
            }
        }
        return -1;
    }

    /** The index of the first {@code ch}, or {@code -1}. */
    public int indexOf(int ch) {
        return indexOf(ch, 0);
    }

    /** The index of the last {@code ch}, or {@code -1}. */
    public int lastIndexOf(int ch) {
        for (int i = this.value.length - 1; i >= 0; i--) {
            if (this.value[i] == (char) ch) {
                return i;
            }
        }
        return -1;
    }

    /** Whether this string begins with {@code prefix}. */
    public boolean startsWith(String prefix) {
        if (prefix.value.length > this.value.length) {
            return false;
        }
        for (int i = 0; i < prefix.value.length; i++) {
            if (this.value[i] != prefix.value[i]) {
                return false;
            }
        }
        return true;
    }

    /** Whether this string ends with {@code suffix}. */
    public boolean endsWith(String suffix) {
        int offset = this.value.length - suffix.value.length;
        if (offset < 0) {
            return false;
        }
        for (int i = 0; i < suffix.value.length; i++) {
            if (this.value[offset + i] != suffix.value[i]) {
                return false;
            }
        }
        return true;
    }

    /** Whether this string holds {@code needle} anywhere. */
    public boolean contains(String needle) {
        return indexOf(needle) >= 0;
    }

    /** The index at which {@code needle} first occurs, or {@code -1}. */
    public int indexOf(String needle) {
        int limit = this.value.length - needle.value.length;
        for (int start = 0; start <= limit; start++) {
            int i = 0;
            while (i < needle.value.length && this.value[start + i] == needle.value[i]) {
                i++;
            }
            if (i == needle.value.length) {
                return start;
            }
        }
        return -1;
    }

    /** This string with every ASCII letter upper-cased; other characters unchanged. */
    public String toUpperCase() {
        char[] out = toCharArray();
        for (int i = 0; i < out.length; i++) {
            out[i] = Character.toUpperCase(out[i]);
        }
        return new String(out, 0, out.length);
    }

    /** This string with every ASCII letter lower-cased; other characters unchanged. */
    public String toLowerCase() {
        char[] out = toCharArray();
        for (int i = 0; i < out.length; i++) {
            out[i] = Character.toLowerCase(out[i]);
        }
        return new String(out, 0, out.length);
    }

    /** This string without leading or trailing characters at or below {@code U+0020}. */
    public String trim() {
        int start = 0;
        int end = this.value.length;
        while (start < end && this.value[start] <= ' ') {
            start++;
        }
        while (end > start && this.value[end - 1] <= ' ') {
            end--;
        }
        return substring(start, end);
    }

    /** Whether {@code other} is a string with the same characters. */
    @Override
    public boolean equals(Object other) {
        if (other == this) {
            return true;
        }
        if (!(other instanceof String)) {
            return false;
        }
        String that = (String) other;
        if (that.value.length != this.value.length) {
            return false;
        }
        for (int i = 0; i < this.value.length; i++) {
            if (this.value[i] != that.value[i]) {
                return false;
            }
        }
        return true;
    }

    @Override
    public int compareTo(String other) {
        int shorter = this.value.length < other.value.length ? this.value.length
                : other.value.length;
        for (int i = 0; i < shorter; i++) {
            if (this.value[i] != other.value[i]) {
                return this.value[i] - other.value[i];
            }
        }
        return this.value.length - other.value.length;
    }

    /** The JDK's hash: {@code s[0]*31^(n-1) + s[1]*31^(n-2) + ... + s[n-1]}. */
    @Override
    public int hashCode() {
        int hash = 0;
        for (int i = 0; i < this.value.length; i++) {
            hash = 31 * hash + this.value[i];
        }
        return hash;
    }

    @Override
    public String toString() {
        return this;
    }

    /** {@code value}'s rendering, or {@code "null"}. */
    public static String valueOf(Object value) {
        if (value == null) {
            return NULL;
        }
        return value.toString();
    }

    /** A string holding {@code chars}. */
    public static String valueOf(char[] chars) {
        return new String(chars, 0, chars.length);
    }

    /** A one-character string. */
    public static String valueOf(char value) {
        char[] one = {value};
        return new String(one, 0, 1);
    }

    /** {@code value} in base ten. */
    public static String valueOf(int value) {
        return Integer.toString(value);
    }

    /** {@code value} in base ten. */
    public static String valueOf(long value) {
        return Long.toString(value);
    }

    /** {@code "true"} or {@code "false"}. */
    public static String valueOf(boolean value) {
        return Boolean.toString(value);
    }

    /** {@code value} in Java's decimal layout. */
    public static String valueOf(double value) {
        return Double.toString(value);
    }

    /** {@code value} in Java's decimal layout, at {@code float} width. */
    public static String valueOf(float value) {
        return Float.toString(value);
    }
}
