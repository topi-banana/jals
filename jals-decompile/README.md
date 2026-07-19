# jals-decompile

Reconstructing readable Java source from a compiled `jals_classfile::ClassFile`.

This crate turns the byte-exact `.class` model produced by [`jals-classfile`](../jals-classfile) into
Java: the type vocabulary shared with the signature-skeleton renderer, the attribute readers a
skeleton needs (`ConstantValue` initializers, declared `throws`, real parameter names), and
**method-body decompilation from bytecode** (stack-machine expression recovery + CFG-based
control-flow structuring).

It is the analysis half of the "decompiled skeletons" feature: when a dependency jar ships no
`-sources.jar`, `jals-lsp` still needs somewhere for go-to-definition to land, so
[`jals-classpath`](../jals-classpath)'s `skeleton.rs` renders a `.java` skeleton for each classpath
`.class` and calls into this crate to fill in member details and method bodies. `jals-classpath` owns
all I/O (reading `.class` bytes, writing the `.java` files); this crate is pure.

## Properties

- **Pure & `wasm32`-compatible.** Depends only on `jals-classfile`; no filesystem, process, or network
  I/O. Built for `wasm32-unknown-unknown` in CI alongside `jals-syntax` / `jals-classfile` / `jals-hir`.
- **Never panics.** Everything is written with `Option` / early-return; no `unwrap` / `panic`.
- **Conservative.** Every reconstruction reports failure (`None` / empty) the moment it meets
  something it cannot render as valid Java, so the caller falls back to a safe form.
- **Navigation-only.** The output exists so an editor can *jump into* a library type and read a
  plausible body. Typing stays authoritative from the `.class` itself — nothing here is fed back into
  the type system.

### The core invariant

**The decompiled output is always valid Java** (`jals_syntax::parse` returns no errors). A method that
cannot be reconstructed falls back *per method* to a safe placeholder body rather than emit broken or
mis-structured source. This is enforced by a property test over a broad fixture corpus (generics,
enums, records, switches, annotations) that parses every rendered skeleton. Correctness beyond
"parses" is best-effort: for the constructs the decompiler claims to handle it aims to be faithful,
and it bails (falls back) rather than guess on anything it is unsure of.

## Public API

Each module exposes a zero-sized namespace type with its functions as associated functions (no free
functions — see the workspace's `no-free-functions` convention).

```rust
pub struct MethodBody;
impl MethodBody {
    // Method-body decompilation. `param_names` are the exact names the signature renders (in order);
    // the body reuses them and bails on a count mismatch (e.g. an enum ctor's synthetic `String, int`).
    // Returns indented Java statement lines, or `None` to fall back to a safe placeholder body.
    pub fn decompile(method: &MethodInfo, cf: &ClassFile, param_names: &[String])
        -> Option<Vec<String>>;
}

pub struct Attrs;
impl Attrs {
    // Attribute readers a signature skeleton needs but bytecode analysis does not.
    pub fn constant_value_initializer(field: &FieldInfo, pool: &ConstantPool) -> Option<String>;
    pub fn signature_string(attrs: &[Attribute], pool: &ConstantPool) -> Option<String>;
    pub fn declared_throws(method: &MethodInfo, pool: &ConstantPool) -> Vec<String>;
    pub fn parameter_names(method: &MethodInfo, pool: &ConstantPool, is_static: bool, arity: usize)
        -> Option<Vec<String>>;
}

pub struct JavaType;
impl JavaType {
    // The descriptor / generic-signature → Java type vocabulary, shared with `skeleton.rs`.
    pub fn internal_to_java(internal: &str) -> String;    // a/b/Outer$Inner -> a.b.Outer.Inner
    pub fn render_field_type(ft: &FieldType) -> String;   // [Ljava/lang/String; -> java.lang.String[]
    pub fn render_type_sig(ts: &TypeSignature) -> String; // generic type signature -> Java
    pub fn render_class_type_sig(c: &ClassTypeSignature) -> String;
    pub fn render_throws(t: &ThrowsSignature) -> String;
}
```

## Modules

| Module | Role |
| --- | --- |
| `types.rs` | Render a field / method descriptor or generic signature to a well-formed Java type reference. The vocabulary shared with `jals-classpath`'s `skeleton.rs`. |
| `literal.rs` | Render constant-pool constants (`int`/`long`/`float`/`double`/`String`/`Class`) as valid Java literals, including the awkward cases (NaN / infinity → `Float.NaN` etc., control-character escaping). |
| `attrs.rs` | Read the attributes a signature skeleton needs: `ConstantValue` (→ field initializer, a boolean's `1`→`true`), `Exceptions` (→ non-generic `throws`), `MethodParameters` / `LocalVariableTable` (→ real, slot-aware parameter names). |
| `expr.rs` | The expression / statement IR (`Expr`, `Stmt`) and its rendering to indented Java, with conservative parenthesization so the grouping the bytecode evaluated is preserved. |
| `cfg.rs` | Control-flow graph construction: reconstruct instruction offsets with `Instruction::encoded_len`, find leaders, and cut the code into basic blocks with typed terminators. Bails on `switch` / `jsr` / a branch to a non-instruction offset. |
| `body.rs` | The decompiler proper: a per-block stack machine (`Sim`) that folds bytecode into the IR, and a `Structurer` that recovers structured Java (`if` / `if`-`else`, `while` / `do`-`while`) from the CFG. `decompile_method_body` is the entry point. |

## How a method body is reconstructed

1. **Locate the `Code` attribute.** Bail if there is an exception table (`try`/`catch` is not yet
   structured).
2. **Map parameter slots to names** from the signature's parameter names, bailing on a count mismatch
   so the body never references a parameter the signature does not declare.
3. **Build the CFG** (`cfg::build`): per-instruction offsets via `Instruction::encoded_len`, leaders
   at the entry / every branch target / after every branch or exit, then basic blocks with `Fall` /
   `Goto` / `Branch` / `Ret` / `Throw` terminators.
4. **Structure the CFG** (`Structurer`): walk the blocks as a single-entry region, running each block
   through the stack machine and folding forward conditional branches into `if` / `if`-`else` and
   natural loops (a block targeted by a back-edge) into `while` / `do`-`while`. A forward `if`
   condition is the *negation* of the branch's jump test (the branch skips the `then` body); a
   top-test `while` condition is likewise the negation of its exit branch, while a `do`-`while`
   condition is the branch's own (positive) test (it jumps back to repeat). A
   `lcmp`/`fcmpl`/`fcmpg`/`dcmpl`/`dcmpg` ending a conditional-branch block is read together with
   that branch as one source `long`/`float`/`double` comparison (see M7). A block reached more than
   once, a `break`/`continue` edge, or any other non-tree/irreducible shape bails. A final check
   requires **every block to be emitted exactly once** — a strong guard that the recovered tree
   matches the real control flow.
5. **Render** the statement tree to indented Java lines, dropping a trailing implicit `return;`.

The stack machine (`Sim`) models: `this` / local (parameter and declared-local) loads and stores,
`iinc`, constants, field get/put, `invokevirtual` / `invokeinterface` / `invokestatic` /
`invokespecial` (including `new`+`dup`+`<init>` object creation and `super(...)` / `this(...)`
constructor chaining), integer/long/float/double arithmetic and bitwise ops, numeric conversions (as
casts), `checkcast` (including array types), `arraylength`, array element reads/writes (`arr[i]` /
`arr[i] = v`), array creation (`newarray` / `anewarray` / `multianewarray` → `new int[n]`,
`new int[a][b]`, `new int[n][]`) with `javac`'s element-store runs folded back into `new T[]{…}`
initializers, string concatenation (both the `invokedynamic` `StringConcatFactory` call sites and
`StringBuilder` append chains, folded back to `a + b + …`), `return` / `athrow`, and a discarded
call / object creation result (`pop` →
statement). Local variables are recovered by hoisting: every non-parameter slot a method stores into
gets one typed declaration (name + type from the `LocalVariableTable`) at the method-body top, and
each store becomes a plain assignment — so a local written inside a branch and read after the join
stays in scope. A method with a stored local the `LocalVariableTable` cannot name/type (a `-g`-less
build, a synthetic temporary, or a reused slot) falls back to the safe body.

## Implementation progress

Delivered as independent, mergeable milestones. Each ships real value and, thanks to the safe-fallback
invariant, never regresses the "always valid Java" guarantee.

### M0 — attribute enrichment + safe body frames &nbsp;✅ done

No bytecode analysis. Fills in the member details a signature needs and gives every method a body
suited to its shape:

- `ConstantValue` field initializers across every constant kind (`static final int MAX = 42;`,
  a boolean's `1` → `true`, `long`/`float`/`double` suffixes, escaped strings).
- Declared checked exceptions from the `Exceptions` attribute (`throws java.io.IOException`).
- Real parameter names from `MethodParameters` / `LocalVariableTable` (else `argN`).
- Placeholder bodies: `;` for `abstract`/`native`, `{}` for `void`/constructor, and
  `{ throw new RuntimeException(); }` for a value-returning method.

### M1 — straight-line method bodies &nbsp;✅ done

Reconstructs the real body of a **single-basic-block** method from its bytecode via the stack machine:
getters, setters, field-storing constructors, arithmetic returns, `throw`s, and object creation become
actual Java. A method with any branch, or any unsupported instruction, falls back to M0.

```java
public T get() { return this.value; }
public Consts(int start) { this.count = start; }
public int add(int delta) { return this.count + delta; }
public void risky(java.lang.String path) throws java.io.IOException {
    throw new java.io.IOException(path);
}
```

### M2 — control-flow structuring &nbsp;✅ done

Adds `Instruction::encoded_len` (in `jals-classfile`), CFG construction (`cfg.rs`), and the
`Structurer`. Delivers **forward conditional branches → `if` / `if`-`else`.**

```java
public int max(int a, int b) {
    if (a > b) {
        return a;
    }
    return b;
}
public void classify(int n) {
    if (n < 0) {
        this.value = -1;
    } else {
        this.value = 1;
    }
    this.value = this.value + 1;
}
```

### M3 — local variables &nbsp;✅ done

Recovers local variables via **hoisting**: every non-parameter slot a method stores into gets one
typed declaration (name + type read from the `LocalVariableTable`) at the method-body top, and each
`istore` / `astore` / … becomes a plain assignment; `iinc` becomes `i = i + n;`. Hoisting keeps a
local written inside a branch and read after the join in scope, so the output is always valid Java.
A method with a stored local the `LocalVariableTable` cannot name/type unambiguously — a `-g`-less
build, a synthetic temporary, or a reused slot — falls back to the M0 safe body.

```java
public int compute(int n) {
    int doubled;
    int result;
    doubled = n * 2;
    result = doubled + 1;
    return result;
}
public int pick(boolean c) {
    int x;
    if (c) {
        x = 1;
    } else {
        x = 2;
    }
    return x;
}
```

### M4 — loops &nbsp;✅ done

Recovers natural loops from back-edges (via `Instruction::encoded_len` offsets) into the two shapes
`javac` emits: a **top-test `while`** (a condition test with a `goto` back-edge — javac's default
loop layout) and a **`do`-`while`** (a conditional back-branch at the bottom). The loop body reuses
the M3 local machinery, so a counter (`istore` + `iinc` + `iload`) reconstructs cleanly. A
`break`/`continue`, a nested/irreducible shape, or a side-effecting loop header falls back to the M0
placeholder. `for` is rendered as `while` for now.

```java
public int sum(int n) {
    int total;
    int i;
    total = 0;
    i = 0;
    while (i < n) {
        total = total + i;
        i = i + 1;
    }
    return total;
}
public int count(int n) {
    int c;
    c = 0;
    do {
        c = c + 1;
    } while (c < n);
    return c;
}
```

### M5 — array operations &nbsp;✅ done

Adds the array instruction family to the stack machine: element reads/writes (`*aload` / `*astore`,
all eight flavors → `arr[i]` / `arr[i] = v;`), creation (`newarray` / `anewarray` /
`multianewarray` → `new int[n]`, `new java.lang.String[n]`, `new int[a][b]`, `new int[n][]`), and
array-typed `checkcast` (`(int[]) o`). `javac`'s initializer pattern — a constant-length creation
followed by `dup; <index>; <value>; Xastore` runs — is folded back into a `new T[]{…}` initializer
(nested initializers compose), with a `boolean[]`'s stored int constants mapped back to
`true`/`false`. The fold only fires on the exact complete, sequential-from-zero shape: a partial or
out-of-order fill (a default-skipping compiler like ECJ) bails rather than render an initializer of
the wrong length, and a compound element store (`xs[i]++`, which compiles to `dup2`) still falls
back to the M0 placeholder.

```java
public int firstTwo() {
    int[] xs;
    xs = new int[]{3, 4};
    return xs[0] + xs[1];
}
public int[][] grid(int a, int b) {
    return new int[a][b];
}
public boolean[] flags() {
    return new boolean[]{true, false};
}
```

### M6 — string concatenation &nbsp;✅ done

Folds both lowerings of a source string concatenation back into the `a + b + …` it came from:

- **`invokedynamic`** (javac's default since 9): an `InvokeDynamic` call site whose bootstrap is
  `java.lang.invoke.StringConcatFactory.makeConcatWithConstants` (or the recipe-free `makeConcat`)
  resolves its recipe through the class's `BootstrapMethods` attribute, then interleaves the
  recipe's literal chunks with the stacked dynamic operands (each `\u0001`) and any trailing
  bootstrap-argument constants (each `\u0002` — how javac passes a constant that itself contains a
  marker char). Any other bootstrap (a lambda / method reference) still bails.
- **`StringBuilder` chains** (javac `-XDstringConcat=inline`, older compilers, hand-written code):
  a concat-safe `append` run on a *fresh* `new StringBuilder()` collects like an array initializer
  and folds when its `toString()` is consumed. Only appends whose operand type string-converts
  exactly like a `+` operand fold (primitives, `String`, `Object`, `CharSequence` — *not*
  `char[]`, which appends characters); a chain consumed any other way (no `toString()`, `.length()`,
  discarded, or built on a local/parameter receiver) re-renders as the original calls.

Faithfulness guards: the rendered chain is only a *string* concatenation if a `String`-typed operand
anchors its first `+`, so a fold with no such anchor is seeded with `""` (recovering `a + "" + b`,
whose empty constant vanishes from the recipe), and a `boolean`/`char` operand that reaches the
concat as an int constant is re-rendered as `true`/`'x'`.

```java
public java.lang.String greet(java.lang.String name) {
    return "Hello, " + name + "!";
}
public java.lang.String bare(int a, int b) {
    return "" + a + b;
}
public java.lang.String excl(java.lang.String s) {
    return s + '!';
}
```

### M7 — numeric comparison conditions &nbsp;✅ done

Recovers `long`/`float`/`double` comparisons in conditions. The JVM lowers `a < b` on these types to
a two-instruction pair — `lcmp`/`fcmpl`/`fcmpg`/`dcmpl`/`dcmpg` pushes -1/0/1, and the following
`if<cond>` tests it against zero — so the structurer reads the pair back as one source comparison:
the `*cmp`'s two operands become `lhs <op> rhs` with the operator taken from the branch (negated for
a fall-through condition, exactly like the int comparisons), in `if` / `if`-`else` and `while` /
`do`-`while` conditions alike.

Faithfulness guard: the two float/double flavors differ only in what either operand being NaN pushes
(`*cmpl` -1, `*cmpg` +1), so a rendered ordering operator whose true side would capture NaN is not
what the bytecode computes. `javac` always drops NaN on the false side (`<`/`<=` compile to `*cmpg`,
`>`/`>=` to `*cmpl`), which reads back exactly; a mismatched pairing — e.g. `!(a < b)`, which is
*true* on NaN and compiles to `fcmpg` + `iflt` — has no single-operator rendering and bails. A `*cmp`
whose result is not consumed by its block's conditional branch (a ternary's value merge, a stored
comparison) also still bails.

```java
public long max(long a, long b) {
    if (a > b) {
        return a;
    }
    return b;
}
public double halve(double d) {
    while (d > 1d) {
        d = d / 2d;
    }
    return d;
}
```

Still falling back to the M0 placeholder (see roadmap): `switch`, `try`/`catch`.

## Supported vs. not (yet)

| Area | Supported | Falls back |
| --- | --- | --- |
| Values / expressions | `this` & parameter/local loads and stores (`istore`/`astore`/… + hoisted declarations), `iinc`, constants (`ldc` int/long/float/double/String/Class), field get/put (instance & static), method calls (virtual/interface/static/special), `new X(...)`, arithmetic / bitwise / shifts, numeric conversions (casts), `checkcast` (incl. array types), `arraylength`, array element load/store (`arr[i]`), array creation (`newarray`/`anewarray`/`multianewarray`) with folded `new T[]{…}` initializers, string concatenation (`invokedynamic` `makeConcatWithConstants`/`makeConcat` recipes and concat-safe `StringBuilder` append chains → `a + b + …`, with a `""` seed when no `String` operand anchors the chain), `super(...)` / `this(...)` | a partial / non-sequential array-initializer store run (a default-skipping compiler, e.g. ECJ), compound element assignment (`arr[i]++` / `arr[i] += v` — `dup2`), a non-concat `invokedynamic` (lambdas, method refs), a non-`String` concat bootstrap constant, `monitor*` (`synchronized`), a `*cmp` whose result is not consumed by its block's conditional branch (a ternary's value merge, a stored comparison), `instanceof`, `dup` (except in `new` and array initializers), `swap`, `wide` loads, a local with no usable `LocalVariableTable` entry (no `-g`, synthetic, or reused slot) |
| Control flow | straight-line, forward `if` / `if`-`else`, `while` / `do`-`while` loops (int/ref comparisons, long/float/double comparisons via a `lcmp`/`fcmp*`/`dcmp*` fused into the following `if<cond>`, null checks, `< 0` vs zero, boolean) | `break` / `continue`, nested / irreducible loops, a side-effecting loop header, a NaN-inexact fused `*cmp` (`!(a < b)`), `switch`, `try`/`catch`/`finally`, any other non-tree shape |

Everything in the "falls back" columns makes the method fall back to the M0 safe body — always valid
Java, just not (yet) a real body.

## Roadmap

Remaining milestones, roughly in priority order. Each is an independent increment gated by the same
safe-fallback invariant. (Local variables — the old first roadmap entry — shipped as M3, and loops as
M4 on top of it, since a loop's induction variable is itself a local.)

- **`switch`** — structure `tableswitch` / `lookupswitch` into a `switch` statement.
- **`try` / `catch` / `finally`** — structure the exception table.
- **Richer loops** — `break` / `continue` (labeled), nested loops, and `for`-loop recovery (folding
  the init / update back into a `for` header instead of rendering as `while`).
- **Expression polish** — ternary (`?:`) and short-circuit (`&&` / `||`) from small diamonds
  (string concatenation shipped as M6, long/float/double comparisons in conditions as M7),
  enhanced-`for`, `instanceof`, and `i++` / `i += n` / `arr[i]++` sugar (the `dup2` stack shuffles).

## Testing

- `cargo test -p jals-decompile` — unit tests (`literal.rs`, `attrs.rs`, the `Instruction::encoded_len`
  round-trip lives in `jals-classfile`) and integration tests (`tests/body.rs`) that run
  `decompile_method_body` over real compiled fixtures (`Consts.class`, `Branchy.class`, `Locals.class`,
  `Loops.class`, `Arrays.class`, `Concat.class`, `Sb.class`, `Cmp.class`, `IntCarried.class`) and
  assert the recovered statements.
- The end-to-end skeleton rendering and the **valid-Java property test** live in
  `jals-classpath/tests/decompile.rs`, which parses every rendered skeleton from the fixture corpus
  and asserts zero syntax errors.
- Fixtures are pre-compiled `.class` files committed under `jals-classpath/tests/fixtures/` (see its
  `README.md` for provenance / how to regenerate with `javac`).
