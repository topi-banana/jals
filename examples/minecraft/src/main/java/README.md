# This source root is generated

Nothing here is authored. `build.rhai` publishes three exclusively-owned roots into this directory —
`net/minecraft` from the decompiled game, and (with the `mixin` / `mixinextras` features)
`org/spongepowered` and `com/llamalad7` from the two libraries' sources jars — and `.gitignore`
keeps all three out of the repository.

This file exists so the directory does too. `jals.toml` declares no `[build] source-dirs`, so this
is the default source root; a publication destination has to be a strict descendant of a declared
source root, and a project that declares a source root it does not have is reported as such by
anyone depending on it — which, before the first build has published anything, is exactly this
directory.

There is deliberately no `Main.java`, and no `[run]` section in `jals.toml`: this example is meant to
be *depended on*, and a source dependency's own files are compiled into whoever consumes it, so a
type in the default package would collide with the consumer's.
