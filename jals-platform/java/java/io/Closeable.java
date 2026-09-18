package java.io;

/** A stream a try-with-resources statement closes. */
public interface Closeable extends AutoCloseable {

    /** Release this stream. */
    @Override
    void close();
}
