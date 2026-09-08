# jals-native

**What a Java package is, and how one is found.**

Java has had a word for "the body is not in this class file" since 1.0, and it is `native`. On the
WebAssembly target that word means an **import**: the module declares what it needs, the embedder
supplies it, and no engine instantiates a module whose needs are unmet. This crate is the other half
of that arrangement — what a package *is*, what a binding may do, and how a name becomes one.

It ships **no Java of its own**, not even `java.lang`. The platform is a package like any other
([`jals-platform`](../jals-platform)), found through the same chain a third party's is. A standard
library privileged into the crate that defines what a package *is* would be a second way to publish
Java, and the second way is always the one that drifts.

No features, and no dependencies, so a package author's crate depends on this one and on nothing
else.

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

## One value, both halves

A binding is keyed by the declaring class's **internal name** and the method's **name with its
descriptor** — `("jals/io/Out", "writeChars([CII)V")` — which are exactly the two strings the wasm
backend writes into the import section for that declaration. So a Rust half that spells a signature
differently does not produce a type mismatch somebody has to notice: it produces an import nothing
satisfies, refused at instantiation with both spellings listed.

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

## Three routes, one chain

`PackageResolver` is rhai's `ModuleResolver`, and `ResolverChain` its `ModuleResolversCollection`:

| route | what it offers | who builds it |
| --- | --- | --- |
| `StaticResolver` | what a binary was compiled with | each host, over its own state |
| `SourceResolver` | what a *project* declared in `[packages]` — Java only | `jals_editor::packages::ProjectPackages`, out of a `ProjectView` |
| yours | whatever you like | implement the trait |

What is **not** borrowed from rhai is shadowing. rhai lets an earlier resolver win a name a later one
also offers; here a name two routes offer is an **error**, with both routes named. The reason is that
a silent shadow would be one project's analysis and that same project's linked module disagreeing
about a type with nothing said — and it is `jals-config`'s own rule for a dependency named in two
tables: one name denotes one entry wherever it is read.

A `SourceResolver` package binds nothing, and needs no check saying so. A `native` method in Java
nobody wrote Rust for is an unresolved import, refused exactly where every unbound import is. One
mechanism, not two.

## What a binding can do

Read and write Java arrays, call the module's own exports, and hold whatever host state its closure
captured. It **cannot allocate** a Java object — a wasm embedder has no `struct.new` of its own — so
a `native` method that must return one calls a `static` factory the package's Java declares, and one
that must return text writes into an array the module allocated.

Bindings are `!Send` by construction, because the state they capture is the host's. That is why
`WasmTestLauncher::run` runs its cases in order when a package is linked and fans out only when none
is.

## Versioning

`JavaPackage::new` takes a version and it is the **package author's**, for the reason
`jals_frontend::FrontendCaps::version` exists: a consumer memoizes a compile against everything it
observed, and a Rust closure's body is the one input it cannot observe. Bump it whenever a binding
starts answering differently for input that did not change.

`SourceResolver::declare` takes **no** version, and that is not an omission — a package declared that
way has no closures at all, so `describe`'s fold over every path and body is already complete, and a
number beside it would be a second identity that could disagree with the first.

## Held honest by its tests

`cargo hawk check` excludes this crate the way it excludes `jinja`: its API is sized by what a
*package author* is offered, not by what this workspace's packages happen to call. `tests/package.rs`
is what sizes it instead — a published item lands with the test that drives it, or it lands
unreachable with nothing reporting it.
