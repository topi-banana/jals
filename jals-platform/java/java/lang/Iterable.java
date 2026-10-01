package java.lang;

import java.util.Iterator;

/**
 * Something a `for`-each can walk.
 *
 * <p>The JDK's one abstract method. A container implements it by handing out an
 * {@link Iterator}, which is an object like any other: a loop calls {@code hasNext} and
 * {@code next} on it, and both calls dispatch over the replayed class that answered — through the
 * interface, with no vtable and no dispatch object, because every implementation is a class the
 * consumer laid out from the library's ABI.
 */
public interface Iterable<E> {

    Iterator<E> iterator();
}
