# hello_world_native

`hello_world_wasm` prints its greeting by spelling it out one call at a time, because a WebAssembly
module has no `String` and no `System.out` of its own. This example is what closes that: the same
greeting, printed.

```toml
[build]
backend = { type = "jals-wasm" }
source-dirs = ["src/main/java"]
```

There is no `native-packages` line, and that is the point. `java.base` is not a package a project
opts into — it is what `java.lang` *is* on this target — so `[build] platform` names it, defaults to
it, and a wasm project gets a `String` and a `System.out` by writing nothing at all. A project that
wants the smallest possible module says `platform = "none"` instead, and gets a target that speaks
only in primitives and arrays.

## Both halves of one declaration

A `native` method compiles to a WebAssembly **import**: the module says what it needs, the embedder
supplies it, and no engine instantiates a module whose needs are unmet. A **package** is one value
holding both halves of that:

| half | what it is | where |
| --- | --- | --- |
| Java | the package's own source, compiled into *this* module beside `Hello.java` | `jals-platform/java/**` |
| Rust | the bodies of the methods that Java declares `native` | `jals-platform/src/bindings.rs` |

The platform declares **twelve** `native` methods and writes everything else in Java on top of them.
Two are on this example's path:

```java
public final class PrintStream implements Closeable {
    private static native void writeUnits(int stream, char[] text, int offset, int count);
    private static native void flushStream(int stream);

    public void println(String text) { put(text); println(); }   // ordinary Java
}
```

So `System.out.println` is Java, compiled into this module, and only the two writes cross the
boundary. So is `Integer.toString`, and so is `Math.sqrt` — reduce, five Newton passes, a Dekker
exact-residual correction, all of it Java. A thing this package can do in Java it does in Java; the
twelve are the operations Java cannot express and a dependency-free `no_std` crate can.

## What the module looks like

```wat
(import "java/io/PrintStream" "writeUnits(I[CII)V" (func (param i32 anyref i32 i32)))
(import "java/io/PrintStream" "flushStream(I)V" (func (param i32)))
```

The two names are derived from the declaration alone: the declaring class's **internal name**, and
the method's **name with its JVM descriptor**. That is what lets the Rust half key its table on the
same two strings without either side restating the other's type mapping — and it makes a signature
the halves disagree about an *unresolved import*, refused at instantiation with both spellings in
hand, rather than a mismatch somebody has to notice.

## Run it

```console
$ jals build
$ jals run --invoke greet
Hello, world!
$ jals run --invoke printNumber -- 42
42
$ jals run --invoke printRoot -- 2
1.4142135623730951
```

`--invoke` names an **export**, because wasm has no entry-point convention and Java's `main` takes a
`String[]` no command line can supply. Every `static` method that is not a constructor is exported;
a library input is deliberately not, so the platform's own `static` methods do not fill the list
`--invoke` offers.

## Still no string literal

`String s = "x"` is refused, which is why the greeting here is a `char[]` wrapped in a `String`, and
why every constant in the platform's own Java looks the same way. That is the one gap left between
this backend and the language on this path. When it closes, this file is one of the diffs that
closes with it.

## Which packages exist is a property of the binary

`jals` ships the platform; the browser playground ships its own set; a program embedding this
toolchain registers whatever it likes. A name none of them offers is reported by the host that holds
the resolver, with the names it does offer — and a name **two** routes offer is refused rather than
shadowed, because one name denotes one package wherever it is read.

A project can add a route of its own with `[packages]`, pointing at a directory of its own `.java`.
That one is Java-only: there is no Rust half to declare, so a `native` method in it is an unresolved
import like any other.

## Where the host's half goes

The Rust bindings write through a `PlatformHost` the host supplies, which is why the same package
serves three of them:

| host | `System.out` | `System.err` | clock |
| --- | --- | --- | --- |
| `jals run` | stdout | stderr | the wall clock, and an `Instant` from start-up |
| the playground | the Run pane | the Run pane | none — both readings are zero |
| the language server | discarded | discarded | none |

A host with no clock is a real host, not a broken one: a language server resolves this Java for
analysis and instantiates nothing.
