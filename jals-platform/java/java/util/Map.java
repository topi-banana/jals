package java.util;

/** A mapping from keys to values. */
public interface Map<K, V> {

    /** How many entries this holds. */
    int size();

    /** Whether this holds no entries. */
    boolean isEmpty();

    /** The value {@code key} maps to, or {@code null}. */
    V get(Object key);

    /** Map {@code key} to {@code value}; the value it mapped to before, or {@code null}. */
    V put(K key, V value);

    /** Remove {@code key}'s entry; the value it mapped to, or {@code null}. */
    V remove(Object key);

    /** Whether {@code key} has an entry. */
    boolean containsKey(Object key);

    /** This map's keys. */
    Set<K> keySet();

    /** This map's values. */
    Collection<V> values();
}
