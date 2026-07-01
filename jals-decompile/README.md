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

```rust
// Method-body decompilation. `param_names` are the exact names the signature renders (in order);
// the body reuses them and bails on a count mismatch (e.g. an enum ctor's synthetic `String, int`).
// Returns indented Java statement lines, or `None` to fall back to a safe placeholder body.
pub fn decompile_method_body(method: &MethodInfo, cf: &ClassFile, param_names: &[String])
    -> Option<Vec<String>>;

// Attribute readers a signature skeleton needs but bytecode analysis does not.
pub fn constant_value_initializer(field: &FieldInfo, pool: &ConstantPool) -> Option<String>;
pub fn declared_throws(method: &MethodInfo, pool: &ConstantPool) -> Vec<String>;
pub fn parameter_names(method: &MethodInfo, pool: &ConstantPool, is_static: bool, arity: usize)
    -> Option<Vec<String>>;

// The descriptor / generic-signature → Java type vocabulary, shared with `skeleton.rs`.
pub fn internal_to_java(internal: &str) -> String;          // a/b/Outer$Inner -> a.b.Outer.Inner
pub fn render_field_type(ft: &FieldType) -> String;         // [Ljava/lang/String; -> java.lang.String[]
pub fn render_type_sig(ts: &TypeSignature) -> String;       // generic type signature -> Java
pub fn render_class_type_sig(c: &ClassTypeSignature) -> String;
pub fn render_type_args(args: &[TypeArgument]) -> String;
pub fn render_type_arg(arg: &TypeArgument) -> String;
pub fn render_throws(t: &ThrowsSignature) -> String;
pub fn is_object_sig(ts: &TypeSignature) -> bool;
```

## Modules

| Module | Role |
| --- | --- |
| `types.rs` | Render a field / method descriptor or generic signature to a well-formed Java type reference. The vocabulary shared with `jals-classpath`'s `skeleton.rs`. |
| `literal.rs` | Render constant-pool constants (`int`/`long`/`float`/`double`/`String`/`Class`) as valid Java literals, including the awkward cases (NaN / infinity → `Float.NaN` etc., control-character escaping). |
| `attrs.rs` | Read the attributes a signature skeleton needs: `ConstantValue` (→ field initializer, a boolean's `1`→`true`), `Exceptions` (→ non-generic `throws`), `MethodParameters` / `LocalVariableTable` (→ real, slot-aware parameter names). |
| `expr.rs` | The expression / statement IR (`Expr`, `Stmt`) and its rendering to indented Java, with conservative parenthesization so the grouping the bytecode evaluated is preserved. |
| `cfg.rs` | Control-flow graph construction: reconstruct instruction offsets with `Instruction::encoded_len`, find leaders, and cut the code into basic blocks with typed terminators. Bails on `switch` / `jsr` / a branch to a non-instruction offset. |
| `body.rs` | The decompiler proper: a per-block stack machine (`Sim`) that folds bytecode into the IR, and a `Structurer` that recovers structured Java (`if` / `if`-`else`) from the CFG. `decompile_method_body` is the entry point. |

## How a method body is reconstructed

1. **Locate the `Code` attribute.** Bail if there is an exception table (`try`/`catch` is not yet
   structured).
2. **Map parameter slots to names** from the signature's parameter names, bailing on a count mismatch
   so the body never references a parameter the signature does not declare.
3. **Build the CFG** (`cfg::build`): per-instruction offsets via `Instruction::encoded_len`, leaders
   at the entry / every branch target / after every branch or exit, then basic blocks with `Fall` /
   `Goto` / `Branch` / `Ret` / `Throw` terminators.
4. **Structure the CFG** (`Structurer`): walk the blocks as a single-entry region, running each block
   through the stack machine and folding forward conditional branches into `if` / `if`-`else`. The
   `if` condition is recovered as the *negation* of the branch's jump test (the branch jumps to skip
   the `then` body). A block reached more than once, a back-edge (loop), or any other non-tree shape
   bails. A final check requires **every block to be emitted exactly once** — a strong guard that the
   recovered tree matches the real control flow.
5. **Render** the statement tree to indented Java lines, dropping a trailing implicit `return;`.

The stack machine (`Sim`) models: `this` / local (parameter) loads, constants, field get/put,
`invokevirtual` / `invokeinterface` / `invokestatic` / `invokespecial` (including `new`+`dup`+`<init>`
object creation and `super(...)` / `this(...)` constructor chaining), integer/long/float/double
arithmetic and bitwise ops, numeric conversions (as casts), `checkcast`, `arraylength`, `return` /
`athrow`, and a discarded call result (`pop` → statement).

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

### M2 — control-flow structuring &nbsp;🚧 in progress

Adds `Instruction::encoded_len` (in `jals-classfile`), CFG construction (`cfg.rs`), and the
`Structurer`. Done so far: **forward conditional branches → `if` / `if`-`else`.**

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

Still falling back to the M0 placeholder (see roadmap): loops, `switch`, `try`/`catch`, and local
variable declarations.

## Supported vs. not (yet)

| Area | Supported | Falls back |
| --- | --- | --- |
| Values / expressions | `this` & parameter loads, constants (`ldc` int/long/float/double/String/Class), field get/put (instance & static), method calls (virtual/interface/static/special), `new X(...)`, arithmetic / bitwise / shifts, numeric conversions (casts), `checkcast`, `arraylength`, `super(...)` / `this(...)` | local stores (`istore` …), array load/store & `newarray`, `invokedynamic` (string concat, lambdas, method refs), `monitor*` (`synchronized`), `lcmp`/`fcmp`/`dcmp`, `instanceof`, `iinc`, `dup` (except in `new`), `swap`, `wide` |
| Control flow | straight-line, forward `if` / `if`-`else` (int/ref comparisons, null checks, `< 0` vs zero, boolean) | loops (back-edges), `switch`, `try`/`catch`/`finally`, any non-tree / irreducible shape |

Everything in the "falls back" columns makes the method fall back to the M0 safe body — always valid
Java, just not (yet) a real body.

## Roadmap

Remaining milestones, roughly in priority order. Each is an independent increment gated by the same
safe-fallback invariant.

- **Loops** — detect natural loops (back-edges via `Instruction::encoded_len` offsets) and recover
  `while` / `for` / `do`-`while`.
- **Local variables** — model local stores (`istore` …) as declarations / assignments, typed and named
  from `LocalVariableTable` where available. This unblocks a large class of real method bodies (both
  straight-line and inside branches).
- **`switch`** — structure `tableswitch` / `lookupswitch` into a `switch` statement.
- **`try` / `catch` / `finally`** — structure the exception table.
- **Expression polish** — string concatenation (`invokedynamic` `makeConcat*` / `StringBuilder`),
  ternary (`?:`) and short-circuit (`&&` / `||`) from small diamonds, enhanced-`for`, `instanceof`,
  array operations, and long/float/double comparisons in conditions.

## Testing

- `cargo test -p jals-decompile` — unit tests (`literal.rs`, `attrs.rs`, the `Instruction::encoded_len`
  round-trip lives in `jals-classfile`) and integration tests (`tests/body.rs`) that run
  `decompile_method_body` over real compiled fixtures (`Consts.class`, `Branchy.class`) and assert the
  recovered statements.
- The end-to-end skeleton rendering and the **valid-Java property test** live in
  `jals-classpath/tests/decompile.rs`, which parses every rendered skeleton (across this crate's
  fixtures plus `jals-classfile`'s round-trip corpus) and asserts zero syntax errors.
- Fixtures are pre-compiled `.class` files committed under `jals-classpath/tests/fixtures/` (see its
  `README.md` for provenance / how to regenerate with `javac`).
