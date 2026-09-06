# unit_tests_wasm — `jals test` on the embedded WebAssembly engine

The same `#[test]` model as [`unit_tests`](../unit_tests), with one thing swapped: the tests run on
the WebAssembly interpreter compiled into `jals` instead of on a JVM. There is **no JDK involved at
any step** — the compiler and the engine are both in this process — so this project builds, tests,
formats and lints with nothing but the `jals` binary on `PATH`.

Two manifest keys select it, and they travel together:

```toml
[build]
backend = { type = "jals-wasm" }   # one module for the whole project

[toolchain]
runtime = "wasm"                   # what runs that module
```

Nothing else emits a module, so `jals.toml` refuses the two apart — in either direction, and for
every command, because the contradiction is the manifest's rather than one command's.

## Layout

```
unit_tests_wasm/
├── jals.toml                                   # the backend/runtime pair, plus [test]
├── .gitignore
├── src/main/java/com/example/
│   └── Calculator.java                         # code with its tests beside it (the Rust model)
└── src/test/java/com/example/
    └── OverflowTest.java                       # a separate test tree (the Java convention)
```

Both places work and they are additive, exactly as on the JVM side: the runner is what differs,
not where tests live.

## Run it

```sh
# Compile the tests into one module and run every one of them, each in a fresh instance.
cargo run -p jals-cli -- test
# → 4 tests run: 4 passed

# List what would run.
cargo run -p jals-cli -- test --list

# Only the tests whose id contains "adds".
cargo run -p jals-cli -- test adds

# Include the `#[ignore]` one — which is marked that way because it does not pass yet,
# so this run reports one failure.
cargo run -p jals-cli -- test --run-ignored all
```

## How a test is reached, and how it fails

- **There is no `main`.** wasm has no entry-point convention and Java's cannot be lowered — `main`
  takes a `String[]`, and a module has no `java.base` to supply `String`. So the harness is one
  **exported function per test**, and the runner calls each by name. An export name carries no
  owner, so the generated name carries the class: `JalsTest$com$example$Calculator$addsTwoNumbers`.
- **`assert` is armed at compile time.** A JVM decides at start-up whether assertions run, and
  `jals test` passes `-ea` for exactly that; a wasm host has no such moment, so `jals test`
  compiles the checks in and `jals build` does not. A failing assertion is a **trap** — Java's
  `AssertionError` is a library type no module declares, and nothing catches a trap, which is the
  property an assertion failure needs.
- **`#[should_fail]` is inverted by the runner, not by the harness.** The JVM shim wraps the call
  in `catch (Throwable)`; here a `catch` type has to be a class the project declares, so there is
  no way to write that. The runner sees the call's outcome directly instead, and a trap and an
  uncaught `throw` are one verdict — as they are on a JVM, where both are `Throwable`s.
- **Each test re-instantiates the module**, which runs every `static` initialiser again. That is
  the same isolation one JVM per test buys, at a far lower price. A `static {}` that traps fails
  the whole run up front rather than each test, because a trap seen later has to mean the body.

## What a test body may use

Whatever the wasm backend compiles: primitives, project-declared classes, arrays, generics,
interfaces, `instanceof`, `switch`, and `throw`/`try`/`catch` over project-declared exception
types. **Not** `String`, boxing, string concatenation, `System.out`, or any other library type —
there is no `java.base` in a module. `OverflowTest` writes `Integer.MAX_VALUE` out as a literal for
that reason.

A construct with no lowering is reported at compile time, and today without the file it is in —
`CompileWasm` takes the whole project at once and names the construct rather than the position.

## Flags this runner refuses

Refused rather than ignored, because dropping a product the command line asked for is worse than
saying the two do not go together:

| Flag | Why |
| --- | --- |
| `--timeout` | A wasm call cannot be interrupted — the embedded engine has no fuel and no epoch deadline, so a test that never returns holds its worker until the process is killed. |
| `--no-capture` | A module has no standard output to hand to the terminal. A failing test's account is on its own result line. |
| `--retries` | No clock, no network, no threads, no filesystem, and a fresh store per test: the second attempt recomputes the identical answer. |

Everything else works unchanged — the filters, `--exact`, `--skip`, `--run-ignored`,
`--partition`, `-j`, `--fail-fast`, `--list`, `--no-run`, `--message-format`. The planning half is
shared with the JVM runner, so a selection means one thing whichever one executes it.

See [`jals-build/README.md`](../../jals-build/README.md#5-testing) for the full reference, and
[`hello_world_wasm`](../hello_world_wasm) for the same backend without tests.
