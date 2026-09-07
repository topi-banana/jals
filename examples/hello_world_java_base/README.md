# hello_world_java_base — the JDK module a wasm host does not have

[`hello_world_native`](../hello_world_native) prints its greeting through `jals.io`, a native
package of three `native` methods. This one prints it through `System.out`, because it selects a
native package of **fifty-two Java files and ten host functions**:

```toml
[build]
backend = { type = "jals-wasm" }
native-packages = ["java.base"]
```

Nothing in `Greeting.java` imports anything. `String`, `StringBuilder`, `Integer`, `Double`,
`Math`, `System` and `NumberFormatException` are all `java.lang`, which every Java file imports
without saying so — and what makes them *work* is the line above.

## What the package supplies

`jals-hir` ships signature-only stubs for `java.lang` and `java.io` so that a reference to `String`
resolves in an editor. They are bones: no bodies, and nothing to run. The wasm backend says so in as
many words when a compile reaches one — *"a wasm host has no `java.base` to supply the rest"*.

[`java.base`](../../jals-native/src/packages/java_base.rs) is that `java.base`, and the ratio is the
point:

| half | what it is |
| --- | --- |
| Java | `String`, `StringBuilder`, every wrapper, `Math`, `System`, `PrintStream`, the whole `Throwable` hierarchy — [52 files](../../jals-native/java/java), compiled into *this* module |
| Rust | ten host functions: the `double`/`float` bit casts and decimal conversions, the clock, and the two `PrintStream` writes |

`Math.sqrt` is in the Java half, and it is exact: Newton's iteration over a reduced mantissa with an
exact-residual correction, with only `Double.doubleToRawLongBits` coming from the host.

## Run it

From this directory (or any subdirectory — `jals` discovers `jals.toml` upward, like Cargo):

```sh
cargo run -p jals-cli -- build

cargo run -p jals-cli -- run --invoke greet
# → Hello, world!

cargo run -p jals-cli -- run --invoke describe -- 2
# → 2 -> 1.4142135623730951

cargo run -p jals-cli -- run --invoke parseFailure
# → java.lang.NumberFormatException: 12x      (on stderr — it is a `System.err` write)

cargo run -p jals-cli -- clean
```

With an installed `jals` binary on `PATH`, drop the `cargo run -p jals-cli --` prefix.

`System.out` lands on **stdout** and `System.err` on stderr, exactly as a `java` child's would:

```sh
cargo run -p jals-cli -- run --invoke greet 2>/dev/null
# → Hello, world!
cargo run -p jals-cli -- run --invoke parseFailure 2>/dev/null
# → (nothing: the failure was written to stderr)
```

## The one thing that still looks unusual

```java
private static final char[] HELLO = {'H', 'e', 'l', 'l', 'o', ',', ' ', 'w', 'o', 'r', 'l', 'd', '!'};
```

A **string literal** is not compiled to wasm yet, and neither is an **autoboxing** conversion — so a
constant string is spelled as a `char[]` and wrapped once, and a wrapper is reached by writing
`Integer.valueOf(1)` rather than `Integer n = 1`. Both are gaps in the backend rather than in the
package: the types are there and the lowering is what is missing, which is why
`jals-build/tests/java_base.rs` pins them as *failures* — the day either is closed, that test fails
and says which line to delete.

Everything after the `char[]` is ordinary Java.

## What is deliberately not in the package

- **`java.lang.Object`.** It is the backend's `anyref` — the top of wasm's reference hierarchy — so
  giving it a struct type as well would be one question with two answers.
- **`Enum`, `Record`, `Iterable`, reflection.** The first two are supertypes the compiler
  synthesises; `Iterable` needs `java.util.Iterator`; reflection needs a runtime that reads
  metadata, and there is none.
- **`Math`'s transcendentals and full Unicode case mapping.** `jals-native` is dependency-free by
  design, so there is no rounded elementary-function library to reach for, and a hand-rolled series
  is a wrong answer that looks like a right one.

See [`jals-native`](../../jals-native) for how to write a package of your own.
