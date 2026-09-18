package java.lang;

/**
 * A native process.
 *
 * <p>Signature tier, for the reason {@link Thread} is: a wasm module has no process to start and
 * none to wait for. It is the record a {@code javac} build's analysis resolves the name through.
 */
public abstract class Process {

    public abstract void destroy();

    public abstract int exitValue();

    public abstract boolean isAlive();

    public abstract int waitFor() throws InterruptedException;
}
