//! Embedded, signature-only stubs for the most common `java.lang` types.
//!
//! The type analysis indexes only the project's own sources, so any reference to a JDK type
//! ([`String`], [`Object`], …) is otherwise [`External`](crate::ClassTy::External) — known by name
//! but with no members, no supertypes, no inferable method return types. These stubs close the gap
//! for the core of `java.lang`: each is an ordinary Java type declaration carrying *signatures only*
//! (method bodies omitted, `;`-terminated), which [`ProjectIndex::build_with_stdlib`] parses with the
//! real parser and folds into the index as just-another-set-of-files
//! (origin [`Stdlib`](crate::ItemOrigin::Stdlib)).
//!
//! This is the "stubs-as-source" approach: it reuses the whole project-indexing machinery
//! (member lookup, the supertype walk, inference) with no new resolution path. It stays **pure** and
//! `wasm32`-compatible — the stub text is a compile-time constant, parsed in memory, with no I/O.
//!
//! Scope is deliberately small (the names that show up in nearly every file). The stubs carry no
//! generics (type arguments are dropped throughout the MVP) and no implicit `Object` supertype is
//! synthesised for user types — these only make the listed JDK types *visible*, nothing more.

/// The `java.lang` core, as one compilation unit. Top-level types here become `java.lang.<Name>`.
const JAVA_LANG: &str = r#"
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

public interface Iterable {
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
"#;

/// The embedded stub sources, each a self-contained compilation unit. One entry today
/// (`java.lang`); future packages (`java.util`, `java.io`) are added as further entries.
pub(crate) fn stub_sources() -> &'static [&'static str] {
    &[JAVA_LANG]
}
