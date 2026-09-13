# `jals-platform`

The Java standard library `jals` ships: `java.lang` and `java.io`, written in Java, behind twelve
host functions.

It is a [`jals-native`](../jals-native) package like any other — found through the same resolver
chain a third party's is, carrying the same `JavaSource` values — and that is deliberate. A standard
library privileged into the crate that defines what a package *is* would be a second way to publish
Java, and the second way is always the one that drifts.

## One text, two tiers

Every consumer reads the same Java. What differs is how faithful those declarations are to what will
run, and that is a property of the **route**, not of the text:

| consumer | reads | as |
| --- | --- | --- |
| a `jals-wasm` build that links this | every unit | `Complete` — what it does not declare, the program does not have |
| a `javac` build, an editor session, `jals lint` | every unit | `Signatures` — the real JDK is a superset |
| any build's compile step | implementation units only | the Java it lowers |

What this replaces is a hand-written signature copy of the same API sitting beside the
implementation, with a test diffing their member sets and a comment asking maintainers not to edit
one to match the other. There is one member set now, so there is nothing to keep in step.

## The twelve host functions

Every one is an operation **Java cannot express** and a dependency-free `no_std` crate can. A thing
this package could do in Java, it does in Java — which is why `Math.sqrt` and every integer text
conversion are on the other side of the seam.

| count | what | why Java cannot |
| --- | --- | --- |
| 2 | `PrintStream.writeUnits`, `flushStream` | there is no output on this target but the host's |
| 2 | `System.currentTimeMillis`, `nanoTime` | there is no clock on this target but the host's |
| 4 | `Double`/`Float` bit casts, both directions | a reinterpretation is not an arithmetic operation |
| 4 | `Double`/`Float` render and parse | the shortest round-tripping decimal is a hard problem `core` already solves |

`tests/bindings.rs` asserts that count against `JavaPackage::binding_count`. A number stated in a
document and checked by nothing is a number that will be wrong.

A binding **cannot allocate**: a wasm embedder has no `struct.new` of its own. So the two that
produce text write into an array *the module* allocated and return how many characters they wrote,
which is why `Double.toChars` takes a `char[]` and `Double.toString` is the Java wrapper around it.

## The host supplies the state

| host | `System.out` | `System.err` | clock |
| --- | --- | --- | --- |
| `jals run` | stdout, through `Shell` | stderr | the wall clock, and an `Instant` taken at start-up |
| the playground | the Run pane | the Run pane | none — both readings are zero |
| the language server | discarded | discarded | none |
| tests | a `String` | a separate `String` | whatever `CapturedHost::at` was given |

A host with no clock is a **real host**, not a broken one: a language server instantiates nothing, so
there is no run for a clock to time. Both clock methods default to zero for that reason.

The two streams stay apart in this crate whatever the host does with them. A browser tab has one
place to show text and joins them — but that is a decision it makes, with a place to be made, and
not one this crate makes for it.

## What is deliberately not here

Each because of what the target is, and each stated in the class that would have carried it.

- **`java.lang.Object` has no implementation, and must not.** It *is* the wasm backend's `anyref`,
  answered for before the backend consults its struct table — so a declared `Object` with fields
  would be one question with two answers: a field present on some instances and not others. It is a
  signature unit, and the tier is what enforces it. A compile takes only implementation units, so
  there is no way to hand it that file, and nothing has to remember a rule.
- **`java.util` is declarations only.** A `List` nobody has implemented is a type a program can
  still *name*, which is what an editor needs; it is not a type a module can call.
- **Reflection, `Enum`, `Record`.** Each needs metadata the backend does not emit.
- **`Math`'s transcendentals.** `sin`, `exp` and `log` need either a polynomial table this package
  would have to be trusted about or a host binding each. `sqrt` is exact and is here — reduce into
  `[1, 4)`, five Newton passes, then a Dekker exact-residual correction. It agrees with the JDK on
  every input tried but `Double.MAX_VALUE`, where the reduction's scaling overflows; that is stated
  rather than hidden.
- **Full Unicode case mapping.** `Character` and `String` map ASCII and say so, rather than shipping
  a half-Unicode answer that looks general.
- **`System.exit`.** A wasm module does not *run*: it is called, and it returns.
- **Stack traces.** There is no walkable frame list, which is why `Throwable` renders through
  `typeName()` — a method every subclass overrides — instead of `getClass().getName()`.

## Constants are `char[]`

Every constant string in this package is written as a `char[]` initialiser and wrapped once in a
`static` field. That is not a style: the backend refuses a string literal, so it is the only way this
target spells one. When that gap closes, this is the diff that closes with it.

## The source list is generated

`src/sources.rs` is written by `cargo run -p xtask -- codegen`, and CI runs the same command with
`--check`. So a `.java` nobody listed is a build failure rather than a file that silently is not part
of the package, and moving one from `signatures` to `implementation` is a visible diff that says
somebody implemented a type.

Which tier a file is in is `xtask`'s own list, not something inferred from the Java. "Has a body" is
the wrong question: an interface's methods have none and interfaces *are* compiled, while `Object`
could be given bodies and must still never be lowered.

## Held honest by its tests

`cargo hawk check` excludes this crate the way it excludes `jals-native` and `jinja`: its API is what
a *host* is offered rather than what one consumer happens to call. `tests/` is what sizes it
instead — a published item lands with the test that drives it, or it lands unreachable with nothing
reporting it.
