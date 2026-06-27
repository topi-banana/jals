import java.util.List;

public interface Iface<E> extends Comparable<Iface<E>> {
    E get(int index);

    default int size() {
        return 0;
    }

    default List<E> asList() {
        return null;
    }
}
