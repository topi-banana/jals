# Decompile test fixtures

These `.class` files drive the skeleton synthesis tests in `../decompile.rs` (`ClassFile` → `.java`
source with recovered or safe-fallback bodies). They are compiled locally with `javac`
from trivial Java sources written for this crate (no third-party or GPL-licensed code). The
valid-Java property test renders and parses every class fixture listed below.

## Provenance / regenerating

Compiled with the JDK pinned for this repo (`javac 25`, class-file major version 69):

```sh
# Consts (M0 enrichments) / Branchy (M2 control flow) / Locals (M3 local variables) /
# Loops (M4 loops) / Arrays (M5 array operations) / Concat + Sb (M6 string concatenation) /
# Cmp (M7 numeric comparison conditions) / IntCarried (JVM int-carried boolean/char values) /
# InvokeSpecialCalls (non-virtual superclass/default-interface dispatch) — need -parameters + -g:
javac -parameters -g -d out jals-classpath/tests/fixtures/src/Consts.java
cp out/demo/Consts.class jals-classpath/tests/fixtures/
javac -parameters -g -d out jals-classpath/tests/fixtures/src/Branchy.java
cp out/demo/Branchy.class jals-classpath/tests/fixtures/
javac -parameters -g -d out jals-classpath/tests/fixtures/src/Locals.java
cp out/demo/Locals.class jals-classpath/tests/fixtures/
javac -parameters -g -d out jals-classpath/tests/fixtures/src/Loops.java
cp out/demo/Loops.class jals-classpath/tests/fixtures/
javac -parameters -g -d out jals-classpath/tests/fixtures/src/Arrays.java
cp out/demo/Arrays.class jals-classpath/tests/fixtures/

javac -parameters -g -d out jals-classpath/tests/fixtures/src/Cmp.java
cp out/demo/Cmp.class jals-classpath/tests/fixtures/
# Switches (M8 switch structuring) — the nested enum is part of the fixture, since an enum switch's
# case labels are recovered from the enum class:
javac -parameters -g -d out jals-classpath/tests/fixtures/src/Switches.java
cp out/demo/Switches.class 'out/demo/Switches$Color.class' jals-classpath/tests/fixtures/
javac -parameters -g -d out jals-classpath/tests/fixtures/src/IntCarried.java
cp out/demo/IntCarried.class jals-classpath/tests/fixtures/
javac -parameters -g -d out jals-classpath/tests/fixtures/src/InvokeSpecialCalls.java
cp out/demo/InvokeSpecialCalls.class jals-classpath/tests/fixtures/
cp out/demo/InvokeSpecialBase.class jals-classpath/tests/fixtures/
cp out/demo/InvokeSpecialDefault.class jals-classpath/tests/fixtures/

# Hierarchy evolution: compile the client and all v1 supertypes, then recompile only the two evolved
# v2 supertypes against v1. HierarchyEvolution.class always remains the old v1 client.
mkdir -p jals-classpath/tests/fixtures/hierarchy-evolution/v1
mkdir -p jals-classpath/tests/fixtures/hierarchy-evolution/v2
javac -parameters -g \
  -d jals-classpath/tests/fixtures/hierarchy-evolution/v1 \
  jals-classpath/tests/fixtures/src/hierarchy-evolution/v1/HierarchyEvolution.java
javac -parameters -g \
  -cp jals-classpath/tests/fixtures/hierarchy-evolution/v1 \
  -d jals-classpath/tests/fixtures/hierarchy-evolution/v2 \
  jals-classpath/tests/fixtures/src/hierarchy-evolution/v2/HierarchyBase.java \
  jals-classpath/tests/fixtures/src/hierarchy-evolution/v2/HierarchyRight.java

# Concat (M6 string concatenation, javac's default invokedynamic lowering) — needs -parameters + -g:
javac -parameters -g -d out jals-classpath/tests/fixtures/src/Concat.java
cp out/demo/Concat.class jals-classpath/tests/fixtures/

# Sb (M6 string concatenation, StringBuilder chains) — additionally forces the inline lowering:
javac -parameters -g -XDstringConcat=inline -d out jals-classpath/tests/fixtures/src/Sb.java
cp out/demo/Sb.class jals-classpath/tests/fixtures/

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
| `Locals.class` | `src/Locals.java` | M3 local variables: straight-line temporaries, a local written in both `if`/`else` branches and read after the join (hoisting), and a reference-typed local |
| `Loops.class` | `src/Loops.java` | M4 loop structuring: a bottom-test `while` with an `iinc` counter and a `do`-`while` |
| `Arrays.class` | `src/Arrays.java` | M5 array operations: element reads/writes, `newarray`/`anewarray`/`multianewarray` creation, folded `new T[]{…}` initializers (int/String/long/boolean, nested), an array-typed `checkcast`, array class literals, `arraylength`, and a compound element store (`xs[i]++`, `dup2`) that must bail |
| `Concat.class` | `src/Concat.java` | M6 string concatenation via `invokedynamic makeConcatWithConstants`: recipe chunks, String/int/char/double/boolean operands, a vanished `""` operand (the `""`-seed case), a marker-bearing constant passed as a bootstrap argument (the U+0002 path), a LambdaMetafactory call site that must bail, and a discarded `new` expression statement |
| `Sb.class` | `src/Sb.java` | M6 string concatenation via `StringBuilder` chains (`-XDstringConcat=inline`): foldable append runs (String/int/boolean, a constant char appended as an int, an empty-String anchor), plus chains that must stay calls — no `toString()`, consumed by `length()`, discarded as a statement, or appended onto a parameter |
| `Cmp.class` | `src/Cmp.java` | M7 numeric comparison conditions: a `lcmp`/`fcmpl`/`fcmpg`/`dcmpl`/`dcmpg` fused into the following `if<cond>` across all six operators and both NaN flavors, in `if`/`while`/`do`-`while`, plus the two bails — a NaN-inexact rendering (`!(f < g)`, `fcmpg` + `iflt`) and a `*cmp` feeding a ternary's value merge |
| `Switches.class` | `src/Switches.java` | M8 `switch` structuring: both table encodings, stacked labels sharing one arm, a `tableswitch` gap key, deliberate fall-through, every join shape (break-derived, a `default`-less fall-out, all-arms-`return`), `default` written in the middle, an `if` and an `if`/`else` inside an arm, `char` labels, an `enum` switch recovered to constant names, and a `String` switch that still falls back |
| `Switches$Color.class` | `src/Switches.java` | The enum `Switches.onEnum` switches on — its `<clinit>` is where the `ordinal -> constant name` map comes from |
| `IntCarried.class` | `src/IntCarried.java` | Type-directed recovery of JVM int-carried `boolean`/`char` values in returns, locals, fields, ordinary call arguments/results, and arrays; integer-zero tests versus boolean negation; explicit `i2c` and literal char casts, including a lone surrogate code unit |
| `InvokeSpecialCalls.class`, `InvokeSpecialBase.class`, `InvokeSpecialDefault.class` | `src/InvokeSpecialCalls.java` | Non-constructor `invokespecial` dispatch to a direct superclass (`super.m()`) and direct interface default (`Interface.super.m()`), plus the complete hierarchy needed to prove the qualified call and explicit argument-bearing `super(...)` constructor delegation |
| `hierarchy-evolution/v1/evolution/*.class` | `src/hierarchy-evolution/v1/HierarchyEvolution.java` | An old client with two legal interface-super calls, a shared-default diamond, and its complete original hierarchy |
| `hierarchy-evolution/v2/evolution/HierarchyBase.class` | `src/hierarchy-evolution/v2/HierarchyBase.java` | Evolved direct superclass that now implements the qualified interface, making the old client's qualifier redundant under JLS 15.12.1 |
| `hierarchy-evolution/v2/evolution/HierarchyRight.class` | `src/hierarchy-evolution/v2/HierarchyRight.java` | Evolved direct superinterface that contributes a distinct override of the selected ancestor default, triggering JLS 15.12.3 |
| `Outer.class` | `Outer.java` | a top-level class with a nested static class and a nested enum (nested-type grouping) |
| `Outer$Inner.class` | `Outer.java` | a nested static class |
| `Outer$Color.class` | `Outer.java` | a nested enum with constants |
