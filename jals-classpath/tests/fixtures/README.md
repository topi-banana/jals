# Decompile test fixtures

These `.class` files drive `../decompile.rs`, the skeleton synthesis tests (`ClassFile` →
signature-`.java` with M0 bodies). They are compiled locally with `javac` from trivial Java sources
written for this crate (no third-party or GPL-licensed code). `../decompile.rs` also reuses
`jals-classfile`'s round-trip fixtures (referenced by relative path) for breadth in the
valid-Java property test.

## Provenance / regenerating

Compiled with the JDK pinned for this repo (`javac 25`, class-file major version 69):

```sh
# Consts (M0 enrichments) / Branchy (M2 control flow) — need -parameters + -g:
javac -parameters -g -d out jals-classpath/tests/fixtures/src/Consts.java
cp out/demo/Consts.class jals-classpath/tests/fixtures/
javac -parameters -g -d out jals-classpath/tests/fixtures/src/Branchy.java
cp out/demo/Branchy.class jals-classpath/tests/fixtures/

# Outer + its nested types (grouping), compiled without debug info (so parameters stay `argN`):
javac -d out jals-classpath/tests/fixtures/Outer.java
cp out/demo/Outer*.class jals-classpath/tests/fixtures/
```

`Box.class` predates this README (a generic `Box<T>` with a `value` field and `get`/`set`, default
package, no debug info) and its source is not committed.

## What each fixture exercises

| `.class` file | from | notable structures |
| --- | --- | --- |
| `Box.class` | (source not committed) | a generic class `Box<T>`, a field, `get`/`set` — no debug info, so `argN` parameter names |
| `Consts.class` | `src/Consts.java` | `ConstantValue` initializers (every constant kind), real parameter names (`-parameters -g`), a declared checked exception (`Exceptions`), and the value / `void` / constructor body shapes |
| `Branchy.class` | `src/Branchy.java` | M2 `if` / `if-else` structuring: a guard-clause return, an `if-else` with a join, a null-guarded field store, and a chained `if` |
| `Outer.class` | `Outer.java` | a top-level class with a nested static class and a nested enum (nested-type grouping) |
| `Outer$Inner.class` | `Outer.java` | a nested static class |
| `Outer$Color.class` | `Outer.java` | a nested enum with constants |
