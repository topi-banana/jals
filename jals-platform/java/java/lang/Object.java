package java.lang;

/**
 * The root of the class hierarchy — declared, never implemented.
 *
 * <p>This is the one type in the platform that has no body and can have none. On the WebAssembly
 * target it <em>is</em> the engine's own {@code anyref}: the backend answers for it before it
 * consults its struct table, so a class that writes no {@code extends} gets no {@code Object}
 * struct prefix. A declared {@code Object} with fields would therefore be one question with two
 * answers — a field that exists on some instances and not others — which is a miscompile with
 * nothing to report it.
 *
 * <p>So it lives in the package's signature tier, and the tier is what enforces it: the compile
 * takes only implementation units, and there is no way to hand it this file. Nothing has to
 * remember the rule.
 *
 * <p>The members here are the ones every reference has by virtue of being one. A subclass that
 * overrides {@code toString} or {@code equals} declares its own, and dispatch finds it.
 */
public class Object {

    /** A textual representation of this object. */
    public String toString();

    /** Whether {@code other} is equal to this object. */
    public boolean equals(Object other);

    /** A hash consistent with {@link #equals}. */
    public int hashCode();
}
