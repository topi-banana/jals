package java.lang;

/**
 * A mutable sequence of UTF-16 code units.
 *
 * <p>The builder exists on this target for one reason the JDK shares: concatenation. {@code a + b}
 * is lowered by the compiler into a chain of {@code append} calls and one {@code toString}, and the
 * overload on each {@code append} is what gives the operand its rendering — a {@code long} goes to
 * {@code append(long)} and comes out as a {@code long}, not as a truncated {@code int}. The set of
 * overloads below is therefore load-bearing: a missing one is a wrong number, not a missing method.
 *
 * <p>The representation is a {@code char[]} that grows by doubling, plus a count. That is also the
 * JDK's, and it is the one part of the JDK's builder worth copying exactly: {@code toString} copies
 * the used prefix and nothing else, so a builder that once held a large string costs nothing to keep
 * afterwards.
 *
 * <p>What is <em>not</em> here yet: {@code append(Object)}, which needs the object's
 * {@code toString} dispatched at run time — and the stub {@code Object} declares no default to
 * dispatch to — so the call is refused by name rather than appended by identity.
 */
public class StringBuilder extends Object implements CharSequence {

    /** The buffer. Capacity beyond {@link #count} is uninitialised for every purpose that matters. */
    private char[] value;

    /** How much of {@link #value} is used. */
    private int count;

    /** An empty builder with room to grow. */
    public StringBuilder() {
        this.value = new char[16];
    }

    /** A builder holding a copy of {@code s}. */
    public StringBuilder(String s) {
        this.value = new char[s.length() + 16];
        this.count = s.length();
        int i = 0;
        while (i < this.count) {
            this.value[i] = s.charAt(i);
            i = i + 1;
        }
    }

    /** How many code units the builder holds. */
    public int length() {
        return this.count;
    }

    /** The code unit at {@code index}. */
    public char charAt(int index) {
        return this.value[index];
    }

    /** Append every code unit of {@code s}. */
    public StringBuilder append(String s) {
        int length = s.length();
        this.ensure(length);
        int i = 0;
        while (i < length) {
            this.value[this.count + i] = s.charAt(i);
            i = i + 1;
        }
        this.count = this.count + length;
        return this;
    }

    /** Append {@code c}. */
    public StringBuilder append(char c) {
        this.ensure(1);
        this.value[this.count] = c;
        this.count = this.count + 1;
        return this;
    }

    /** Append {@code true} or {@code false}, which is what the JDK appends. */
    public StringBuilder append(boolean b) {
        if (b) {
            return this.append("true");
        }
        return this.append("false");
    }

    /** Append {@code i} in decimal. */
    public StringBuilder append(int i) {
        return this.appendLong(i);
    }

    /** Append {@code l} in decimal. */
    public StringBuilder append(long l) {
        return this.appendLong(l);
    }

    /** Append {@code d} as {@link Double#toString(double)} renders it. */
    public StringBuilder append(double d) {
        return this.append(Double.toString(d));
    }

    /** Append {@code f} as {@link Float#toString(float)} renders it. */
    public StringBuilder append(float f) {
        return this.append(Float.toString(f));
    }

    /** A new string with the builder's code units, copied out of the buffer. */
    public String toString() {
        char[] copy = new char[this.count];
        int i = 0;
        while (i < this.count) {
            copy[i] = this.value[i];
            i = i + 1;
        }
        return new String(copy);
    }

    /** Make room for {@code extra} more code units, growing the buffer if it has to. */
    private void ensure(int extra) {
        if (this.count + extra <= this.value.length) {
            return;
        }
        int capacity = this.value.length * 2 + 2;
        while (capacity < this.count + extra) {
            capacity = capacity * 2;
        }
        char[] bigger = new char[capacity];
        int i = 0;
        while (i < this.count) {
            bigger[i] = this.value[i];
            i = i + 1;
        }
        this.value = bigger;
    }

    /** Append {@code length} code units of {@code chars} from {@code offset}. */
    private StringBuilder append(char[] chars, int offset, int length) {
        this.ensure(length);
        int i = 0;
        while (i < length) {
            this.value[this.count + i] = chars[offset + i];
            i = i + 1;
        }
        this.count = this.count + length;
        return this;
    }

    /**
     * Append {@code value} in decimal.
     *
     * <p>The digits are taken from the *negative* side of zero: {@code -Long.MIN_VALUE} does not fit
     * a {@code long}, so negating a positive value first would be wrong for exactly one input, while
     * every negative value's digits come out of {@code %} and {@code /} unchanged (Java truncates
     * toward zero, so {@code -7 % 10} is {@code -7 + 3} — the digit's sign, not its value). Written
     * out rather than delegated because the delegate would be {@code Long.toString}, which returns a
     * {@code String}, which allocates.
     */
    private StringBuilder appendLong(long value) {
        if (value == 0) {
            return this.append('0');
        }
        char[] digits = new char[20];
        int at = digits.length;
        boolean negative = value < 0;
        long rest = value;
        if (!negative) {
            rest = -value;
        }
        while (rest != 0) {
            at = at - 1;
            digits[at] = (char) ('0' - (rest % 10));
            rest = rest / 10;
        }
        if (negative) {
            at = at - 1;
            digits[at] = '-';
        }
        return this.append(digits, at, digits.length - at);
    }
}
