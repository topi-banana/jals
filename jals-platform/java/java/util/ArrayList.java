package java.util;

/**
 * A {@link List} whose storage is a Rust {@code Vec}.
 *
 * <p>This is a <em>native class</em>: an instance carries an {@code int} handle naming an entry in
 * the host's table, and every method below is a thin wrapper over a {@code native} one that reads
 * or writes the vector behind it. The constructor is what mints the handle, so a list whose
 * constructor did not run carries zero — a handle nothing was stored under, refused the first time
 * a method reads it rather than answered with someone else's list.
 *
 * <p>Elements are <em>retained</em>, not copied: an element is a Java reference, and the host
 * roots it in the engine's collector for as long as the list holds it. That is what lets a
 * {@code get} return the same object a {@code add} was handed, calls apart.
 *
 * <p>Equality follows {@code Object.equals}: an element the class overrides it on compares by
 * value, and one that does not compares by reference — the same dispatch a JVM performs.
 */
public class ArrayList<E> implements List<E> {

    /** The host table entry holding this list's vector. */
    private int handle;

    public ArrayList() {
        this.handle = allocate();
    }

    public int size() {
        return sizeOf(this.handle);
    }

    public boolean isEmpty() {
        return sizeOf(this.handle) == 0;
    }

    public boolean add(E element) {
        return addElement(this.handle, element);
    }

    public void add(int index, E element) {
        if (index < 0 || index > sizeOf(this.handle)) {
            throw new IndexOutOfBoundsException();
        }
        insertElement(this.handle, index, element);
    }

    public E get(int index) {
        if (index < 0 || index >= sizeOf(this.handle)) {
            throw new IndexOutOfBoundsException();
        }
        return getElement(this.handle, index);
    }

    public E set(int index, E element) {
        if (index < 0 || index >= sizeOf(this.handle)) {
            throw new IndexOutOfBoundsException();
        }
        return setElement(this.handle, index, element);
    }

    public E remove(int index) {
        if (index < 0 || index >= sizeOf(this.handle)) {
            throw new IndexOutOfBoundsException();
        }
        return removeElement(this.handle, index);
    }

    public boolean remove(Object element) {
        int index = indexOf(element);
        if (index < 0) {
            return false;
        }
        removeElement(this.handle, index);
        return true;
    }

    public boolean contains(Object element) {
        return indexOf(element) >= 0;
    }

    public int indexOf(Object element) {
        int length = sizeOf(this.handle);
        for (int i = 0; i < length; i = i + 1) {
            Object candidate = getElement(this.handle, i);
            if (element == candidate) {
                return i;
            }
            if (element != null && element.equals(candidate)) {
                return i;
            }
        }
        return -1;
    }

    public Iterator<E> iterator() {
        return new Itr<E>(this);
    }

    private static native int allocate();

    private static native int sizeOf(int handle);

    private static native boolean addElement(int handle, Object element);

    private static native void insertElement(int handle, int index, Object element);

    private static native Object getElement(int handle, int index);

    private static native Object setElement(int handle, int index, Object element);

    private static native Object removeElement(int handle, int index);

    /** The iterator over a list: ordinary Java, holding the list it walks. */
    private static class Itr<E> implements Iterator<E> {

        private ArrayList<E> list;
        private int cursor;

        Itr(ArrayList<E> list) {
            this.list = list;
        }

        public boolean hasNext() {
            return this.cursor < this.list.size();
        }

        public E next() {
            E value = this.list.get(this.cursor);
            this.cursor = this.cursor + 1;
            return value;
        }
    }
}
