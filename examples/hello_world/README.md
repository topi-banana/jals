# hello_world — a `jals-build` example

A minimal Java project driven entirely by [`jals-build`](../../jals-build) through the `jals`
CLI. It shows the full `jals.toml` → `javac`/`java` flow: a manifest, a `com.example` package
with two source files, and the four build subcommands.

## Layout

```
hello_world/
├── jals.toml                              # the manifest (package / build / run)
├── .gitignore                             # ignores /target (the build output)
└── src/main/java/com/example/
    ├── Main.java                          # [run] main-class — greets its args
    └── Greeter.java                       # a second file → multi-file compilation
```

## Run it

From this directory (or any subdirectory — `jals` discovers `jals.toml` upward, like Cargo):

```sh
# Compile src/main/java/**/*.java into target/classes with `javac --release 21`.
cargo run -p jals-cli -- build

# Print the exact javac command without compiling.
cargo run -p jals-cli -- build --dry-run

# Compile, then run com.example.Main with `java`.
cargo run -p jals-cli -- run
# → Hello, world!

# Pass program arguments through to main(String[] args).
cargo run -p jals-cli -- run -- Ada Linus
# → Hello, Ada!
# → Hello, Linus!

# Remove target/classes.
cargo run -p jals-cli -- clean
```

With an installed `jals` binary on `PATH`, drop the `cargo run -p jals-cli --` prefix (e.g.
`jals build`, `jals run -- Ada`).

## What the manifest demonstrates

- **`[build]` is explicit but optional** — the values written in `jals.toml` are the Maven-style
  defaults, so deleting the whole table builds identically. `release = 21` becomes
  `javac --release 21`.
- **Source discovery** — `jals` scans `source-dirs` for every `.java` file, so adding
  `Greeter.java` needs no manifest change; both files are compiled together.
- **`[run] main-class`** — `jals run` runs `com.example.Main` with a run classpath of
  `target/classes`. Override it per-invocation with `--main-class`.

See [`jals-build/README.md`](../../jals-build/README.md) for the complete manifest reference.
