# unit_tests — tests with `#[test]`, run like `cargo nextest`

A project whose tests are `#[test]` methods rather than a separate framework. `jals test` finds
them, compiles them, and runs **each one in its own JVM**, in parallel, with a progress bar and
`cargo nextest`-shaped output.

## Layout

```
unit_tests/
├── jals.toml                                   # [package] features = ["attributes"], [test]
├── .gitignore
├── src/main/java/com/example/
│   ├── Calculator.java                         # code with its tests beside it (the Rust model)
│   └── Main.java                               # [run] main-class
└── src/test/java/com/example/
    └── CalculatorTest.java                     # a separate test tree (the Java convention)
```

Both places work, and they are additive: `[test] source-dirs` is compiled *in addition to*
`[build] source-dirs`, never instead of it.

## Run it

From this directory:

```sh
# Compile the project's tests and run every one of them.
cargo run -p jals-cli -- test

# Just list what would run, one id per line on standard output.
cargo run -p jals-cli -- test --list

# Only the tests whose id contains "divide".
cargo run -p jals-cli -- test divide

# Include the `#[ignore]` ones.
cargo run -p jals-cli -- test --run-ignored all

# Serially, with the tests' own output going straight to the terminal.
cargo run -p jals-cli -- test -j 1 --no-capture
```

Output:

```
   Compiling unit_tests
    Starting 4 tests across 2 classes (1 skipped)
        PASS [   0.077s] com.example.CalculatorTest#addsAcrossTheTree
        PASS [   0.078s] com.example.Calculator#addsTwoNumbers
        PASS [   0.077s] com.example.CalculatorTest#dividesExactly
        PASS [   0.078s] com.example.Calculator#divisionByZeroThrows
------------
     Summary [   0.079s] 4 tests run: 4 passed
```

## What the example demonstrates

- **A test is a `static void` method with no parameters.** The harness reaches it by name from a
  generated sibling class in the same package, so it must not be `private` — every other shape is
  rejected at compile time, and the language server reports it while you type.
- **A failing `assert` fails the test.** `jals test` runs the JVM with `-ea`, which Java does not
  do by default; without it every `assert`-based test would pass without checking anything.
- **`#[should_fail]`** inverts the verdict: `divisionByZeroThrows` passes *because* its body
  throws.
- **`#[ignore]`** keeps a test listed but unrun until `--run-ignored ignored-only|all` asks for it.
- **`jals build` produces no test code at all.** `#[test]` methods are removed from the lowered
  source the way a false `#[cfg]` is, so `target/classes` holds only `add`, `divide` and `main`:

  ```sh
  cargo run -p jals-cli -- build
  javap -p -cp target/classes com.example.Calculator
  # public static int add(int, int);
  # public static int divide(int, int);
  ```

  The test run compiles to `[test] classes-dir` (`target/test-classes`) instead, so the two never
  overwrite each other.

## Failure output

A failing test's captured output is replayed under its status line, with the generated harness's
own stack frames removed so the trace names the line you wrote:

```
        FAIL [   0.102s] com.example.Calculator#addsTwoNumbers
--- stderr: com.example.Calculator#addsTwoNumbers ---
    Exception in thread "main" java.lang.AssertionError
    	at com.example.Calculator.addsTwoNumbers(Calculator.java:21)
```

See [`jals-build/README.md`](../../jals-build/README.md) for the full manifest reference.
