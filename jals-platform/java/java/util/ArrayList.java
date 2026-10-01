package java.util;

/**
 * A {@link List} over a growable array.
 *
 * <p>The implementation is the JDK's shape at the size a build script needs: an `Object[]` that
 * grows by half again, a count of the slots in use, and an iterator that walks indices. Capacity is
 * an implementation detail — the interface's surface is the public one — except for the constructor
 * that takes one, which exists because a caller who knows the count should not pay for the growth.
 *
 * <p>Elements are stored erased because the JVM's erasure is this target's too: `E` is `Object` by
 * the time a slot is written, and a use at a concrete type comes back down with the `ref.cast` the
 * caller's own erasure inserts. What that costs is what it costs on the JVM; what it buys is one
 * `ArrayList` for every instantiation.
 *
 * <p>Two things the JDK's class has are deliberately not here. Element-comparing methods
 * (`contains`, `indexOf`, `remove(Object)`) need `equals` on an element that may be a consumer's
 * object, which is the library-to-consumer dispatch this platform cannot make yet. And
 * `toString`, `equals` and `hashCode` are not overridden: each would dispatch on the elements the
 * same way, and a `toString` that printed object identities while promising a list would be a worse
 * answer than the `Object` one the class inherits.
 */
public class ArrayList<E> extends Object implements List<E> {

    /** The slots, never null; entries past {@link #size} are null. */
    private Object[] elements;

    /** How many slots are in use. */
    private int size;

    public ArrayList() {
        this.elements = new Object[10];
    }

    /**
     * An empty list with room for {@code initialCapacity} elements before it grows.
     *
     * <p>A negative capacity is illegal, as the JDK says it is, and the exception is this
     * platform's own class — the first library method to throw one it declared itself.
     */
    public ArrayList(int initialCapacity) {
        if (initialCapacity < 0) {
            throw new IllegalArgumentException("Illegal Capacity: " + initialCapacity);
        }
        this.elements = new Object[initialCapacity];
    }

    public int size() {
        return this.size;
    }

    public boolean isEmpty() {
        return this.size == 0;
    }

    public boolean add(E element) {
        this.ensure(this.size + 1);
        this.elements[this.size] = element;
        this.size = this.size + 1;
        return true;
    }

    public void add(int index, E element) {
        if (index < 0 || index > this.size) {
            throw new IndexOutOfBoundsException("Index: " + index + ", Size: " + this.size);
        }
        this.ensure(this.size + 1);
        int i = this.size;
        while (i > index) {
            this.elements[i] = this.elements[i - 1];
            i = i - 1;
        }
        this.elements[index] = element;
        this.size = this.size + 1;
    }

    public E get(int index) {
        this.check(index);
        return this.elements[index];
    }

    public E set(int index, E element) {
        this.check(index);
        E previous = this.elements[index];
        this.elements[index] = element;
        return previous;
    }

    public void clear() {
        int i = 0;
        while (i < this.size) {
            this.elements[i] = null;
            i = i + 1;
        }
        this.size = 0;
    }

    public Iterator<E> iterator() {
        return new ArrayListIterator(this);
    }

    /** The index is in range, or the exception the JDK's contract names. */
    private void check(int index) {
        if (index < 0 || index >= this.size) {
            throw new IndexOutOfBoundsException("Index: " + index + ", Size: " + this.size);
        }
    }

    /** Room for at least {@code capacity} elements, growing by half again. */
    private void ensure(int capacity) {
        if (capacity <= this.elements.length) {
            return;
        }
        int grown = this.elements.length + this.elements.length / 2 + 1;
        if (grown < capacity) {
            grown = capacity;
        }
        Object[] next = new Object[grown];
        int i = 0;
        while (i < this.size) {
            next[i] = this.elements[i];
            i = i + 1;
        }
        this.elements = next;
    }
}
