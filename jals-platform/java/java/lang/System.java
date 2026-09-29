package java.lang;

import java.io.PrintStream;

/**
 * The one stream a program starts with.
 *
 * <p>The JDK's other two streams, its properties, its clock and its exit are not here yet;
 * {@code out} is, because a program that cannot print cannot say what it did. It is a genuine
 * {@code static} field initialised by the platform's own code — the class's initialiser runs when
 * the library is instantiated, before any consumer can read the accessor — rather than an object
 * the host would have to allocate, which a wasm embedder cannot do.
 *
 * <p>{@code err} is deliberately absent: two streams writing to one sink is a distinction this
 * target cannot make, and a name that silently aliases the other is worse than a name that is
 * refused.
 */
public class System extends Object {

    /** The standard output stream. */
    public static final PrintStream out = new PrintStream();

    private System() {}
}
