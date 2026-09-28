package java.lang;

/**
 * The boxed {@code long}.
 *
 * <p>The same shape as {@link Integer}: a private constructor behind {@code valueOf}, the widening
 * accessors, and a decimal {@code toString} built by the same {@link StringBuilder} overload that
 * {@code "" + l} resolves to. {@code parseLong} is absent for the same reason {@code parseInt} is —
 * a parser with no way to report bad input would have to invent an answer.
 */
public class Long extends Number implements Comparable {

    private long value;

    private Long(long value) {
        this.value = value;
    }

    public static Long valueOf(long l) {
        return new Long(l);
    }

    /** The narrowing conversion, exactly as a cast would do it. */
    public int intValue() {
        return (int) this.value;
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

    public String toString() {
        return new StringBuilder().append(this.value).toString();
    }

    public boolean equals(Object o) {
        if (!(o instanceof Long)) {
            return false;
        }
        return ((Long) o).value == this.value;
    }

    /** The JDK's fold: the two halves of the value, exclusive-ored. */
    public int hashCode() {
        return (int) (this.value ^ (this.value >>> 32));
    }
}
