# hello_world_native — a Java package written in Rust

The same greeting as [`hello_world_wasm`](../hello_world_wasm), except that this one **prints it**.

Nothing about the compiler changed. There is still no `String` and still no `System.out`: the
greeting is a `char[]`, exactly as it was next door. What changed is that `jals.toml` selects a
**native package** —

```toml
[build]
backend = { type = "jals-wasm" }
native-packages = ["jals.io"]
```

— and a native package is a Java package whose `native` methods are implemented in Rust.

## What a native package is

One value, in one Rust crate, holding both halves of one declaration:

| half | what it is | where it is |
| --- | --- | --- |
| Java | the package's own source, compiled into *this* module beside `Hello.java` | `jals-native/java/jals/io/Out.java` |
| Rust | the bodies of the methods that Java declares `native` | `jals-native/src/packages/jals_io.rs` |

`jals.io.Out` declares three `native` methods and writes everything else in Java on top of them:

```java
public static native void writeChar(int codeUnit);
public static native void writeChars(char[] text, int offset, int count);
public static native void flush();

public static void println(char[] text) { writeChars(text, 0, text.length); writeChar('\n'); flush(); }
public static void printInt(int value)  { /* digits into a char[], then writeChars */ }
```

That ratio is the point. The seam is three functions; the *library* is Java.

## What the compiler does with `native`

Java has had a word for "the body is not in this class file" since 1.0. On this target it means a
WebAssembly **import**:

```wat
(import "jals/io/Out" "writeChar(I)V" (func (param i32)))
```

The two names are derived from the declaration alone — the declaring class's internal name, and the
method's name with its JVM descriptor. The Rust half registers its implementation under exactly
those two strings, which is why the two halves cannot disagree about a signature: one that spelled
it differently would produce an import nothing satisfies, refused when the module is instantiated
with both spellings listed.

Nothing re-derives a wasm type from that descriptor, either. The runner defines each host function
under the type **the module itself declared** for the import, so the engine's own "these types are
equal" check is what links them.

## Run it

From this directory (or any subdirectory — `jals` discovers `jals.toml` upward, like Cargo):

```sh
# Compile Hello.java *and* the package's Out.java into one module at target/classes/project.wasm.
cargo run -p jals-cli -- build

cargo run -p jals-cli -- run --invoke greet
# → Hello, world!

cargo run -p jals-cli -- run --invoke printNumber -- -2147483648
# → -2147483648      (the digits are built in Java; the host only ever sees a char[])

cargo run -p jals-cli -- run --invoke countTo -- 3
# → 1
# → 2
# → 3

cargo run -p jals-cli -- clean
```

With an installed `jals` binary on `PATH`, drop the `cargo run -p jals-cli --` prefix.

## What the package is *not*

- **Not exported.** `Out.println` is `public static`, and every `static` method of the project is a
  module export — but a package's are not. Otherwise a library's internals would fill the list
  `--invoke` offers, and, because the first export of a name wins and the second is dropped without
  a word, a package method could silently take a project method's export away from it.

  ```sh
  cargo run -p jals-cli -- run --invoke absent
  # → error: the module exports no function named `absent`;
  #          it exports greet, printNumber, countTo
  ```

- **Not available on any other backend.** A package's implementation is a host function supplied to
  a module, and a class file has nowhere to put one — so `native-packages` beside a class-file
  backend is refused by `jals.toml` itself, exactly as `[toolchain] runtime = "wasm"` is.

- **Not something the host can pick without saying so.** Which packages exist is a property of the
  *binary*: `jals` ships `jals.io`, the browser playground ships its own, and a program embedding
  this toolchain registers whatever it likes. A name this build does not offer is reported with the
  names it does.

## Where the host's half of `jals.io` goes

Nowhere in particular — that is the package's decision, and it is why `JalsIo::package` takes the
sink rather than owning one. `jals` writes through the one thing in `jals-cli` allowed to touch a
stream, so the greeting lands on **stdout** while every status line stays on stderr:

```sh
cargo run -p jals-cli -- run --invoke greet 2>/dev/null
# → Hello, world!
```

The browser playground passes a sink that appends to the Run pane, and `jals-native`'s own tests
pass one that appends to a `String`.

See [`jals-native`](../../jals-native) for how to write a second one.
