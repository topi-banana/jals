# jals-native

**What a Java package is, and how one is found.**

Java has had a word for "the body is not in this class file" since 1.0, and it is `native`. On the
WebAssembly target that word means an **import**: the module declares what it needs, the embedder
supplies it, and no engine instantiates a module whose needs are unmet. This crate is the other half
of that arrangement — what a package *is*, what a binding may do, and how a name becomes one.

It ships **no Java of its own**, not even `java.lang`. The platform is a package like any other
([`jals-platform`](../jals-platform)), built from this crate's vocabulary. A standard library
privileged into the crate that defines what a package *is* would be a second way to publish Java,
and the second way is always the one that drifts.

No features, and no dependencies, so a package author's crate depends on this one and on nothing
else.

## One value, both halves

A **package** is one value holding the Java it publishes and the Rust behind that Java's `native`
methods. A binding is keyed by the declaring class's **internal name** and the method's **name with
its descriptor** — `("jals/io/Out", "writeChars([CII)V")` — which are exactly the two strings the
wasm backend writes into the import section for that declaration. So a Rust half that spells a
signature differently does not produce a type mismatch somebody has to notice: it produces an import
nothing satisfies, refused at instantiation with both spellings listed.

Nothing re-derives a wasm type from a descriptor either. The runner defines each host function under
the `FuncType` **the module itself declared**, which makes the engine's own equality check the link.

## Two kinds of Java, and why that is not a fidelity

`SourceKind` says whether a published unit carries bodies. It is a fact about the text, and it stops
there.

How an *index* should read that text is a different question with a different answer, and it belongs
to whoever knows what the build links: the same `String.java` is the code that will run for a project
compiling it into its own module, and a record of a JDK for every other project. That answer is
`jals_hir::LibraryFidelity`, deliberately not a second enum here. **A package author states what
they wrote, never how somebody else's build should treat it** — which is what lets one text serve
both, and what replaced a hand-written signature copy sitting beside an implementation with a test
diffing their member sets.

## Declaring one

`java_package!` takes both halves at once — deno_core's `extension!` is the model, and the reason is
the same in both languages: a text list in one file and a binding table in another are two lists a
reviewer has to read together, and one of them is always the one nobody updated.

```rust,ignore
java_package! {
    /// A package with one host function.
    pub Demo {
        name: "demo.clock",
        version: 1,
        host: dyn Clock,
        root: "java",
        signatures: ["demo/Marker.java"],
        implementation: ["demo/Clock.java"],
        bind: Demo::install,
    }
}
```

The generated type carries `NAME`, `VERSION`, `package(host)`, and — load-bearing — **`SOURCES`,
reachable with no host constructed at all**. Indexing a package needs the Java and nothing else,
which is what a language server that instantiates nothing has.

## Registering a native class

A Java class whose instances carry state a `Vec` (or a table, or a cursor) lives in Rust uses the
typed registration, which is this crate's answer to rhai's `register_type_with_name`/`register_fn`:

```rust,ignore
package
    .native_class::<Vec<i32>>("demo/Counter")
    .allocate("allocate()I", |_| Ok(Vec::new()))
    .method("add(I)V", |list, _host, args, _results| {
        list.push(args.i32(0)?);
        Ok(())
    })
    .release("release(I)V");
```

The convention the primitives share: the Java declaration owns an `int` field, a `static native int
allocate()` fills it, and every other `native` method is a `static` one whose first parameter is that
handle. The framework reads the handle, hands the closure `&mut T`, and puts the object back whatever
the closure answered. The Java half is still written by hand — the type layer binds to it, never
generates it.

## What outlives one call

A reference argument is a `RefSlot`: an index into the references the host made live for **one**
call, structurally useless in another. A package that must keep something longer asks the host to
hold it in a table whose lifetime is the **run**'s:

| method | what it keeps |
| --- | --- |
| `object_store` / `object_take` / `object_restore` / `object_drop` | a Rust object behind an `int` handle |
| `reference_retain` / `reference_restore` / `reference_release` | a Java reference, rooted in the engine's collector |

`HostValue` is the shape such a value takes in a package's own `Vec`: a number, a null, or a
retained reference. `HostObjects` adds the typed `put`/`take`/`put_back` on top, so a wrong Rust type
is a refusal naming both rather than a reinterpretation. The table is dropped with the run, so a
handle left over from another run is `UnknownHandle` rather than a stale pointer.

A binding can also read and write Java arrays and call the module's own exports. It **cannot
allocate** a Java object — a wasm embedder has no `struct.new` of its own — so a `native` method that
must hand one back calls a `static` factory the package's Java declares, or returns an object it was
handed.

Bindings are `!Send` by construction, because the state they capture is the host's. That is why
`WasmTestLauncher::run` runs its cases in order when a package is linked and fans out only when none
is.

## Two routes, one selection

| route | what it offers | who builds it |
| --- | --- | --- |
| `PackageRegistry` | what a binary was compiled with | each host, over its own state |
| yours | whatever you like | `PackageRegistry::add` |

A host resolves `Manifest::package_names` through the registry — all-or-nothing for a compile,
`select_reporting` (keep what resolved) for an analysis host — and gets a `PackageSelection`. That
one value answers two questions about the same text: `analysis_sources` is everything an index reads,
`link_sources` is the units a linking compile lowers, and `bindings` is what the runner links a
module's imports against.

## Versioning

`JavaPackage::new` takes a version and it is the **package author's**, for the reason
`jals_frontend::FrontendCaps::version` exists: a consumer memoizes a compile against everything it
observed, and a Rust closure's body is the one input it cannot observe. Bump it whenever a binding
starts answering differently for input that did not change.

## Held honest by its tests

`cargo hawk check` excludes this crate the way it excludes `jinja`: its API is sized by what a
*package author* is offered, not by what this workspace's packages happen to call. `tests/package.rs`
is what sizes it instead — a published item lands with the test that drives it, or it lands
unreachable with nothing reporting it.
