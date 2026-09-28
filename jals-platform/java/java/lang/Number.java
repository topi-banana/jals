package java.lang;

/**
 * The superclass of the boxed primitives.
 *
 * <p>Nothing constructs one: every value of this type is one of the classes below, and the four
 * accessors are what a caller holding a {@code Number} can ask for without knowing which. They are
 * abstract here because the JDK's are — this class has no storage to widen — and a call through the
 * type dispatches to the subclass that does, whether that call is written in this platform or in a
 * module that links it.
 *
 * <p>Declared here rather than left to the stub for that dispatch's sake: a stub is a name the
 * checker knows, and a receiver of a stub's type has no representation to dispatch on. Declaring
 * the class is what makes {@code Number n = Integer.valueOf(1); n.intValue()} a call with a
 * receiver instead of a compile error.
 */
public abstract class Number extends Object {

    /** Never called directly — a subclass's constructor reaches it — but every class needs one. */
    public Number() {}

    public abstract int intValue();

    public abstract long longValue();

    public abstract float floatValue();

    public abstract double doubleValue();
}
