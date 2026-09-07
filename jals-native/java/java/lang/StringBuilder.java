package java.lang;

/**
 * A mutable sequence of UTF-16 code units.
 *
 * <p>The buffer is a {@code char[]} that doubles when it fills, which is the same growth the JDK
 * uses and the reason {@link #append} is amortised constant time. Nothing here is synchronised —
 * this target is single-threaded by construction, so {@code StringBuffer} would be the same class
 * under a second name and is not declared.
 *
 * <p>Every {@code append} returns {@code this} so calls chain, which is also the shape the JVM
 * backend lowers {@code a + b} into. That lowering is not reachable on this target yet (the wasm
 * backend refuses a string literal, so it never sees a string concatenation), and the overloads
 * are written out anyway: they are what a project calls directly, and the set has to be complete
 * for the day the lowering arrives — a missing {@code append(long)} would send a {@code long} to
 * {@code append(int)}, which compiles and prints a different number.
 */
public final class StringBuilder implements CharSequence {

    /** What an empty builder starts with, as the JDK does. */
    private static final int INITIAL_CAPACITY = 16;

    /** The code units, of which the first {@link #count} are live. */
    private char[] value;

    /** How many code units of {@link #value} this builder holds. */
    private int count;

    /** An empty builder. */
    public StringBuilder() {
        this.value = new char[INITIAL_CAPACITY];
        this.count = 0;
    }

    /** An empty builder with room for {@code capacity} code units before it has to grow. */
    public StringBuilder(int capacity) {
        if (capacity < 0) {
            throw new NegativeArraySizeException();
        }
        this.value = new char[capacity];
        this.count = 0;
    }

    /** A builder holding {@code text}. */
    public StringBuilder(String text) {
        this.value = new char[text.length() + INITIAL_CAPACITY];
        this.count = 0;
        append(text);
    }

    @Override
    public int length() {
        return this.count;
    }

    @Override
    public char charAt(int index) {
        if (index < 0 || index >= this.count) {
            throw new StringIndexOutOfBoundsException(index);
        }
        return this.value[index];
    }

    /** Overwrite the code unit at {@code index}. */
    public void setCharAt(int index, char unit) {
        if (index < 0 || index >= this.count) {
            throw new StringIndexOutOfBoundsException(index);
        }
        this.value[index] = unit;
    }

    /** Drop everything after {@code length} code units. */
    public void setLength(int length) {
        if (length < 0) {
            throw new StringIndexOutOfBoundsException(length);
        }
        reserve(length);
        int at = this.count;
        while (at < length) {
            this.value[at] = '\u0000';
            at = at + 1;
        }
        this.count = length;
    }

    /** Append one code unit. */
    public StringBuilder append(char unit) {
        reserve(this.count + 1);
        this.value[this.count] = unit;
        this.count = this.count + 1;
        return this;
    }

    /**
     * Append every code unit of {@code text}, or {@code "null"} when it is null.
     *
     * <p>The null goes through {@link String#valueOf(Object)}, which is the overload a
     * {@code String} argument selects — there is no {@code valueOf(String)} — so the rendering of
     * a null reference is written down in one place rather than in each caller.
     */
    public StringBuilder append(String text) {
        String rendered = String.valueOf(text);
        return appendChars(rendered.toCharArray(), 0, rendered.length());
    }

    /** Append every code unit of {@code sequence}. */
    public StringBuilder append(CharSequence sequence) {
        int at = 0;
        int end = sequence.length();
        reserve(this.count + end);
        while (at < end) {
            append(sequence.charAt(at));
            at = at + 1;
        }
        return this;
    }

    /** Append every code unit of {@code chars}. */
    public StringBuilder append(char[] chars) {
        return appendChars(chars, 0, chars.length);
    }

    /** Append {@code count} code units of {@code chars} from {@code offset}. */
    public StringBuilder append(char[] chars, int offset, int count) {
        return appendChars(chars, offset, count);
    }

    /** Append {@code "true"} or {@code "false"}. */
    public StringBuilder append(boolean value) {
        return append(Boolean.toString(value));
    }

    /** Append {@code value} in decimal. */
    public StringBuilder append(int value) {
        return append(Integer.toString(value));
    }

    /** Append {@code value} in decimal. */
    public StringBuilder append(long value) {
        return append(Long.toString(value));
    }

    /** Append {@code value} as {@link Float#toString} renders it. */
    public StringBuilder append(float value) {
        return append(Float.toString(value));
    }

    /** Append {@code value} as {@link Double#toString} renders it. */
    public StringBuilder append(double value) {
        return append(Double.toString(value));
    }

    /**
     * Append the rendering of a reference.
     *
     * <p>Delegates to {@link String#valueOf(Object)} rather than repeating its rule, which is also
     * where the reason this overload cannot answer for every reference is written down.
     */
    public StringBuilder append(Object value) {
        return append(String.valueOf(value));
    }

    /** Reverse the code units in place. */
    public StringBuilder reverse() {
        int left = 0;
        int right = this.count - 1;
        while (left < right) {
            char held = this.value[left];
            this.value[left] = this.value[right];
            this.value[right] = held;
            left = left + 1;
            right = right - 1;
        }
        return this;
    }

    /** Drop the code units in {@code [begin, end)}. */
    public StringBuilder delete(int begin, int end) {
        if (begin < 0 || begin > this.count || begin > end) {
            throw new StringIndexOutOfBoundsException(begin);
        }
        int last = end;
        if (last > this.count) {
            last = this.count;
        }
        int at = last;
        int out = begin;
        while (at < this.count) {
            this.value[out] = this.value[at];
            out = out + 1;
            at = at + 1;
        }
        this.count = out;
        return this;
    }

    /** The code units held so far, as a string. */
    @Override
    public String toString() {
        return new String(this.value, 0, this.count);
    }

    /** The one place code units are copied in, so the bounds check is written once. */
    private StringBuilder appendChars(char[] chars, int offset, int count) {
        if (offset < 0 || count < 0 || offset > chars.length - count) {
            throw new StringIndexOutOfBoundsException(offset);
        }
        reserve(this.count + count);
        int at = 0;
        while (at < count) {
            this.value[this.count + at] = chars[offset + at];
            at = at + 1;
        }
        this.count = this.count + count;
        return this;
    }

    /** Grow the buffer until it holds {@code needed} code units, doubling each time. */
    private void reserve(int needed) {
        if (needed <= this.value.length) {
            return;
        }
        int capacity = this.value.length * 2 + 2;
        if (capacity < needed) {
            capacity = needed;
        }
        char[] grown = new char[capacity];
        int at = 0;
        while (at < this.count) {
            grown[at] = this.value[at];
            at = at + 1;
        }
        this.value = grown;
    }
}
