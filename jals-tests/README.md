# jals-tests

Corpus harnesses that exercise jals against large bodies of real Java.

Three binaries, three questions:

| binary | question | metric |
| --- | --- | --- |
| `jals-tests` | Does the **parser** hold its invariants? | never panics, lossless round-trip, syntax-error rate |
| `jals-golden` | How close is the **formatter** to each native Java formatter? | exact-match count + mean line similarity |
| `jals-compile` | Does the **compiler** emit class files a real JVM loads? | how far each file gets: parsed → lowered → re-read → verified |

The corpora are git submodules (and, for the generated ones, local files) under
`sources/`; none of the Java is committed to this repo.

## Parser soundness — `jals-tests`

```sh
git submodule update --init --depth 1 jals-tests/sources/openjdk
cargo run -p jals-tests -- openjdk --list-failures
```

Two sources, both under the `openjdk` submodule (`src/lib.rs`'s `ALL_SOURCES`): `openjdk`
(every `.java` file in the repository) and `langtools` (`test/langtools`, OpenJDK's own
compiler test suite, which *intentionally* contains invalid Java — a nonzero syntax-error
rate there is expected, not a bug). Running with no source argument checks both; `--markdown`
emits the GitHub-flavored summary table CI posts to pull requests.

See `src/lib.rs` for the outcome classification.

## Formatter fidelity vs the native formatters — `jals-golden`

A **corpus** is a tree of `*.input`/`*.output` pairs: `Foo.input` is unformatted source and
`Foo.output` is what a reference formatter produced from it. `jals-golden` formats every
`.input` with that reference's style and scores the result against the `.output`.

Each corpus names a **target** — the formatter it is measured against, the `Config` jals is
given, and the accuracy tier `jals-fmt/DESIGN.md` §18.1 promises:

| target | reference | tier | promise |
| --- | --- | --- | --- |
| `gjf` | google-java-format | T1 | byte match (the engine is a port of GJF's own `computeBreaks`) |
| `palantir` | palantir-java-format | T2 | layout approximation only |
| `eclipse` | Eclipse JDT | T2 | layout approximation only |
| `intellij` | IntelliJ IDEA | T2 | layout approximation only |

> **It does not pass/fail.** Only `gjf` aims at a byte match. The other three resolve line
> breaks with algorithms jals deliberately does not port (§11 conclusion 1: Palantir's
> backtracking search, Eclipse's penalty minimization, IntelliJ's rewind), so **any exact
> match they show is incidental** — Palantir is a GJF fork, so much of its layout coincides
> with the ported engine's, while Eclipse and IntelliJ sit near zero — and the number that
> means something is mean similarity. A byte-equal rate alone would also hide progress — one
> space of difference sinks a whole file — which is why every target reports similarity.

> **What the numbers mean today.** The `gjf` byte match is the goal, not a reached state. The
> harness exists so convergence is measured rather than asserted, and each of the four targets
> moves on its own row.

```sh
cargo run -p jals-tests --bin jals-golden -- gjf-testdata --worst 20
# every corpus that is present, skipping the ones that are not:
cargo run -p jals-tests --bin jals-golden -- --allow-missing --markdown
# point it at your own formatted project (a tree of .input/.output):
cargo run -p jals-tests --bin jals-golden -- --dir /path/to/pairs --style palantir
# add DESIGN.md §18.2.1's two extra columns — what each corpus would score with perfect
# comment formatting, so what is left is the layout algorithms alone:
cargo run -p jals-tests --bin jals-golden -- --allow-missing --worst 0 --ceiling
```

`--ceiling` formats the corpus a second time, so it is off by default. It is what keeps
`jals-fmt/DESIGN.md` §18.2.1's table a measurement rather than a remembered number: drop every
comment line from both sides and the residue is the permanent differences D1–D4, and add the
expected side's comment lines back as matches and the result is the score a perfect comment
formatter would reach.

### Where the style config comes from

google-java-format and palantir-java-format have no config file — their style *is* the
tool — so `Target::google_config` / `Target::palantir_config` come straight from
`jals_fmt::import`'s models.

Eclipse and IntelliJ have ~400 and ~270 options each, so a corpus generated from one style
and scored against another would measure the config plumbing rather than the layout engines.
Both therefore read **one committed file, twice**: `gen-openjdk-corpus.sh` hands it to the
native tool, and `Target::eclipse_config` / `Target::intellij_config` `include_str!` the same
bytes back through `jals_fmt::import`. That also makes the corpus report a live integration
test of those two importers.

- `config/eclipse-jals.prefs` — JDT's own built-in default profile, dumped verbatim by
  `EclipseFormat --dump-defaults`, plus four compliance keys. Nothing is tuned in jals's
  favour, and nothing needs to be: `join_wrapped_lines` is already `true` by default, so the
  corpus does not carry §18.2's **D5** (the input's line breaks preserved).
- `config/intellij-jals.xml` — deliberately short: the right margin, the indent options, and
  the `KEEP_*` family **forced off**. That last group is the one departure from a stock IDEA,
  and it is the point: IntelliJ's `KEEP_LINE_BREAKS` defaults to `true`, so left alone most
  of every `.output` would just be OpenJDK's own hand-wrapping preserved, and the score would
  report D5 rather than anything about wrapping.

  **Known caveat.** Everything else in that file is left unstated, so IDEA uses its defaults
  and the importer uses its model's. The two are believed equal — `jals-fmt`'s
  `intellij/inventory.tsv` is extracted from intellij-community's own sources — but the
  inventory carries no default *values*, so nothing here proves it. Values are not guessed
  into the file: writing a wrong "default" would move the corpus away from stock IDEA, which
  is worse than leaving it out.

### Corpora

Two are vendored (the reference tool's own regression suite, pinned by submodule commit) and
four are generated from OpenJDK (pinned by tool release). The generated ones are derivatives
of GPL'd OpenJDK sources, so they are **built locally and gitignored, never committed**.

| corpus | target | source |
| --- | --- | --- |
| `gjf-testdata` | `gjf` | google-java-format's own suite (submodule) |
| `palantir-testdata` | `palantir` | palantir-java-format's own suite (submodule) |
| `openjdk-gjf` | `gjf` | OpenJDK `src/` (generated) |
| `openjdk-palantir` | `palantir` | OpenJDK `src/` (generated) |
| `openjdk-eclipse` | `eclipse` | OpenJDK `src/` (generated) |
| `openjdk-intellij` | `intellij` | OpenJDK `src/java.base` (generated) |

#### Vendored suites

```sh
git submodule update --init --depth 1 jals-tests/sources/google-java-format
git submodule update --init --depth 1 jals-tests/sources/palantir-java-format
```

Both are Apache-2.0 and mostly bug-tracker regressions, so they are *edge-case* sets more
than representative real code. Palantir's suite is generated by its `FormatterIntegrationTest`
under `Style.PALANTIR`, which is what `Target::palantir_config` scores it with.

#### Generated from OpenJDK

`scripts/gen-openjdk-corpus.sh <target> [COUNT]` walks the OpenJDK submodule (or the subtree
named by `SUBTREE`), formats a throwaway copy of each file with the target's formatter, and
writes the `.input`/`.output` pairs. A file the tool declines comes back byte-identical and
is skipped, so a corpus only ever holds pairs the tool actually produced. `COUNT` caps how
many files to consider (`0` = no cap) — useful for a quick local sample. Common env:
`SUBTREE` (subtree to walk), `JOBS` (concurrent formatter processes, default 2).

Each target needs its tool pointed at by one variable:

```sh
git submodule update --init --depth 1 jals-tests/sources/openjdk

# gjf — an "all-deps" jar from the google-java-format releases page (needs JDK 21+; the
# script passes the --add-exports flags modern JDKs require).
GJF_JAR=jals-tests/vendor/google-java-format-1.35.0-all-deps.jar \
  SUBTREE=src jals-tests/scripts/gen-openjdk-corpus.sh gjf 0

# palantir — a GraalVM native image from Maven Central: one self-contained binary, no JVM.
curl -fL --create-dirs -o jals-tests/vendor/pjf \
  https://repo1.maven.org/maven2/com/palantir/javaformat/palantir-java-format-native/2.96.0/palantir-java-format-native-2.96.0-nativeImage-linux-glibc_x86-64.bin
chmod +x jals-tests/vendor/pjf
PJF_BIN=jals-tests/vendor/pjf SUBTREE=src jals-tests/scripts/gen-openjdk-corpus.sh palantir 0

# eclipse — JDT jars from Maven Central plus the ~120-line driver in scripts/eclipse/, which
# the generator compiles for you. No Eclipse installation and no OSGi involved.
ECLIPSE_CP="$(jals-tests/scripts/fetch-eclipse-jdt.sh)" \
  SUBTREE=src jals-tests/scripts/gen-openjdk-corpus.sh eclipse 0

# intellij — an unpacked IntelliJ IDEA (the unified distribution; Community stopped
# shipping separately at 2025.3 and the formatter stays in the free tier). The generator
# drives bin/format.sh headless.
IDEA_HOME=/path/to/idea SUBTREE=src/java.base \
  jals-tests/scripts/gen-openjdk-corpus.sh intellij 0
```

`openjdk-intellij` covers only `src/java.base`, not all of `src/`: IDEA's formatter starts a
whole IDE and is an order of magnitude slower than the other three. The narrower scope is
stated in the corpus description so the report cannot read as full coverage.

### Version pins

All four references are version-unstable across releases (`DESIGN.md` §7.1, §11 conclusion
6), so a similarity number is only defined against a fixed version. `golden::TOOL_PINS` holds
the pinned release per tool and prints it in the report's `reference` column;
`the_tool_pins_match_ci` and `the_eclipse_pin_matches_the_fetch_script` fail when those pins
drift from `.github/workflows/ci.yml` or from `scripts/fetch-eclipse-jdt.sh`. Bump all of
them together.

## Compiler end-to-end — `jals-compile`

Does `jals-javac` turn real Java into class files a real JVM loads? Every case reports how far
it got, because one number over a compiler says nothing about what is missing:

| rung | what it proves |
| --- | --- |
| parsed | `jals-syntax` accepted the source with no syntax error |
| lowered | `Compile::file` produced class files rather than a `LowerError` |
| re-read | `jals-classfile` reads back what the assembler wrote |
| **verified** | a real JVM **linked** the class: the bytecode verifier accepted it |

The last rung is the point. The assembler computes its own `max_stack`, `max_locals` and
`StackMapTable`, and `jals-classfile` reads back whatever those say — so a frame describing the
wrong type round-trips perfectly and is still a class no JVM will load. Only the verifier has an
opinion, and it is the authority.

```sh
git submodule update --init --depth 1 jals-tests/sources/openjdk
jals-tests/scripts/gen-javac-corpus.sh 0          # or a COUNT, for a quick local sample
cargo run -p jals-tests --bin jals-compile -- langtools
```

Generation runs one `javac` per candidate, so it is the slow half: `JOBS` sets how many run at
once, and `JAVAC_TIMEOUT` (default 60s) bounds each one. That bound is not hygiene — this is a
*compiler's* regression suite, and some of it exists to push javac to its limits, so a handful of
files never finish compiling at all. `SUBTREE` picks a different tree to walk.

### Why the corpus is generated, and what the denominator excludes

There is no ready-made `.java` → expected `.class` corpus, in OpenJDK or anywhere else.
`test/langtools/tools/javac` is a jtreg-driven **behaviour and diagnostic** suite: a fifth of it
is `@compile/fail` — deliberately invalid Java, which measures nothing for a compiler that never
checks (diagnostics are `jals-lint`'s job over `jals-hir`) — and a third has no `@test` header at
all, being auxiliary sources that only mean something beside a sibling.

So `scripts/gen-javac-corpus.sh` runs the pinned `javac` over each candidate **on its own** and
keeps the ones it compiles. That is what makes the denominator honest: a file javac itself cannot
compile alone — a multi-file test, one that needs `com.sun.tools.javac` internals, one whose
sibling package is missing — is **out of scope**, recorded with javac's own reason in
`SKIPPED.tsv`, and never counted as a compiler failure. Negative tests are excluded outright
rather than left to fail, since scoring a file whose purpose is to be rejected would quietly turn
this harness into a checker.

Each case is `<Base>.java` beside a `<Base>.expected/` directory holding javac's own class files.
Nothing reads `expected/` yet; it is written now because a future run-equivalence rung needs it
and regenerating the corpus to obtain it later costs the whole generation pass. The corpus is a
derivative of GPL'd OpenJDK sources, so like the four formatter corpora it is **generated locally
and gitignored, never committed**.

### The classpath is a real JDK's

`jals-hir`'s embedded stubs are ~58 signature-only types — enough to say something useful about
an editor buffer, nowhere near enough to compile arbitrary Java. Scoring against them would
report *stub coverage* wearing a compiler's name. `$JAVA_HOME/lib/ct.sym` is the signature data
`javac --release` reads (an ordinary zip of ordinary class files with their bodies stripped), so
the harness lowers it into the `LoweredClasspath` the analysis resolves against — the same thing
the product does through `jals-classpath` for a real dependency. Reading it needs a host path,
which is why this lives in a test harness; `jals-javac`'s own stdlib oracle reads the same file
for the same reason.

### What fails the run, and what only lowers the rate

An unimplemented lowering path lowers the percentage. Four outcomes are **defects** and are
listed separately: a class file the JVM rejects, output that does not read back, a panic, and a
syntax error on a file that is valid Java by construction. `--strict` exits non-zero on those, so
a regression into a wrong class file fails a build while the long tail of unimplemented syntax
does not.

CI leaves `--strict` off: known defects are still open (a nested `new` passing the wrong enclosing
instance, `this$0` read or stored before `super()` — which JEP 447's statements-before-`super()`
reaches from a second direction — and four `jals-syntax` gaps: unicode escapes as JLS §3.3 defines
them, a stray `;` at top level, `{,}` where the list may be empty, and `var` used as an ordinary
identifier), so the report is a measurement rather than a gate. Turning it on is what would make it
one, and that is a decision to take once the list is empty.

Expect the list to *change* as the rate rises, and not always to shrink. A file blocked at
`lowered` never reaches the verifier, so fixing what blocked it does not only move it up the
ladder — it can move it into the defect list, exposing a backend bug that was always there. Reading
a bigger defect count as a regression is therefore wrong on its own; what says whether a change
regressed is which cases entered and left, and the ladder alongside them.

### Version pin

The rate depends on the JDK twice — javac decides the scope and its `ct.sym` is the classpath —
so it is pinned like the formatter releases: `compile::JAVAC_PIN`, `JAVAC_VERSION` in
`.github/workflows/ci.yml`, and the generator's own check, with `the_javac_pin_matches_ci` and
`the_generator_states_the_pin` failing when they drift.

## In CI

The `corpus-reports` job downloads each pinned tool, generates the four OpenJDK formatter corpora
plus the javac one, and runs all three harnesses, putting every corpus in one summary. Each is
cached independently on the OpenJDK submodule commit, the tool version, the generator script and
(for Eclipse) the committed profile, so a corpus is rebuilt only when something it depends on
moves. `--allow-missing` is what keeps one failed or timed-out generation step from costing the
whole report.

The formatter table lists six corpora with a least-similar `<details>` list each; the compiler
table lists the ladder per corpus, the defects in full, and collapsed lists of what stopped the
rest and of what javac declined.
