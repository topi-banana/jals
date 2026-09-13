# hello_world_wasm — a `jals-wasm` example

The same greeting as [`hello_world`](../hello_world), compiled to **one WebAssembly module** and
executed in the `jals` process itself — no JVM, and no wasm engine installed on the host.
`[build] backend = { type = "jals-wasm" }` selects the compiler; `jals run --invoke <name>` runs
what it emitted, through the `tinywasm` interpreter `jals-build` embeds behind its `wasm-run`
feature.

It exists to show the two things that are *different* about this target, both of which are
consequences of one fact: **the module is the whole world, and this project put no `java.base` in
it.** That is `platform = "none"` in `jals.toml` — the smallest artifact this backend produces, and
the third state `[build] platform` names. A wasm build links the platform by default;
`examples/hello_world_native` is this same greeting with it, printed through a real
`System.out.println`.

## There is no `String`, so there is no `println`

With no platform linked, a library type has no wasm representation at all — it is not even a name
that resolves — so `System.out.println("Hello, world!")` does not compile here. One line of manifest
configuration *does* make it (`platform` defaults to the platform, and this project turns it off),
which is the point: what follows is what that line buys, priced. The greeting is a `char[]`: a wasm array, allocated and owned by the host's
garbage collector, holding the code units in order.

The module can therefore *hold* the greeting but never *print* it. Turning code units back into
text is the caller's half of the job — which is what the loop below is, and why this example spells
its output out one call at a time instead of returning it.

## There is no `main`, so the entry point is a name

wasm has no entry-point convention, and Java's cannot be lowered: `main` takes a `String[]`, and
there is no `String`. So the entry point is an **exported function**. Every `static` method that is
not a constructor is exported — wider than `public`, and the name carries no owner, so
`Hello.greetingLength` is exported as plain `greetingLength`.

`[run] main-class` is not read at all under this backend, which is why this manifest declares none.

## Layout

```
hello_world_wasm/
├── jals.toml                              # [build] backend = { type = "jals-wasm" }
├── .gitignore                             # ignores /target (the build output)
└── src/main/java/com/example/
    ├── Hello.java                         # the two exports, and the module's static state
    └── Greeting.java                      # the char[] behind them → multi-file, one module
```

## Run it

From this directory (or any subdirectory — `jals` discovers `jals.toml` upward, like Cargo):

```sh
# Compile both sources into ONE module at target/classes/project.wasm.
cargo run -p jals-cli -- build

# Instantiate the module and stop. This is still a run: instantiating executes the
# start function, which is where the `static` initialiser that allocates the
# greeting went. Nothing reaches stdout, because nothing returned a value.
cargo run -p jals-cli -- run

# Call an export. The value lands on stdout; every status line stays on stderr.
cargo run -p jals-cli -- run --invoke greetingLength
# → 13

# Pass arguments to the export. They are unparsed here — the engine reads the
# export's signature and interprets each one against the parameter in its position.
cargo run -p jals-cli -- run --invoke greetingCharAt -- 0
# → 72   (the code unit for 'H')

# Print the module's plan without running it.
cargo run -p jals-cli -- run --invoke greetingLength --dry-run
# → jals-javac: 2 source(s) -> one WebAssembly module (host-managed memory)
# → tinywasm: instantiate the module, then invoke `greetingLength`

# Remove target/classes.
cargo run -p jals-cli -- clean
```

With an installed `jals` binary on `PATH`, drop the `cargo run -p jals-cli --` prefix.

## Reassembling the greeting

The module hands back code units, and the host is what has a notion of text. Thirteen calls and an
`awk` is the whole of it:

```sh
len=$(jals run --invoke greetingLength 2>/dev/null)
for i in $(seq 0 $((len - 1))); do
    jals run --invoke greetingCharAt -- "$i" 2>/dev/null
done | awk '{printf "%c", $1} END { print "" }'
# → Hello, world!
```

That indirection is the example.s point rather than an inconvenience to route around: it is exactly
what `platform = "none"` costs, made visible.

## What the two failure paths report

Both are worth trying, because each is the only evidence a caller gets for the thing it names.

```sh
# A name that is not exported reports the ones that are — the only way to see that two
# `static` methods of one name collided, since an export name carries no owner.
jals run --invoke absent
# → error: the module exports no function named `absent`;
#          it exports greetingLength, greetingCharAt

# The array bounds check belongs to the host, not to anything this backend emits.
jals run --invoke greetingCharAt -- 99
# → error: the call trapped: trap: out of bounds array access
```

## What the manifest demonstrates

- **`backend` selects the compiler, not a post-processing step** — `{ type = "jals-wasm" }` compiles
  every source at once into a single module. There is no per-type artifact, because wasm has no
  dynamic loading and no classpath for one to be loaded by: a call from `Hello` to `Greeting` is a
  plain `call` to a function index, which exists only because both were compiled together.
- **`classes-dir` is still where output lands** — `target/classes/project.wasm`, and still what
  `jals clean` removes.
- **No `release`, and no `[run] main-class`** — the first is a `javac` flag with no meaning here,
  and the second is not read under this backend. `jals run --main-class`, `--bin`, and arguments
  with no `--invoke` are all refused rather than ignored, and a `[run] main-class` left over from
  a previous backend is reported as ignored.

See [`jals-build/README.md`](../../jals-build/README.md) for the complete manifest reference, and
[`jals-javac`](../../jals-javac)'s `src/wasm/` for what the backend does and does not lower.
