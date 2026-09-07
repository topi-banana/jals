package java.lang;

/**
 * The identity of a type, as far as this target has one.
 *
 * <p>Declared for one reason: every wrapper class publishes a {@code static Class TYPE} naming the
 * primitive it wraps, and a field needs a type this backend can lay out. So this is a name and
 * nothing else — there is no {@code forName}, no member lookup, and no {@code .class} literal
 * (the backend refuses one), which is to say there is no reflection here and this class does not
 * pretend otherwise.
 *
 * <p>The instances that exist are exactly the nine {@code TYPE} constants: the eight primitives and
 * {@code void}.
 */
public final class Class {

    /** The name this instance answers with. */
    private final String name;

    /**
     * A type identity named {@code name}.
     *
     * <p>Package-private: the instances are the {@code TYPE} constants, and a project that could
     * build one would be building a type identity for a type nobody declared.
     */
    Class(String name) {
        this.name = name;
    }

    /** The type's name — {@code "int"} for {@code Integer.TYPE}, as the JDK answers. */
    public String getName() {
        return this.name;
    }

    /** The same as {@link #getName}: a primitive's name has no package to strip. */
    public String getSimpleName() {
        return this.name;
    }

    /**
     * Whether assertions are on for this type.
     *
     * <p>Always false, and it is a fact rather than a stub: a wasm host has no start-up moment to
     * read an {@code -ea} at, so the wasm backend decides at *compile* time whether an
     * {@code assert} evaluates its condition and emits no flag for anything to read back. A
     * program that asked this to decide whether to do expensive checking would get the answer that
     * matches what the compiler already did to its {@code assert} statements — off — whenever it
     * asks in a module built by {@code jals build}.
     */
    public boolean desiredAssertionStatus() {
        return false;
    }

    /** {@link #getName}. */
    @Override
    public String toString() {
        return this.name;
    }
}
