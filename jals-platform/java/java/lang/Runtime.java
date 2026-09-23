package java.lang;

/**
 * The process this program runs in.
 *
 * <p>Signature tier: a wasm module is called and returns, and has no process to halt or count the
 * processors of. This is the record a {@code javac} build's analysis reads.
 */
public class Runtime {

    public static Runtime getRuntime();

    public int availableProcessors();

    public void halt(int status);
}
