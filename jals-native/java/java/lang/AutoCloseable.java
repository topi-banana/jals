package java.lang;

/**
 * A resource a {@code try}-with-resources statement closes on the way out.
 *
 * <p>{@code throws Exception} is the JDK's signature and is kept, even though the stub this
 * shadows omits it: a stub is read for resolution and a missing {@code throws} costs nothing
 * there, while this declaration is what a project's own {@code catch} clauses are checked against.
 * Dropping it would let a resource whose {@code close()} throws pass unreported.
 */
public interface AutoCloseable {

    /** Release whatever this holds. Called by {@code try}-with-resources, once, on every path. */
    void close() throws Exception;
}
