package java.lang;

/**
 * A type at run time — the shape of one, not the reflection over it.
 *
 * <p>This class exists so {@code int.class} and {@code Foo.class} are expressions that resolve, and
 * so a {@code Class}-typed field or parameter type-checks. There is no {@code forName}, no
 * {@code newInstance}, no member enumeration: reflection needs metadata the backend does not emit
 * and a loader the target does not have.
 */
// `T` is this type's API and not a binding: `Foo.class` has type `Class<Foo>`, and dropping the
// parameter would make every such expression raw. Nothing here can *use* it, because there is no
// reflection member to return one.
@SuppressWarnings("unused-variables")
public final class Class<T> {

    private final String name;

    // Package-private rather than private: each wrapper's `TYPE` is built here, since that field is
    // the JDK's answer to `int.class` and a JVM lowering reads it by name.
    Class(String name) {
        this.name = name;
    }

    /** This type's name. */
    public String getName() {
        return this.name;
    }

    /** This type's name — the same answer as {@link #getName} on this target. */
    public String getSimpleName() {
        return this.name;
    }

    @Override
    public String toString() {
        return this.name;
    }
}
