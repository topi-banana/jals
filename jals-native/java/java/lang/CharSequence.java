package java.lang;

/**
 * A readable sequence of {@code char} values.
 *
 * <p>The two methods below are the whole interface here, and that is deliberate. Everything else
 * the JDK declares on {@code CharSequence} ({@code subSequence}, {@code chars}, {@code isEmpty})
 * is written in terms of these two, and a default method that returned a {@code CharSequence}
 * would put an interface-typed value where this target holds {@code anyref} — reachable, but for
 * no gain over writing the loop in the class that wanted it.
 */
public interface CharSequence {

    /** How many {@code char} values the sequence holds. */
    int length();

    /** The {@code char} at {@code index}, counting from zero. */
    char charAt(int index);
}
