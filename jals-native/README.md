# jals-native

A **Java package whose implementation is Rust**.

Java has had a word for "the body is not in this class file" since 1.0, and it is `native`. On the
WebAssembly target that word means an **import**: the module declares what it needs, the embedder
supplies it, and no engine instantiates a module whose needs are unmet. This crate is the other
half of that arrangement — what a package *is*, what a binding may do, and how a host selects one.

```rust
use jals_native::{Args, NativeError, NativeHost, NativePackage, NativeValue, Results};

const JAVA: &str = r"
package demo;
public final class Answer {
    public static native int compute();
}
";

let mut package = NativePackage::new("demo", 1);
package.source("demo/Answer.java", JAVA);
package.bind(
    "demo/Answer",
    "compute()I",
    |_host: &mut dyn NativeHost, _args: Args<'_>, mut results: Results<'_>| {
        results.set(0, NativeValue::I32(42));
        Ok::<(), NativeError>(())
    },
);
```

A project takes it in by name:

```toml
[build]
backend = { type = "jals-wasm" }
native-packages = ["demo"]
```

and `demo.Answer.compute()` is then callable from ordinary Java, compiled into the same module.

## One value, both halves

A package holds the Java it publishes **and** the Rust behind that Java's `native` methods. The
alternative — a Java library somewhere and a table of Rust functions registered somewhere else — is
two artifacts that can disagree about a signature. Here they cannot, and the reason is worth
stating precisely because it is the whole design:

- A binding is keyed by the declaring class's **internal name** and the method's **name with its
  JVM descriptor** — `("demo/Answer", "compute()I")`.
- Those are exactly the two strings `jals-javac`'s wasm backend writes into the module's import
  section for the same declaration.
- So a Rust half that spelled the signature differently does not produce a *type* mismatch that
  somebody has to notice. It produces an import nothing satisfies, refused when the module is
  instantiated, listing both spellings.

Nothing re-derives a wasm type from that descriptor either. The runner defines each host function
under the type **the module itself declared** for the import, so the engine's own equality check is
what links them.

## What a binding can do

Read and write Java arrays, call the module's own exports, and hold whatever host state its closure
captured — see [`NativeHost`](src/host.rs).

It cannot **allocate** a Java object: a wasm embedder has no `struct.new` of its own. A native
method that must hand one back declares a `static` factory in the package's own Java and calls it
through `NativeHost::call_export`, so the module allocates and the host computes.

Anything a reference names that is not an array is **opaque**. Its field indices are the backend's
own layout, and a package that read one would be reading a fact no declaration states.

## The host supplies the state

This crate is `no_std` and has no dependencies, so there is no `println!` here and there cannot be
one. A package that writes text is therefore *constructed with* the place its output goes — which
is not a workaround but the shape every stateful package has. `jals.io` is the worked example:

| host | sink |
| --- | --- |
| `jals` | writes through `jals-cli`'s `Shell`, so output lands on stdout and status lines stay on stderr |
| the browser playground | appends to the Run pane |
| this crate's own tests | appends to a `String` (`CapturedConsole`) |

Bindings are `!Send` by construction for the same reason — they capture host state, and every
runtime in this workspace is current-thread.

## Versioning

`NativePackage::new` takes a version, and it is the package author's. It exists for the reason
`jals_frontend::FrontendCaps::version` does: a consumer memoizes a compile against everything it
observed, and the bodies of Rust closures are the one input it cannot observe. Bump it whenever a
binding starts answering differently for input that did not change — otherwise a warm cache serves
the previous answer and the fix is invisible.

## Where the pieces are used

- `jals-config` reads `[build] native-packages`, and refuses a non-empty list under any backend but
  `jals-wasm`: a package's implementation is a host function supplied to a module, and a class file
  has nowhere to put one.
- `jals-build` selects a `NativePackageSet` out of a `NativeRegistry`, compiles its `sources` into
  the module beside the project's own, folds its `provenance` into the backend's cache key, and
  links its `bindings` when the module is instantiated.
- `jals-hir` indexes the same Java under `ItemOrigin::Native`, so the project's own source resolves
  against it and the linter reports nothing about names that are really there.

## What the shipped package looks like

[`jals.io`](java/jals/io/Out.java) declares three `native` methods and writes everything else in
Java on top of them — the seam is three Rust functions and the library is Java. That ratio is the
point, and it is what makes `examples/hello_world_native` print `Hello, world!` from a module that
still has no `String` in it.
