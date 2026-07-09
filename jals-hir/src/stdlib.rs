//! Embedded, signature-only stubs for the most common `java.lang` and `java.util` types.
//!
//! The type analysis indexes only the project's own sources, so any reference to a JDK type
//! ([`String`], [`List`], …) is otherwise [`External`](crate::ClassTy::External) — known by name
//! but with no members, no supertypes, no inferable method return types. These stubs close the gap
//! for the core of `java.lang` and `java.util`: each is an ordinary Java type declaration carrying
//! *signatures only* (method bodies omitted, `;`-terminated), which
//! [`ProjectIndexBuilder::with_stdlib`] parses with the real parser and folds into the index as
//! just-another-set-of-files (origin [`Stdlib`](crate::ItemOrigin::Stdlib)).
//!
//! This is the "stubs-as-source" approach: it reuses the whole project-indexing machinery
//! (member lookup, the supertype walk, inference, generic substitution) with no new resolution path.
//! It stays **pure** and `wasm32`-compatible — the stub text is a compile-time constant, parsed in
//! memory, with no I/O.
//!
//! Scope is deliberately small (the names that show up in nearly every file), but the generic
//! containers *are* parameterised (`List<E>`, `Map<K, V>`, …), so `List<String>.get(0)` infers
//! `String` through the same member-substitution machinery user generics use. No implicit `Object`
//! supertype is synthesised for user types — the stubs only make the listed JDK types *visible*.
//! In type **checking** a stub type is treated leniently (demoted to external), since its hierarchy
//! and member set are deliberately partial; see [`Ty::is_assignable_to`](crate::Ty::is_assignable_to).

/// The `java.lang` core, as one compilation unit. Top-level types here become `java.lang.<Name>`.
const JAVA_LANG: &str = r"
package java.lang;

public class Object {
    public String toString();
    public boolean equals(Object o);
    public int hashCode();
    public Class getClass();
}

public interface CharSequence {
    public int length();
    public char charAt(int index);
}

public interface Comparable {
    public int compareTo(Object o);
}

public interface Iterable<T> {
}

public class String extends Object implements CharSequence, Comparable {
    public int length();
    public char charAt(int index);
    public boolean isEmpty();
    public String substring(int beginIndex);
    public String substring(int beginIndex, int endIndex);
    public boolean equals(Object o);
    public String toString();
    public int indexOf(int ch);
    public String concat(String s);
}

public class StringBuilder extends Object implements CharSequence {
    public StringBuilder append(String s);
    public StringBuilder append(int i);
    public StringBuilder append(char c);
    public int length();
    public char charAt(int index);
    public String toString();
}

public class Number extends Object {
    public int intValue();
    public long longValue();
    public float floatValue();
    public double doubleValue();
}

public class Integer extends Number {
    public static Integer valueOf(int i);
    public static int parseInt(String s);
    public int intValue();
}

public class Long extends Number {
    public static Long valueOf(long l);
    public static long parseLong(String s);
    public long longValue();
}

public class Double extends Number {
    public static Double valueOf(double d);
    public static double parseDouble(String s);
    public double doubleValue();
}

public class Float extends Number {
    public static Float valueOf(float f);
    public float floatValue();
}

public class Short extends Number {
    public short shortValue();
}

public class Byte extends Number {
    public byte byteValue();
}

public class Character extends Object {
    public char charValue();
}

public class Boolean extends Object {
    public static Boolean valueOf(boolean b);
    public boolean booleanValue();
}

public class Math extends Object {
    public static int max(int a, int b);
    public static int min(int a, int b);
    public static int abs(int a);
    public static double sqrt(double a);
}

public class System extends Object {
    public static long currentTimeMillis();
}

public class Throwable extends Object {
    public String getMessage();
    public String toString();
}

public class Exception extends Throwable {
}

public class RuntimeException extends Exception {
}

public class Error extends Throwable {
}

public class IllegalArgumentException extends RuntimeException {
}

public class NumberFormatException extends IllegalArgumentException {
}

public class IllegalStateException extends RuntimeException {
}

public class NullPointerException extends RuntimeException {
}

public class IndexOutOfBoundsException extends RuntimeException {
}

public class ArrayIndexOutOfBoundsException extends IndexOutOfBoundsException {
}

public class StringIndexOutOfBoundsException extends IndexOutOfBoundsException {
}

public class UnsupportedOperationException extends RuntimeException {
}

public class ClassCastException extends RuntimeException {
}

public class ArithmeticException extends RuntimeException {
}

public class NegativeArraySizeException extends RuntimeException {
}

public class InterruptedException extends Exception {
}

public class CloneNotSupportedException extends Exception {
}
";

/// The `java.util` containers, as one compilation unit. Top-level types here become `java.util.<Name>`.
/// These are the generic ones: their type parameters and the type arguments their members and
/// supertypes carry are indexed, so a use like `List<String>` substitutes `E := String` into `get`
/// (`String`) through the same machinery user generics use. References to `java.lang` types resolve
/// via the implicit `java.lang` import.
const JAVA_UTIL: &str = r"
package java.util;

public interface Iterator<E> {
    public boolean hasNext();
    public E next();
}

public interface Collection<E> extends Iterable<E> {
    public int size();
    public boolean isEmpty();
    public boolean add(E e);
    public boolean remove(Object o);
    public boolean contains(Object o);
    public Iterator<E> iterator();
}

public interface List<E> extends Collection<E> {
    public E get(int index);
    public E set(int index, E element);
    public void add(int index, E element);
    public E remove(int index);
    public int indexOf(Object o);
}

public interface Set<E> extends Collection<E> {
}

public interface Map<K, V> {
    public int size();
    public boolean isEmpty();
    public V get(Object key);
    public V put(K key, V value);
    public V remove(Object key);
    public boolean containsKey(Object key);
    public Set<K> keySet();
    public Collection<V> values();
}

public class ArrayList<E> implements List<E> {
    public int size();
    public boolean isEmpty();
    public boolean add(E e);
    public E get(int index);
    public E set(int index, E element);
    public Iterator<E> iterator();
}

public class HashSet<E> implements Set<E> {
    public int size();
    public boolean add(E e);
    public boolean contains(Object o);
    public Iterator<E> iterator();
}

public class HashMap<K, V> implements Map<K, V> {
    public int size();
    public V get(Object key);
    public V put(K key, V value);
    public Set<K> keySet();
    public Collection<V> values();
}

public class Optional<T> {
    public T get();
    public boolean isPresent();
    public boolean isEmpty();
    public T orElse(T other);
}
";

/// The `java.io` exceptions, as one compilation unit. Only the exception hierarchy is modelled (no
/// streams/readers yet) — enough that a thrown / propagated `IOException` classifies as *checked* and
/// an `UncheckedIOException` as *unchecked* through the [`ProjectIndex::is_subtype`](crate::ProjectIndex)
/// walk. References to `java.lang` supertypes (`Exception`, `RuntimeException`) resolve via the
/// implicit `java.lang` import.
const JAVA_IO: &str = r"
package java.io;

public class IOException extends Exception {
}

public class FileNotFoundException extends IOException {
}

public class UncheckedIOException extends RuntimeException {
}
";

/// The embedded stub sources, each a self-contained compilation unit (`java.lang`, `java.util`, then
/// `java.io`). Later units may reference earlier ones, but build order does not actually matter
/// (members and supertypes are resolved in a second pass over all units); the list is kept in
/// package-dependency order.
pub const fn stub_sources() -> &'static [&'static str] {
    &[JAVA_LANG, JAVA_UTIL, JAVA_IO]
}
