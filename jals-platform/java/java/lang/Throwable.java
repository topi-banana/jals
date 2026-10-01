package java.lang;

/**
 * The superclass of everything a `throw` can carry.
 *
 * <p>One wasm tag carries every Java exception and its payload is the thrown *reference*, so what a
 * `catch` clause tests is this object's class — which is why the classes in this file exist at all.
 * Before them a `throw` of anything the platform did not declare was a compile error naming a type
 * with no wasm representation; now a consumer can construct one through the linked factory, throw
 * it, catch it by class, and read its message back.
 *
 * <p>{@code toString} is the JDK's shape — the class's name, then {@code ": "} and the message when
 * there is one — but the name comes from an overridable method rather than from
 * {@code getClass().getName()}, because this platform has no {@code Class} value to ask. Every
 * class below answers with its own name; the virtual call is the same one the JDK makes, with the
 * one substitution the target forces.
 *
 * <p>{@code addSuppressed} and {@code getSuppressed} are absent: the stub declares them for a
 * try-with-resources whose {@code close()} throws, and this platform's try-with-resources swallows
 * that exception rather than recording it. A method that could never be given a value is not
 * declared, so a call to one is a compile error at the line that needs it.
 */
public class Throwable extends Object {

    /** The message, or {@code null} when the constructor was given none. */
    private String message;

    public Throwable() {}

    public Throwable(String message) {
        this.message = message;
    }

    /** This class's own name, the thing {@code toString} leads with. */
    protected String className() {
        return "java.lang.Throwable";
    }

    public String getMessage() {
        return this.message;
    }

    public String toString() {
        if (this.message == null) {
            return this.className();
        }
        return this.className() + ": " + this.message;
    }
}
