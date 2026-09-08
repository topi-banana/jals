package java.lang;

/** A resource a try-with-resources statement closes. */
public interface AutoCloseable {

    /** Release this resource. */
    void close() throws Exception;
}
