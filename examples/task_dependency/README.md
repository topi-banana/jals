# Build tasks across a dependency edge

A dependency may declare the same typed task DAG a root project can. `library/` fetches (here:
reads) a JAR and publishes it, and `consumer/` gets the result through an ordinary `path` entry:

```toml
[dependencies]
example = { path = "../library", features = ["sources"] }
```

Supply `library/vendor/example.jar` containing `net/example/Greeter.class` — and, for the `sources`
feature, `net/example/Greeter.java`. A JAR is a binary, so it is generated rather than committed:
`../scripts/gen-vendor-jars.sh` writes one holding both halves, and any JAR of your own with those
two members does as well. Then:

```sh
cd consumer && jals run
```

## What crosses the edge

| Terminal                                 | Consumer receives                                       |
| ---------------------------------------- | ------------------------------------------------------- |
| `add_classpath` / `add_nested_classpath` | Compile classpath and analysis, like a `jar` dependency |
| `publish_tree(…, "navigation")`          | Read-only navigation sources, addressed by package      |
| `publish_tree(…, "compile")`             | Ordinary source-dependency inputs the consumer compiles |

`publish_tree` is **virtual** here. A root project physically replaces its destination directory; a
dependency is an immutable snapshot, so `library/src/main/java/net/example` is never written and the
tree arrives as cache artifacts instead.

This example declares `"navigation"`, and the addressing follows from that: the artifacts arrive as
`net/example/…` — the destination's source root (`src/main/java`) stripped off, which is how
extracted `sources` jars and synthesized skeletons are addressed too, so one type resolves to one
artifact. They are deliberately not compile inputs, because the classpath JAR already defines the
same types and handing `javac` both would be a duplicate-class error rather than better coverage.

`"compile"` is the answer for the other shape, where a published tree is the only carrier of its
package and nothing on the classpath stands behind it. jals says so when that is not what the script
declared: a `"navigation"` root no classpath entry backs is reported against the publication, in the
build of whoever depends on it, rather than left to surface as `package … does not exist` several
layers away.

## Features and caching

Features resolve per package, so `features = ["sources"]` on the `[dependencies]` entry — not the
consumer's own `[features]` — is what turns the publication on. Dropping it leaves the classpath
intact and the sources absent.

Each dependency execution is memoized under the library's identity, its plan, and that resolved
feature set, then re-verified against the cache before reuse: switching features re-runs the plan,
rebuilding without changes does not.

## Failure

A dependency task failure fails the build, deliberately: a missing classpath entry would otherwise
surface much later as unrelated `javac` errors. The diagnostic names the dependency's location
rather than its digest.

`jals build --offline` permits verified cache hits but no task fetch, and the LSP is always offline
— so a dependency whose plan fetches must have been built online at least once before an editor can
analyse the project.

See [`jals-build/README.md`](../../jals-build/README.md#rhai-build-scripts) for the complete task
API, and [`../minecraft`](../minecraft) for a real fetch-remap-decompile
pipeline consumed the same way.
