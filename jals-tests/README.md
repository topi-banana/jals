# jals-tests

Corpus harnesses that exercise jals against large bodies of real Java.

Two binaries, two questions:

| binary | question | metric |
| --- | --- | --- |
| `jals-tests` | Does the **parser** hold its invariants? | never panics, lossless round-trip, syntax-error rate |
| `jals-golden` | How close is the **formatter** to `google-java-format`? | exact-match count + mean line similarity |

The corpora are git submodules (and, for the generated one, local files) under
`sources/`; none of the Java is committed to this repo.

## Parser soundness — `jals-tests`

```sh
git submodule update --init --depth 1 jals-tests/sources/openjdk
cargo run -p jals-tests -- openjdk --list-failures
```

See `src/lib.rs` for the outcome classification.

## Formatter fidelity vs google-java-format — `jals-golden`

`jals-golden` formats each `*.input` of a corpus with a best-effort **Google Java
Style** config (`golden::google_config`) and compares the result to the paired
`*.output` that `google-java-format` produced.

> **It does not pass/fail.** jals cannot byte-match google-java-format on line
> wrapping yet — it has no separate continuation indent (Google uses +4 columns for
> continuations over a +2 block indent) and a different wrapping algorithm — so the
> harness reports a **similarity** metric to track convergence as formatter options
> land. The biggest expected jump is a dedicated continuation-indent option.

```sh
cargo run -p jals-tests --bin jals-golden -- gjf-testdata --worst 20
# point it at your own google-java-format-formatted project (a tree of .input/.output):
cargo run -p jals-tests --bin jals-golden -- --dir /path/to/pairs
# CI-style summary:
cargo run -p jals-tests --bin jals-golden -- --markdown
```

### Corpora

**`gjf-testdata`** — google-java-format's own `.input`/`.output` regression suite
(Apache-2.0). Add it as a submodule (kept upstream, not vendored):

```sh
git submodule add --depth 1 https://github.com/google/google-java-format \
  jals-tests/sources/google-java-format
```

These cases are mostly bug-tracker regressions, so they are an *edge-case* set more
than representative real code.

**`openjdk-gjf`** — real OpenJDK `src/` library code run through google-java-format
(what CI reports as the second fidelity row). These are derivatives of GPL'd OpenJDK
sources, so they are **generated locally and gitignored, never committed**.

1. Get a google-java-format "all-deps" jar (v1.35.0 at time of writing) from the
   [releases page](https://github.com/google/google-java-format/releases) and drop it
   in `jals-tests/vendor/` (gitignored).
2. Make sure the OpenJDK submodule is checked out (see above).
3. Generate the pairs. `gen-openjdk-gjf.sh` walks the submodule (or the subtree named
   by `SUBTREE`), formats each file with batched, warm google-java-format JVMs, and
   writes `.input`/`.output` pairs. google-java-format needs JDK 21+ (tested on 25); the
   script passes the `--add-exports` flags modern JDKs require. Env: `GJF_JAR` (required),
   `SUBTREE` (subtree to walk, default the whole submodule), `JOBS` (concurrent JVMs,
   default 2). The `COUNT` argument caps how many files to consider (default `0` = no cap):

   ```sh
   # The full src/ subtree (what CI generates):
   GJF_JAR=jals-tests/vendor/google-java-format-1.35.0-all-deps.jar \
     SUBTREE=src jals-tests/scripts/gen-openjdk-gjf.sh 0
   # A quick local sample (first 500 files of src/):
   GJF_JAR=jals-tests/vendor/google-java-format-1.35.0-all-deps.jar \
     SUBTREE=src jals-tests/scripts/gen-openjdk-gjf.sh 500
   ```

Then run `cargo run -p jals-tests --bin jals-golden -- openjdk-gjf`.

CI generates this corpus automatically in the `corpus-reports` job and caches it on the
OpenJDK submodule commit (plus the google-java-format version), so it is regenerated only
when the submodule pin moves. The report runs
`jals-golden gjf-testdata openjdk-gjf --markdown`, putting both corpora in one table with
a least-similar `<details>` list per corpus.
