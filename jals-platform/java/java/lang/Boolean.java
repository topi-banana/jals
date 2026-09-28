package java.lang;

/**
 * The boxed {@code boolean}.
 *
 * <p>Its {@code toString} is where the two constants of the text world come from: {@code "true"}
 * and {@code "false"} are the words {@code "" + b} renders, and the JDK's {@code hashCode} values
 * are the two primes every hash of a boolean has used since Java 1.0.
 */
public class Boolean extends Object implements Comparable {

    private boolean value;

    private Boolean(boolean value) {
        this.value = value;
    }

    public static Boolean valueOf(boolean b) {
        return new Boolean(b);
    }

    public boolean booleanValue() {
        return this.value;
    }

    public String toString() {
        return this.value ? "true" : "false";
    }

    public boolean equals(Object o) {
        if (!(o instanceof Boolean)) {
            return false;
        }
        return ((Boolean) o).value == this.value;
    }

    /** {@code 1231} for {@code true}, {@code 1237} for {@code false}, as the JDK documents. */
    public int hashCode() {
        return this.value ? 1231 : 1237;
    }
}
