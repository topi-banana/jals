package java.util;

/** A {@link List} backed by an array. */
public class ArrayList<E> implements List<E> {

    public ArrayList();

    public int size();

    public boolean isEmpty();

    public boolean add(E e);

    public E get(int index);

    public E set(int index, E element);

    public Iterator<E> iterator();
}
