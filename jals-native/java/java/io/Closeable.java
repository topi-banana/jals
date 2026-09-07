package java.io;

/**
 * A resource whose closing may fail with an {@link IOException}.
 *
 * <p>The narrowing is the whole point of this interface existing beside {@link AutoCloseable}: a
 * {@code try}-with-resources over a {@code Closeable} needs to catch an {@code IOException} and
 * not an {@code Exception}, and the {@code throws} clause is where that is written.
 */
public interface Closeable extends AutoCloseable {

    /** Release whatever this holds. */
    @Override
    void close() throws IOException;
}
