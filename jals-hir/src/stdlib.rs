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
    public Object();
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

// The interface a try-with-resources closes through.
public interface AutoCloseable {
    public void close();
}

public interface Iterable<T> {
    // Qualified because `java.lang` imports nothing: the return type has to name the `java.util`
    // stub outright or it resolves to an external `Iterator` with no descriptor.
    public java.util.Iterator<T> iterator();
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

// The `append` overloads are the whole set a string concatenation lowers to: `a + b` becomes a
// builder chain, and each operand picks the overload its own static type names. A missing one would
// send a `long` to `append(int)`, which compiles and prints the wrong number.
public class StringBuilder extends Object implements CharSequence {
    public StringBuilder();
    public StringBuilder(String s);
    public StringBuilder append(String s);
    public StringBuilder append(Object o);
    public StringBuilder append(boolean b);
    public StringBuilder append(char c);
    public StringBuilder append(int i);
    public StringBuilder append(long l);
    public StringBuilder append(float f);
    public StringBuilder append(double d);
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
    // `int.class` is a `getstatic` of this field, not an `ldc` — a primitive has no `Class` entry.
    public static Class TYPE;
    public static Integer valueOf(int i);
    public static int parseInt(String s);
    public int intValue();
}

public class Long extends Number {
    public static Class TYPE;
    public static Long valueOf(long l);
    public static long parseLong(String s);
    public long longValue();
}

public class Double extends Number {
    public static Class TYPE;
    public static Double valueOf(double d);
    public static double parseDouble(String s);
    public double doubleValue();
}

public class Float extends Number {
    public static Class TYPE;
    public static Float valueOf(float f);
    public float floatValue();
}

public class Void extends Object {
    public static Class TYPE;
}

public class Short extends Number {
    public static Class TYPE;
    public static Short valueOf(short s);
    public short shortValue();
}

public class Byte extends Number {
    public static Class TYPE;
    public static Byte valueOf(byte b);
    public byte byteValue();
}

public class Character extends Object {
    public static Class TYPE;
    public static Character valueOf(char c);
    public char charValue();
}

public class Boolean extends Object {
    public static Class TYPE;
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
    public static java.io.PrintStream out;
    public static java.io.PrintStream err;
    public static long currentTimeMillis();
}

public class Class extends Object {
    public String getName();
    public String getSimpleName();
    public boolean desiredAssertionStatus();
}

public class Throwable extends Object {
    public Throwable();
    public Throwable(String message);
    public String getMessage();
    public String toString();
    // A try-with-resources whose `close()` throws suppresses that rather than losing the body's
    // exception (JLS §14.20.3.1).
    public void addSuppressed(Throwable exception);
    public Throwable[] getSuppressed();
}

public class Exception extends Throwable {
    public Exception();
    public Exception(String message);
}

public class RuntimeException extends Exception {
    public RuntimeException();
    public RuntimeException(String message);
}

public class Error extends Throwable {
}

// `assert` throws one of these, and the two constructors are the two forms the statement has.
public class AssertionError extends Error {
    public AssertionError();
    public AssertionError(Object detailMessage);
}

public class IllegalArgumentException extends RuntimeException {
    public IllegalArgumentException();
    public IllegalArgumentException(String message);
}

public class NumberFormatException extends IllegalArgumentException {
    public NumberFormatException();
    public NumberFormatException(String message);
}

public class IllegalStateException extends RuntimeException {
    public IllegalStateException();
    public IllegalStateException(String message);
}

public class NullPointerException extends RuntimeException {
    public NullPointerException();
    public NullPointerException(String message);
}

public class IndexOutOfBoundsException extends RuntimeException {
    public IndexOutOfBoundsException();
    public IndexOutOfBoundsException(String message);
}

public class ArrayIndexOutOfBoundsException extends IndexOutOfBoundsException {
    public ArrayIndexOutOfBoundsException();
    public ArrayIndexOutOfBoundsException(String message);
}

public class StringIndexOutOfBoundsException extends IndexOutOfBoundsException {
    public StringIndexOutOfBoundsException();
    public StringIndexOutOfBoundsException(String message);
}

public class UnsupportedOperationException extends RuntimeException {
    public UnsupportedOperationException();
    public UnsupportedOperationException(String message);
}

public class ClassCastException extends RuntimeException {
    public ClassCastException();
    public ClassCastException(String message);
}

public class ArithmeticException extends RuntimeException {
    public ArithmeticException();
    public ArithmeticException(String message);
}

public class NegativeArraySizeException extends RuntimeException {
    public NegativeArraySizeException();
    public NegativeArraySizeException(String message);
}

public class InterruptedException extends Exception {
    public InterruptedException();
    public InterruptedException(String message);
}

public class CloneNotSupportedException extends Exception {
    public CloneNotSupportedException();
    public CloneNotSupportedException(String message);
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
    public ArrayList();
    public int size();
    public boolean isEmpty();
    public boolean add(E e);
    public E get(int index);
    public E set(int index, E element);
    public Iterator<E> iterator();
}

public class HashSet<E> implements Set<E> {
    public HashSet();
    public int size();
    public boolean add(E e);
    public boolean contains(Object o);
    public Iterator<E> iterator();
}

public class HashMap<K, V> implements Map<K, V> {
    public HashMap();
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

public class PrintStream extends Object {
    public void println();
    public void println(boolean b);
    public void println(char c);
    public void println(int i);
    public void println(long l);
    public void println(float f);
    public void println(double d);
    public void println(String s);
    public void println(Object o);
    public void print(String s);
    public void print(int i);
    public void print(Object o);
}

public interface Closeable extends AutoCloseable {
    public void close();
}

public class IOException extends Exception {
}

public class FileNotFoundException extends IOException {
}

public class UncheckedIOException extends RuntimeException {
}
";

/// The embedded standard-library signature stubs.
pub(crate) struct Stdlib;

impl Stdlib {
    /// The embedded stub sources, each a self-contained compilation unit (`java.lang`, `java.util`,
    /// then `java.io`). Later units may reference earlier ones, but build order does not actually
    /// matter (members and supertypes are resolved in a second pass over all units); the list is kept
    /// in package-dependency order.
    pub(crate) const fn stub_sources() -> &'static [&'static str] {
        &[JAVA_LANG, JAVA_UTIL, JAVA_IO]
    }
}
