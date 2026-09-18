package java.util;

/** A {@link Map} backed by a hash table. */
public class HashMap<K, V> implements Map<K, V> {

    public HashMap();

    public int size();

    public V get(Object key);

    public V put(K key, V value);

    public Set<K> keySet();

    public Collection<V> values();
}
