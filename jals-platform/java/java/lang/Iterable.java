package java.lang;

/**
 * A sequence a {@code for (T t : xs)} header can walk.
 *
 * <p>Signature tier, and it has to be: its one method returns a {@link java.util.Iterator}, which
 * nothing here implements yet. An interface whose method names a type the module does not declare
 * is not an interface a compile can lower.
 */
public interface Iterable<T> {

    /** A cursor over this sequence. */
    java.util.Iterator<T> iterator();
}
