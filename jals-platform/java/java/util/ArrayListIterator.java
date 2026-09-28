package java.util;

/**
 * The cursor {@link ArrayList#iterator} hands out.
 *
 * <p>Package-private, and top level rather than nested: the type a caller names is
 * {@link Iterator}, the only one the surface promises. It holds the list rather than a copy of the
 * elements, so an iterator taken before an `add` sees it — the JDK's iterators are fail-fast about
 * *structural* modification, and this platform's has no modification count to compare, which is a
 * difference a caller can only observe by mutating a list mid-iteration.
 */
class ArrayListIterator<E> extends Object implements Iterator<E> {

    /** The list being walked. */
    private ArrayList<E> list;

    /** The index `next` will return. */
    private int cursor;

    ArrayListIterator(ArrayList<E> list) {
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
