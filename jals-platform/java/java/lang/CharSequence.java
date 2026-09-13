package java.lang;

/** A readable sequence of {@code char} values. */
public interface CharSequence {

    /** How many {@code char} values this holds. */
    int length();

    /** The {@code char} at {@code index}. */
    char charAt(int index);
}
