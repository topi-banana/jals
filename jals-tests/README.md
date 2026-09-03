# jals-tests

Corpus harnesses that exercise jals against large bodies of real Java.

Four binaries, four questions:

| binary | question | metric |
| --- | --- | --- |
| `jals-tests` | Does the **parser** hold its invariants? | never panics, lossless round-trip, syntax-error rate |
| `jals-golden` | How close is the **formatter** to each native Java formatter? | exact-match count + mean line similarity |
| `jals-compile` | Does the **compiler** emit class files a real JVM loads? | how far each file gets: parsed → lowered → re-read → verified |
| `jals-wasm` | Does the **WasmGC backend** emit modules an engine runs, computing what javac's output computes? | how far each file gets: parsed → lowered → validated → instantiated → agreed |

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
| descriptor-equal | every method's descriptor is one javac gave the same name |
| *descriptors-unjudged* | not a rung: nothing was compared, and the outcome says which of three reasons |

`verified` is what this harness was built for. The assembler computes its own `max_stack`,
`max_locals` and `StackMapTable`, and `jals-classfile` reads back whatever those say — so a frame
describing the wrong type round-trips perfectly and is still a class no JVM will load. Only the
verifier has an opinion, and it is the authority.

`descriptor-equal` is the rung the verifier structurally cannot reach. It judges one compilation at
a time, and every case here is a single file, so an erasure the declaration and its call sites get
*equally* wrong is self-consistent and links cleanly — `<T extends Comparable<T>> void f(T)` emitted
as `f(Ljava/lang/Object;)` passes every rung below. Catching that needs a second opinion, and the
corpus already holds one beside every case. It is a **rung, not a defect**: the bytes load and run,
so `--strict` does not fail on one. And it is narrower than "compiled the way javac did" — where
both compilers declared a method of the same name, they must agree on what it takes and returns,
which is all a separately-compiled caller links against. Types jals did not emit, members javac has
and jals does not, access flags, and attributes are all out of scope.

It also has a third answer, `descriptors-unjudged`, and it exists because the comparison can fail
to *happen*. Folding that into "agreed" made the rung fail **open** — and open is the top rung, so
nothing-compared was scored as "jals agrees with javac", per construct family (a whole package
shares its `expected/` output), and invisibly: no bucket, no `--list-failures` entry, no effect on
`--strict`. A case that lands there counts as `verified` and not as `descriptor-equal`, and is never
a defect — the same treatment `read-error` gets for a source the harness could not read.

The outcome carries **which** of three reasons, because they are not one finding and only the first
is about the corpus:

| reason | what it means |
| --- | --- |
| the class files could not be read | javac's own `expected/` output is read through this workspace's own reader, so a `.class` it refuses or a directory a partial generation run left empty means there was nothing to read |
| jals emitted no type javac also named | both compilers produced class files and named none of them the same — a disagreement about *names*, which is a finding of its own and not one about descriptors |
| neither compiler declares a method the other does | an annotation interface, a marker interface, a `package-info`: javac's class file declares no method, so there is nothing to agree about |

The distinction is not cosmetic. Every one of these used to print "javac's own class files for the
case could not be read", which sent a reader to the corpus generator for something the compiler
decided — and of the thirty cases in the current run, **none** is an unreadable class file.

Both this and `descriptor-equal`'s failures are listed **per case** rather than bucketed. A bucket
exists to bundle failures of one shape, and these have none in common: each names a different method
of a different class, so eliding the names leaves one row saying `a descriptor javac spells
differently` forty-seven times over. `--limit` bounds the listing the way it bounds the gaps.

### Working a bucket: `--list-gaps`

A bucket says what the remaining work *is*; it does not say which file to open. `--list-gaps` prints
every gap case by name with its message unelided, so a bucket of 55 becomes 55 paths. It is
deliberately not bounded by `--limit` — that flag bounds a listing chosen for a summary, and this one
is asked for by name to be worked through. Both `jals-compile` and `jals-wasm` take it; on the wasm
side it lists the *in-subset* gaps only, since a case outside the subset is the denominator rather
than the rate.

```sh
cargo run --release -p jals-tests --bin jals-compile -- langtools --limit 0 --list-gaps
```

### Working a case: the `emit` example

A gap listing names a file; `emit` turns that file into the bytes. It hands the same source to the
same front end and writes the class files or the WebAssembly module, so `javap -c`,
`wasm-tools print`, and a real JVM or engine can be pointed at what the compiler actually produced.
It resolves against the host JDK's `ct.sym` like `jals-compile` does — which is what makes a corpus
case reproduce — and `--stdlib` switches to the embedded stubs, which is what `jals-javac`'s own
tests resolve against.

```sh
cargo run --release -p jals-tests --example emit -- <File.java> <out-dir>
cargo run --release -p jals-tests --example emit -- --wasm <File.java> <out.wasm>
```

An example rather than a fifth binary: it answers no question and reports no rate, so it is not one
of the four this README's table is about.

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
That directory is both what makes a `.java` under the root a case at all and what the
`descriptor-equal` rung reads. The corpus is a derivative of GPL'd OpenJDK sources, so like the four
formatter corpora it is **generated locally and gitignored, never committed**.

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

CI leaves `--strict` off: known defects are still open, so the report is a measurement rather than a
gate. Turning it on is what would make it one, and that is a decision to take once the list is
empty. What is open, by family:

- **an inferred type the analysis does not compute**: a `return` whose value comes from an
  inference this crate does not run (`generics/inference`), where the emitted `areturn` carries a
  type the method's own descriptor does not admit.
- **an operand whose type the analysis got wrong** at a call — the same cause seen from the call
  site rather than from the `return`.
- **`protected` access across packages** (JLS §6.6.2): a `protected` member of a superclass in
  another package may be reached only through a reference of the accessing class's own type, and
  the emitted `invokevirtual` names the declaring one.

The parser is no longer among them: every file in the corpus parses, so `parsed` is 100% and a
syntax error there would now be a regression rather than a known gap.

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

## WasmGC end-to-end — `jals-wasm`

The other backend, over **the same corpus**. `[build] backend = "jals-wasm"` compiles a whole
project to one WebAssembly module rather than a class file per type, and the question is the same
one `jals-compile` asks with the authorities swapped: not "does a JVM link this" but "is this a
module, does an engine run it, and does it compute what javac's own output computes".

Nothing here generates anything. `jals-wasm` walks `sources/javac-langtools` — the very cases
`gen-javac-corpus.sh` wrote — and reads the same `expected/` directories, because both backends
compile the same language from the same front end and a second corpus would be a second
denominator for one question.

```sh
git submodule update --init --depth 1 jals-tests/sources/openjdk
jals-tests/scripts/gen-javac-corpus.sh 0          # the corpus jals-compile already needs
cargo run -p jals-tests --bin jals-wasm -- langtools
```

| rung | what it proves |
| --- | --- |
| parsed | `jals-syntax` accepted the source with no syntax error |
| lowered | `CompileWasm::project` produced a module rather than a `WasmError` |
| **validated** | `wasm-tools validate` accepted it — the specification's own authority |
| instantiated | `wasmtime` instantiated it, which is where the start function runs |
| **agreed** | every jointly-callable method answered what javac's class file answers on a JVM |

`validated` is this ladder's `verified`. The encoder writes its own type indices, block types and
local counts, and `Module::finish` encodes whatever they say — so a body whose stack does not
balance round-trips perfectly and is still a module no engine will load. Only a validator has an
opinion, and it is the authority.

`instantiated` is a rung and not a formality. A Java `static` initialiser is lowered into the
module's **start function**, so instantiating is the cheapest way to *run* the lowering of every
case that has one, with no entry point to choose and no arguments to invent. It is asked for by
invoking an export that cannot exist: the engine compiles and instantiates the module *before* it
looks the name up, so a `no func export named` reply is the proof that instantiation succeeded.

`agreed` is the rung a validator structurally cannot reach — a module can be perfectly well-typed
and compute the wrong number — and it is the wasm counterpart of `descriptor-equal`.

### Two denominators, because this backend has a target subset

`WasmError::NoRepresentation` is not a gap. A wasm host has no `java.base`, so a file naming
`String` is **outside what this backend compiles**, by design, exactly as a file javac declines
alone is outside `jals-compile`'s corpus. Those cases are reported as *out of subset* and excluded
from the rate that measures the compiler; the corpus total is printed beside it so the scoped rate
can never read as coverage of Java. On the current corpus that is 1316 of 2188 cases, `String`
alone accounting for the largest share of them.

Three types the backend *does* represent are not in that count, and each is a rule rather than a
stub: `java.lang.Object` is the root of Java's reference hierarchy and `anyref` is wasm's, a **type
variable** erases to its bound and to `Object` with none (JLS §4.6), and an `@interface` is an
interface (§9.6). What still needs `java.base` after that is what a **value** of a library type
needs — a `String`, a wrapper for a boxing conversion, a `PrintStream` for a call.

The classification is **post hoc and order-dependent**: lowering reports the first thing it cannot
do, so a file that both names `String` *and* declares an `@interface` lands in whichever the
lowering reached first. That is a property of the traversal and not of the source. It is stated
rather than solved — nothing re-lowers a case to find out what else it would have said.

### What the agreement rung compares, and what it does not

A wasm export carries a **bare method name and no owner** (`is_static && !is_constructor` is the
whole export rule, with no visibility test), so a pairing is only defined where javac declares that
name exactly once in the case, `static`, over a parameter list this harness can spell. That means
primitives: inventing a receiver or a `String` would be inventing the test rather than running it.
Everything else answers *unjudged* with its reason, and `unjudged` is an outcome of its own rather
than a pass — folding "nothing compared" into "agreed" makes the top rung fail **open**, and open
is the top rung.

Each paired method is called six times, with the parameters' sample lists indexed at the same
position rather than crossed, so a method of six parameters costs six calls and not six to the
sixth. Three asymmetries between the two sides are closed by construction, because each would
otherwise manufacture disagreements that say nothing about the compiler:

- **State.** `wasmtime run --invoke` is a fresh process, so the module's globals restart on every
  call. The driver therefore gives every call a class loader of its own.
- **Width and sign.** wasm has `i32`/`i64`/`f32`/`f64`; Java has `boolean`, `byte`, `char` and
  `short` besides. The comparison is on a canonical form derived from javac's *descriptor*, so a
  `byte` the backend forgot to narrow is a difference rather than a formatting quirk. A float is
  compared as its bit pattern with `NaN` canonicalised — neither side's printed decimal is the
  value (`-0` against `-0.0`).
- **Failure.** A trap and a thrown exception are **not** folded together. Collapsing them would
  hide the finding this rung exists for: a missing bounds check reads garbage where the JVM throws.
  A call that failed on both sides is counted apart from one that agreed, and a call that failed on
  only one is a finding.

The two comparison counts are reported separately, because they are different claims: a returned
value that matches is *"computed the same answer"*, while a `void` method that completes on both
sides is only *"neither side failed"*.

**What the numbers mean today.** On the current corpus the rung judges 23 cases — 5 value
comparisons and 26 completions — because a Java entry point takes `String[]` and a file naming
`String` never reaches this rung at all. That is a fact about the corpus, not a claim about the
compiler, which is why `jals-wasm` **lists the judged cases by name** rather than only counting
them: a reader has to be able to tell a rate of 2% that means "2% of the compiler is checked" from
one that means "2% of the corpus offers anything to check". It is the second. Widening it needs a
corpus written in the backend's own subset, which is work this harness makes measurable and does
not itself do.

### Version pins

Three tools decide this measurement. javac still chooses the corpus and its `ct.sym` is still the
classpath (`compile::JAVAC_PIN`); on top of that `wasm::WASM_TOOLS_PIN` is the validator and
`wasm::WASMTIME_PIN` the engine. The validator's message text **is** the failure-bucket key, so a
bump silently re-partitions the report — which is why these are pinned like a formatter release and
`the_pins_match_ci` fails when they drift from `.github/workflows/ci.yml`.

### What fails the run, and what only lowers the rate

An unimplemented lowering path lowers the percentage. Four outcomes are **defects**: a module the
validator refuses, a compiled program that answers something else than javac's, a panic, and a
syntax error on a file that is valid Java by construction. `--strict` exits non-zero on those.

CI leaves `--strict` off, as it does for `jals-compile`: known defects are still open, so the
report is a measurement rather than a gate. What is open today is **eight modules `wasm-tools`
refuses**, and they are two families, not one:

- **seven ill-typed function bodies** — `expected (ref null $type), found (ref $type)`, `expected
  i32, found (ref $type)`, `values remaining on stack at end of block`, `expected … but nothing on
  stack`. Four of them print the *same* type on both sides of the mismatch, which means two
  distinct types in the recursive group print identically — most likely one class laid out twice
  (the anonymous, inner and lambda shapes are where the cases cluster).
- **one format limit reached without being reported** — `JsrRet.java`, `too many locals: locals
  exceed maximum`. `WasmError::TooLarge` exists precisely so a limit is refused rather than
  emitted, and this one got past it.

Everything else is a gap rather than a defect, and the report buckets it. The largest are an
`@interface` declaration (157 cases), a subclass of an inner class (71), and a call to a method
outside this module (45).

Two things the ladder does **not** yet report, because on this corpus they never happened: a start
function that trapped, and a disagreement. Both have their own listing, so the day one appears it
is named rather than averaged in.

## In CI

The `corpus-reports` job downloads each pinned tool, generates the four OpenJDK formatter corpora
plus the javac one, and runs all four harnesses, putting every corpus in one summary. `jals-wasm`
adds no corpus of its own — it reads the javac one — so what it adds to the job is two pinned
binaries (`wasm-tools`, `wasmtime`) and one more report. Each is
cached independently on the OpenJDK submodule commit, the tool version, the generator script and
(for Eclipse) the committed profile, so a corpus is rebuilt only when something it depends on
moves. `--allow-missing` is what keeps one failed or timed-out generation step from costing the
whole report.

Each report shows its **heading, one caption, and its ladder table**; everything else is a
collapsed `<details>`, because four harnesses' worth of per-case rows is what buries the four
tables they exist to explain. Collapsed is not hidden: a `<summary>` carries the count and what the
rows are, so the defect listings still announce themselves — `DEFECTS_ALWAYS_LISTED` keeps a defect
in the report whatever `--limit` says, and what the `<details>` removes is the twenty rows of
corpus paths under the sentence that already named them.

So the formatter report collapses a least-similar list per corpus; the compiler report collapses
what each rung proves, the defects, the descriptor rung's cases, what stopped the rest and what
javac declined; and the WasmGC report collapses the same plus why the agreement rung compared
nothing, which cases it judged, and which types put a case outside the subset. The WasmGC ladder
keeps both denominators in the visible table, since the scoped rate cannot be read without them.
