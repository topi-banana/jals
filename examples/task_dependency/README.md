# Build tasks across a dependency edge

A dependency may declare the same typed task DAG a root project can. `library/` fetches (here:
reads) a JAR and publishes it, and `consumer/` gets the result through an ordinary `path` entry:

```toml
[dependencies]
example = { path = "../library", features = ["sources"] }
```

Supply `library/vendor/example.jar` containing `net/example/Greeter.class` — and, for the `sources`
feature, `net/example/Greeter.java` — then:

```sh
cd consumer && jals run
```

## What crosses the edge

| Terminal | Consumer receives |
| --- | --- |
| `add_classpath` / `add_nested_classpath` | Compile classpath and analysis, like a `jar` dependency |
| `publish_tree` | Read-only navigation sources, addressed by package |

`publish_tree` is **virtual** here. A root project physically replaces its destination directory; a
dependency is an immutable snapshot, so `library/src/main/java/net/example` is never written and the
tree arrives as cache artifacts addressed `net/example/…` — the destination's source root
(`src/main/java`) stripped off, which is how extracted `sources` jars and synthesized skeletons are
addressed too, so one type resolves to one artifact.

Those sources are deliberately not compile inputs. The classpath JAR already defines the same types;
handing `javac` both would be a duplicate-class error, not better coverage.

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
API, and [`../minecraft-mojang-remap`](../minecraft-mojang-remap) for a real fetch-remap-decompile
pipeline consumed the same way.
