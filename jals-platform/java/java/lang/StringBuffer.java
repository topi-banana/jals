package java.lang;

/**
 * The synchronized counterpart of {@link StringBuilder}.
 *
 * <p>Signature tier. A wasm module has one thread, so there is nothing for the synchronization this
 * class exists for to guard, and an implementation here would be {@link StringBuilder} under a
 * second name — two answers to what a character buffer is. It is the record a {@code javac} build's
 * analysis resolves the name through, and the JDK behind that build supplies the bodies.
 */
public final class StringBuffer implements CharSequence {

    public StringBuffer();

    public StringBuffer(String initial);

    public StringBuffer append(String text);

    public StringBuffer append(char value);

    public StringBuffer append(int value);

    public StringBuffer append(long value);

    public StringBuffer append(boolean value);

    public StringBuffer append(Object value);

    public StringBuffer reverse();

    public void setLength(int length);

    @Override
    public int length();

    @Override
    public char charAt(int index);

    @Override
    public String toString();
}
