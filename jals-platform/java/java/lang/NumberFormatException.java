package java.lang;

/**
 * A string did not hold the number it was parsed as.
 *
 * <p>This is the exception {@link Integer#parseInt} and {@link Long#parseLong} throw, and the
 * reason those methods could not exist before it: a parser with no way to report bad input would
 * have had to invent an answer.
 */
public class NumberFormatException extends IllegalArgumentException {

    public NumberFormatException() {
        super();
    }

    public NumberFormatException(String message) {
        super(message);
    }

    protected String className() {
        return "java.lang.NumberFormatException";
    }
}
