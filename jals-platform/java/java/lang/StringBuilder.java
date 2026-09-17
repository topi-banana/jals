package java.lang;

/**
 * A growable sequence of {@code char} values.
 *
 * <p>The {@code append} overloads are the whole set a string concatenation lowers to: {@code a + b}
 * becomes a builder chain, and each operand picks the overload its own static type names. A missing
 * one would send a {@code long} to {@code append(int)}, which compiles and prints the wrong number.
 */
public final class StringBuilder implements CharSequence {

    private static final int INITIAL_CAPACITY = 16;

    /** {@code "null"}, appended where a null reference is. */
    private static final char[] NULL_TEXT = {'n', 'u', 'l', 'l'};

    private char[] value;

    private int count;

    public StringBuilder() {
        this.value = new char[INITIAL_CAPACITY];
        this.count = 0;
    }

    public StringBuilder(String initial) {
        this();
        // The length is read first, exactly as the JDK sizes the buffer from it — which is also
        // what makes a `null` throw here instead of being appended as `"null"`. `append(null)` is
        // a statement about a value; a constructor argument is not one.
        reserve(initial.length());
        append(initial);
    }

    private void reserve(int extra) {
        int needed = this.count + extra;
        if (needed <= this.value.length) {
            return;
        }
        // Doubling overflows before a `char[]` can reach `Integer.MAX_VALUE`, and an overflowed
        // `grown` is negative — so `grown < needed` stays true, the next doubling lands on zero, and
        // `0 * 2` is zero for ever. The guard is what makes the loop terminate at all; past it the
        // request is `needed` exactly, and the allocation is what fails rather than this.
        int grown = this.value.length * 2;
        while (grown < needed) {
            grown = grown * 2;
            if (grown <= 0) {
                grown = needed;
            }
        }
        char[] larger = new char[grown];
        for (int i = 0; i < this.count; i++) {
            larger[i] = this.value[i];
        }
        this.value = larger;
    }

    private StringBuilder appendChars(char[] chars, int offset, int length) {
        reserve(length);
        for (int i = 0; i < length; i++) {
            this.value[this.count + i] = chars[offset + i];
        }
        this.count += length;
        return this;
    }

    /** Append {@code text}, or {@code "null"} when it is {@code null}. */
    public StringBuilder append(String text) {
        if (text == null) {
            return appendChars(NULL_TEXT, 0, NULL_TEXT.length);
        }
        char[] chars = text.toCharArray();
        return appendChars(chars, 0, chars.length);
    }

    /** Append {@code value}'s rendering, or {@code "null"}. */
    public StringBuilder append(Object value) {
        return append(String.valueOf(value));
    }

    /** Append every character in {@code chars}. */
    public StringBuilder append(char[] chars) {
        return appendChars(chars, 0, chars.length);
    }

    /** Append one character. */
    public StringBuilder append(char value) {
        reserve(1);
        this.value[this.count] = value;
        this.count++;
        return this;
    }

    /** Append {@code "true"} or {@code "false"}. */
    public StringBuilder append(boolean value) {
        return append(Boolean.toString(value));
    }

    /** Append {@code value} in base ten. */
    public StringBuilder append(int value) {
        return append(Integer.toString(value));
    }

    /** Append {@code value} in base ten. */
    public StringBuilder append(long value) {
        return append(Long.toString(value));
    }

    /** Append {@code value} in Java's decimal layout, at {@code float} width. */
    public StringBuilder append(float value) {
        return append(Float.toString(value));
    }

    /** Append {@code value} in Java's decimal layout. */
    public StringBuilder append(double value) {
        return append(Double.toString(value));
    }

    @Override
    public int length() {
        return this.count;
    }

    @Override
    public char charAt(int index) {
        if (index < 0 || index >= this.count) {
            throw new StringIndexOutOfBoundsException();
        }
        return this.value[index];
    }

    /** Truncate to {@code length} characters, or pad up to it with the null character. */
    public void setLength(int length) {
        if (length < 0) {
            throw new StringIndexOutOfBoundsException();
        }
        if (length > this.count) {
            reserve(length - this.count);
            // Written out rather than left to the allocation: a truncation keeps the characters it
            // dropped in the buffer, and growing back over them must not bring them back.
            for (int i = this.count; i < length; i++) {
                this.value[i] = (char) 0;
            }
        }
        this.count = length;
    }

    /**
     * This builder's characters, reversed in place.
     *
     * <p>A surrogate pair is one character, so reversing the code units alone is not enough: it
     * leaves every pair the wrong way round, a low surrogate ahead of its high one, which is not
     * valid UTF-16 at all. That matters more here than on a JVM — the host decodes what it is
     * handed, so an inverted pair reaches a terminal as two replacement characters rather than as
     * the character somebody wrote. So the code units are reversed and then each pair is put back,
     * which is what the JDK does and why its answer is a character reversal rather than a byte one.
     */
    public StringBuilder reverse() {
        int left = 0;
        int right = this.count - 1;
        while (left < right) {
            char held = this.value[left];
            this.value[left] = this.value[right];
            this.value[right] = held;
            left++;
            right--;
        }
        int at = 0;
        while (at < this.count - 1) {
            char low = this.value[at];
            char high = this.value[at + 1];
            // `0xDC00..0xDFFF` is the low half of the surrogate range and `0xD800..0xDBFF` the
            // high half; a `char` widens to `int` for the comparison, exactly as `String`'s own
            // surrogate arithmetic spells it.
            if (low >= 0xDC00 && low <= 0xDFFF && high >= 0xD800 && high <= 0xDBFF) {
                this.value[at] = high;
                this.value[at + 1] = low;
                at++;
            }
            at++;
        }
        return this;
    }

    @Override
    public String toString() {
        return new String(this.value, 0, this.count);
    }
}
