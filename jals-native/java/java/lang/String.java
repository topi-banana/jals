package java.lang;

/**
 * An immutable sequence of UTF-16 code units.
 *
 * <p>A {@code String} is a {@code char[]} and nothing else. That is not a simplification of the
 * JDK's layout so much as the only one available: a WebAssembly array is an object the embedder's
 * collector owns, and it is the one aggregate this target can index. Every method below is written
 * over that array.
 *
 * <h2>What is not here, and why</h2>
 *
 * <p>There is no string literal on this target yet — the backend refuses one — so a constant this
 * class needs is written as a {@code char[]} initialiser and wrapped once in a {@code static}
 * field. The ugliness is deliberate and local: it is the whole cost of the gap, and it lives in
 * the library rather than in every file that uses one.
 *
 * <p>{@code toUpperCase} and {@code toLowerCase} map the ASCII range only. Full Unicode case
 * mapping is a table this package does not carry, and a method that silently mapped half of its
 * input would be worse than one that says which half it maps.
 */
public final class String implements CharSequence, Comparable<String> {

    /** {@code "null"}, for the one place a null reference is rendered rather than refused. */
    private static final char[] NULL_TEXT = {'n', 'u', 'l', 'l'};

    /** The rendering of a null reference, built once. */
    private static final String NULL = new String(NULL_TEXT, 0, NULL_TEXT.length);

    /** What {@link #valueOf(Object)} refuses with, spelled the only way this target can spell it. */
    private static final char[] NO_RENDERING_TEXT = {
        'n', 'o', ' ', 'r', 'e', 'n', 'd', 'e', 'r', 'i', 'n', 'g', ' ', 'f', 'o', 'r', ' ',
        't', 'h', 'i', 's', ' ', 'r', 'e', 'f', 'e', 'r', 'e', 'n', 'c', 'e'
    };

    /** The refusal {@link #valueOf(Object)} carries, built once. */
    private static final String NO_RENDERING =
            new String(NO_RENDERING_TEXT, 0, NO_RENDERING_TEXT.length);

    /**
     * The code units, owned by this string.
     *
     * <p>Never shared with a caller: every constructor copies in and {@link #toCharArray} copies
     * out, because a wasm array is mutable and immutability here is a property of this class
     * rather than of the array it holds.
     */
    private final char[] value;

    /** The empty string. */
    public String() {
        this.value = new char[0];
    }

    /** A string holding a copy of every code unit of {@code chars}. */
    public String(char[] chars) {
        this(chars, 0, chars.length);
    }

    /** A string holding a copy of {@code count} code units of {@code chars} from {@code offset}. */
    public String(char[] chars, int offset, int count) {
        if (offset < 0 || count < 0 || offset > chars.length - count) {
            throw new StringIndexOutOfBoundsException(offset);
        }
        char[] copy = new char[count];
        int at = 0;
        while (at < count) {
            copy[at] = chars[offset + at];
            at = at + 1;
        }
        this.value = copy;
    }

    /**
     * A string holding the same code units as {@code original}.
     *
     * <p>The array is shared rather than copied: {@code original} never hands it out either, so
     * two strings over one array cannot be told apart by anything this class publishes.
     */
    public String(String original) {
        this.value = original.value;
    }

    @Override
    public int length() {
        return this.value.length;
    }

    /** Whether this string holds no code units. */
    public boolean isEmpty() {
        return this.value.length == 0;
    }

    @Override
    public char charAt(int index) {
        if (index < 0 || index >= this.value.length) {
            throw new StringIndexOutOfBoundsException(index);
        }
        return this.value[index];
    }

    /** A fresh array holding every code unit of this string. */
    public char[] toCharArray() {
        char[] copy = new char[this.value.length];
        int at = 0;
        while (at < copy.length) {
            copy[at] = this.value[at];
            at = at + 1;
        }
        return copy;
    }

    /**
     * Copy {@code [begin, end)} of this string into {@code destination} starting at
     * {@code destinationBegin}.
     */
    public void getChars(int begin, int end, char[] destination, int destinationBegin) {
        if (begin < 0 || end > this.value.length || begin > end) {
            throw new StringIndexOutOfBoundsException(begin);
        }
        int at = begin;
        int out = destinationBegin;
        while (at < end) {
            destination[out] = this.value[at];
            at = at + 1;
            out = out + 1;
        }
    }

    /**
     * Whether {@code other} is a string holding the same code units.
     *
     * <p>The {@code instanceof} names {@code String} and not {@code CharSequence} because this
     * target has no {@code ref.test} against an interface — an interface is {@code anyref} here and
     * has no type to test. That is also Java's own rule: a {@code String} never equals a
     * {@code StringBuilder}.
     */
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
        int at = 0;
        while (at < this.value.length) {
            if (this.value[at] != that.value[at]) {
                return false;
            }
            at = at + 1;
        }
        return true;
    }

    /** Whether {@code other} holds the same code units, ignoring ASCII case. */
    public boolean equalsIgnoreCase(String other) {
        if (other == null || other.value.length != this.value.length) {
            return false;
        }
        int at = 0;
        while (at < this.value.length) {
            if (lowerAscii(this.value[at]) != lowerAscii(other.value[at])) {
                return false;
            }
            at = at + 1;
        }
        return true;
    }

    /**
     * {@code s[0]*31^(n-1) + s[1]*31^(n-2) + ... + s[n-1]}, as {@code java.lang.String} has
     * specified it since 1.2.
     *
     * <p>Written out rather than approximated: the value is part of the JDK's published contract,
     * and a hash that differed would make a table built on one implementation unreadable by the
     * other.
     */
    @Override
    public int hashCode() {
        int hash = 0;
        int at = 0;
        while (at < this.value.length) {
            hash = 31 * hash + this.value[at];
            at = at + 1;
        }
        return hash;
    }

    /** Lexicographic order over code units, as {@code java.lang.String} defines it. */
    @Override
    public int compareTo(String other) {
        int shorter = this.value.length;
        if (other.value.length < shorter) {
            shorter = other.value.length;
        }
        int at = 0;
        while (at < shorter) {
            if (this.value[at] != other.value[at]) {
                return this.value[at] - other.value[at];
            }
            at = at + 1;
        }
        return this.value.length - other.value.length;
    }

    /** {@link #compareTo} over ASCII-case-folded code units. */
    public int compareToIgnoreCase(String other) {
        int shorter = this.value.length;
        if (other.value.length < shorter) {
            shorter = other.value.length;
        }
        int at = 0;
        while (at < shorter) {
            char left = lowerAscii(this.value[at]);
            char right = lowerAscii(other.value[at]);
            if (left != right) {
                return left - right;
            }
            at = at + 1;
        }
        return this.value.length - other.value.length;
    }

    /** The index of the first {@code unit}, or {@code -1}. */
    public int indexOf(int unit) {
        return indexOf(unit, 0);
    }

    /** The index of the first {@code unit} at or after {@code from}, or {@code -1}. */
    public int indexOf(int unit, int from) {
        int at = from;
        if (at < 0) {
            at = 0;
        }
        while (at < this.value.length) {
            if (this.value[at] == unit) {
                return at;
            }
            at = at + 1;
        }
        return -1;
    }

    /** The index of the last {@code unit}, or {@code -1}. */
    public int lastIndexOf(int unit) {
        int at = this.value.length - 1;
        while (at >= 0) {
            if (this.value[at] == unit) {
                return at;
            }
            at = at - 1;
        }
        return -1;
    }

    /** The index where {@code needle} first occurs, or {@code -1}. */
    public int indexOf(String needle) {
        return indexOf(needle, 0);
    }

    /** The index where {@code needle} first occurs at or after {@code from}, or {@code -1}. */
    public int indexOf(String needle, int from) {
        int at = from;
        if (at < 0) {
            at = 0;
        }
        int last = this.value.length - needle.value.length;
        while (at <= last) {
            if (matchesAt(needle, at)) {
                return at;
            }
            at = at + 1;
        }
        return -1;
    }

    /** The index where {@code needle} last occurs, or {@code -1}. */
    public int lastIndexOf(String needle) {
        int at = this.value.length - needle.value.length;
        while (at >= 0) {
            if (matchesAt(needle, at)) {
                return at;
            }
            at = at - 1;
        }
        return -1;
    }

    /** Whether {@code needle} occurs anywhere in this string. */
    public boolean contains(String needle) {
        return indexOf(needle, 0) >= 0;
    }

    /** Whether this string starts with {@code prefix}. */
    public boolean startsWith(String prefix) {
        return startsWith(prefix, 0);
    }

    /** Whether {@code prefix} occurs at {@code from}. */
    public boolean startsWith(String prefix, int from) {
        if (from < 0 || from > this.value.length - prefix.value.length) {
            return false;
        }
        return matchesAt(prefix, from);
    }

    /** Whether this string ends with {@code suffix}. */
    public boolean endsWith(String suffix) {
        return startsWith(suffix, this.value.length - suffix.value.length);
    }

    /** Everything from {@code begin} to the end. */
    public String substring(int begin) {
        return substring(begin, this.value.length);
    }

    /** The code units in {@code [begin, end)}. */
    public String substring(int begin, int end) {
        if (begin < 0 || end > this.value.length || begin > end) {
            throw new StringIndexOutOfBoundsException(begin);
        }
        return new String(this.value, begin, end - begin);
    }

    /** This string followed by {@code other}. */
    public String concat(String other) {
        if (other.value.length == 0) {
            return this;
        }
        char[] joined = new char[this.value.length + other.value.length];
        int at = 0;
        while (at < this.value.length) {
            joined[at] = this.value[at];
            at = at + 1;
        }
        int from = 0;
        while (from < other.value.length) {
            joined[at] = other.value[from];
            at = at + 1;
            from = from + 1;
        }
        return new String(joined, 0, joined.length);
    }

    /** This string with every {@code from} replaced by {@code to}. */
    public String replace(char from, char to) {
        char[] replaced = new char[this.value.length];
        int at = 0;
        while (at < this.value.length) {
            char unit = this.value[at];
            if (unit == from) {
                unit = to;
            }
            replaced[at] = unit;
            at = at + 1;
        }
        return new String(replaced, 0, replaced.length);
    }

    /** This string without leading or trailing code units at or below {@code U+0020}. */
    public String trim() {
        int begin = 0;
        int end = this.value.length;
        while (begin < end && this.value[begin] <= ' ') {
            begin = begin + 1;
        }
        while (end > begin && this.value[end - 1] <= ' ') {
            end = end - 1;
        }
        return substring(begin, end);
    }

    /** Whether this string is empty or holds only code units at or below {@code U+0020}. */
    public boolean isBlank() {
        int at = 0;
        while (at < this.value.length) {
            if (this.value[at] > ' ') {
                return false;
            }
            at = at + 1;
        }
        return true;
    }

    /** This string with every ASCII letter folded to upper case. */
    public String toUpperCase() {
        char[] mapped = new char[this.value.length];
        int at = 0;
        while (at < this.value.length) {
            mapped[at] = upperAscii(this.value[at]);
            at = at + 1;
        }
        return new String(mapped, 0, mapped.length);
    }

    /** This string with every ASCII letter folded to lower case. */
    public String toLowerCase() {
        char[] mapped = new char[this.value.length];
        int at = 0;
        while (at < this.value.length) {
            mapped[at] = lowerAscii(this.value[at]);
            at = at + 1;
        }
        return new String(mapped, 0, mapped.length);
    }

    /** This string written {@code count} times. */
    public String repeat(int count) {
        if (count < 0) {
            throw new IllegalArgumentException();
        }
        char[] repeated = new char[this.value.length * count];
        int out = 0;
        int done = 0;
        while (done < count) {
            int at = 0;
            while (at < this.value.length) {
                repeated[out] = this.value[at];
                out = out + 1;
                at = at + 1;
            }
            done = done + 1;
        }
        return new String(repeated, 0, repeated.length);
    }

    /** This string. */
    @Override
    public String toString() {
        return this;
    }

    /** A string holding a copy of every code unit of {@code chars}. */
    public static String valueOf(char[] chars) {
        return new String(chars, 0, chars.length);
    }

    /** A string holding a copy of {@code count} code units of {@code chars} from {@code offset}. */
    public static String valueOf(char[] chars, int offset, int count) {
        return new String(chars, offset, count);
    }

    /** {@code "true"} or {@code "false"}. */
    public static String valueOf(boolean value) {
        return Boolean.toString(value);
    }

    /** A one-code-unit string. */
    public static String valueOf(char value) {
        char[] one = new char[1];
        one[0] = value;
        return new String(one, 0, 1);
    }

    /** {@code value} in decimal. */
    public static String valueOf(int value) {
        return Integer.toString(value);
    }

    /** {@code value} in decimal. */
    public static String valueOf(long value) {
        return Long.toString(value);
    }

    /** {@code value} as {@link Float#toString} renders it. */
    public static String valueOf(float value) {
        return Float.toString(value);
    }

    /** {@code value} as {@link Double#toString} renders it. */
    public static String valueOf(double value) {
        return Double.toString(value);
    }

    /**
     * The rendering of a reference.
     *
     * <p>The one method in this package that cannot answer for every input, and the reason is
     * structural rather than an omission. {@code java.lang.Object} is not a type this package
     * declares — it is the backend's {@code anyref}, the top of wasm's reference hierarchy — so
     * there is no {@code Object.toString()} body to call. What is left is the set of renderings
     * this package can produce itself: {@code null}, and the two types it declares that hold text.
     * Anything else is refused with a message that says so, rather than rendered as something
     * nobody wrote.
     */
    public static String valueOf(Object value) {
        if (value == null) {
            return NULL;
        }
        if (value instanceof String) {
            return (String) value;
        }
        if (value instanceof StringBuilder) {
            return ((StringBuilder) value).toString();
        }
        throw new UnsupportedOperationException(NO_RENDERING);
    }

    /** Whether {@code needle} occurs at exactly {@code at}. */
    private boolean matchesAt(String needle, int at) {
        int seen = 0;
        while (seen < needle.value.length) {
            if (this.value[at + seen] != needle.value[seen]) {
                return false;
            }
            seen = seen + 1;
        }
        return true;
    }

    /** {@code unit} folded to lower case, over the ASCII range only. */
    private static char lowerAscii(char unit) {
        if (unit >= 'A' && unit <= 'Z') {
            return (char) (unit + 32);
        }
        return unit;
    }

    /** {@code unit} folded to upper case, over the ASCII range only. */
    private static char upperAscii(char unit) {
        if (unit >= 'a' && unit <= 'z') {
            return (char) (unit - 32);
        }
        return unit;
    }
}
