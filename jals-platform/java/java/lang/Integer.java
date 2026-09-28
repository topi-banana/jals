package java.lang;

/**
 * The boxed {@code int}.
 *
 * <p>{@code valueOf} is the way in — the constructor is private, so a caller gets the method the
 * JDK documents rather than the constructor it happens to have — and the {@code Number} accessors
 * widen: {@code longValue} is exact, and {@code floatValue} and {@code doubleValue} round to
 * nearest as the JLS says.
 *
 * <p>{@code parseInt} is deliberately absent. A parser that cannot throw
 * {@code NumberFormatException} cannot report bad input, and one that returned {@code 0} for
 * {@code "x"} would be worse than one that does not exist: today the call is refused with the name
 * it asked for, which is a compile error at the line that needs it. The method lands with the
 * exception model. {@code TYPE} is absent for a reason of the same shape: this platform has no
 * {@code Class} value to put in it, and a field that could only ever hold {@code null} is a promise
 * no code can keep.
 */
public class Integer extends Number implements Comparable {

    private int value;

    private Integer(int value) {
        this.value = value;
    }

    /** The box, for the widening conversions a call site used to do implicitly. */
    public static Integer valueOf(int i) {
        return new Integer(i);
    }

    public int intValue() {
        return this.value;
    }

    public long longValue() {
        return this.value;
    }

    public float floatValue() {
        return this.value;
    }

    public double doubleValue() {
        return this.value;
    }

    /** The decimal text — the same one a {@code "" + i} builds, by the same builder. */
    public String toString() {
        return new StringBuilder().append(this.value).toString();
    }

    /** Whether {@code o} is an {@code Integer} holding the same {@code int}. */
    public boolean equals(Object o) {
        if (!(o instanceof Integer)) {
            return false;
        }
        return ((Integer) o).value == this.value;
    }

    /** The value itself, which is the JDK's definition. */
    public int hashCode() {
        return this.value;
    }
}
