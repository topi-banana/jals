package java.util;

/** A {@link Set} backed by a hash table. */
public class HashSet<E> implements Set<E> {

    public HashSet();

    public int size();

    public boolean add(E e);

    public boolean contains(Object o);

    public Iterator<E> iterator();
}
