# Minecraft 1.21.1 Mojang-mappings remapped sources

This example uses the build-task DAG to:

1. Fetch Minecraft **1.21.1** version metadata (pinned SHA-1).
2. Resolve the selected jar + official Mojang mappings through JSON projections.
3. For the server bundler jar, extract `META-INF/versions/1.21.1/server-1.21.1.jar` and flatten
   nested library jars onto the classpath with `add_nested_classpath`.
4. **Remap** the jar with those mappings (member paths renamed to official names).
5. **Decompile** every class under `net/minecraft` into compile-oriented Java skeletons.
6. Publish the tree at `src/main/java/net/minecraft` (`replace-root`).

## Side selection

The distribution comes from the `[build.features]` declared in `jals.toml` (`default = ["server"]`).
Selection is **additive**, exactly like Cargo: a feature never subtracts, so `--features client`
keeps the default `server` and therefore builds the *merged* jar. Drop `server` with
`--no-default-features`.

| selection | resolved features | behaviour |
|---|---|---|
| (none) | `server` | server bundler → nested game jar + server mappings |
| `--features client` | `server`, `client` | remap both jars, then `merge_jars(server, client)` |
| `--features server,client` | `server`, `client` | same as above |
| `--all-features` | `server`, `client` | same as above |
| `--no-default-features --features client` | `client` | client jar + client mappings only |

`merge_jars` overlays the client onto the server, so the client wins path conflicts. A client-only
build never enters the server bundler branch, so `add_nested_classpath` is skipped and the bundled
libraries (brigadier, guava, netty, …) are absent from its compile classpath.

```sh
# First run downloads ~50 MiB and then remaps + decompiles (slow).
cargo run -p jals-cli -- build

# Merged: the client overlaid on the server.
cargo run -p jals-cli -- build --features client

# Client only.
cargo run -p jals-cli -- build --no-default-features --features client

# Subsequent runs reuse the verified SHA-256 project cache.
cargo run -p jals-cli -- build --offline

cargo run -p jals-cli -- clean   # removes the owned publication root too
```

## What it demonstrates

- `tasks.fetch_json` / `fetch_jar` / `fetch_text` with mandatory HTTPS + digest + byte cap.
- `tasks.json_url` / `json_sha1` / `json_u64` projections over Mojang version metadata.
- `tasks.nested_jar(jar, member)` — pull the game jar out of the server bundler.
- `tasks.add_nested_classpath(jar)` — flatten every nested library jar onto the compile classpath.
- `tasks.remap_jar(jar, mappings)` — hierarchy-aware Mojang mojmap deobfuscation.
- `tasks.merge_jars(base, overlay)` — deterministic union, overlay wins on conflict.
- `tasks.decompile_java(jar, prefix)` — compile-oriented skeleton source tree.
- `tasks.publish_tree(..., "replace-root")` + `tasks.add_classpath` for the remapped game jar.
- `build.feature("server")` / `build.feature("client")` for `[build.features]` side switching — the
  resolved feature set is always part of the build-script fingerprint, so no `rerun_if_env_changed`
  is needed for it.

## Compile-safety

Compile-oriented rendering applies several defenses so skeletons stay closer to valid Java:

- field `final` is dropped (avoids blank-final errors under empty constructors),
- bridge/synthetic methods are omitted (they reference anonymous types the tree does not render),
- enum `values()` / `valueOf()` are omitted (javac synthesizes them),
- interface methods with bodies are marked `default`,
- nested classes keep outer capture (not forced `static`) so enclosing type parameters still bind,
- `extends` is omitted so empty constructors never need an unavailable `super(...)`,
- method bodies are safe placeholders (`{}` / `throw new RuntimeException()`).

Even so, Minecraft 1.21.1 still leaves residual `javac` errors (generic type bounds that depended on
dropped supers, and other structural edge cases). Treat the published tree as **reference sources +
remapped bytecode classpath** first: browse it in the LSP, and expect `jals build` of the full tree
to report remaining errors. Full semantic recompilation of vanilla is not guaranteed.

The demonstrated piece is the pipeline itself (fetch → nested extract → remap → decompile →
exclusive publish); a cleanly compiling tree is best-effort.

## Ownership and clean

`replace-root` exclusively owns `src/main/java/net/minecraft`. A successful changed result
removes every existing descendant before rewriting the tree. `jals clean` drops that directory
along with `target/classes` and `target/jals/build`. The shared verified cache under
`target/jals/cache` is kept so `--offline` rebuilds stay fast.

## Legal note

Generated Minecraft sources and the original jars/mappings are Mojang's copyrighted material.
This example only records the download URL and digests; artifacts stay local to your machine
and must not be redistributed.

See [`jals-build/README.md`](../../jals-build/README.md#rhai-build-scripts) for the complete
task API.
