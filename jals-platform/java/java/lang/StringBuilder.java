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
        append(initial);
    }

    private void reserve(int extra) {
        int needed = this.count + extra;
        if (needed <= this.value.length) {
            return;
        }
        int grown = this.value.length * 2;
        while (grown < needed) {
            grown = grown * 2;
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

    /** Discard every character appended so far. */
    public StringBuilder setLength(int length) {
        if (length < 0 || length > this.count) {
            throw new StringIndexOutOfBoundsException();
        }
        this.count = length;
        return this;
    }

    /** This builder's characters, reversed in place. */
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
        return this;
    }

    @Override
    public String toString() {
        return new String(this.value, 0, this.count);
    }
}
