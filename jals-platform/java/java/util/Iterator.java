package java.util;

/** A cursor over a sequence. Declared so a {@code for (T t : xs)} header resolves. */
public interface Iterator<E> {

    /** Whether {@link #next} would return an element. */
    boolean hasNext();

    /** The next element. */
    E next();
}
