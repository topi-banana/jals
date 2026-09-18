# `jals-platform`

The Java standard library `jals` ships: `java.lang`, `java.util` and `java.io`, written in Java,
behind a handful of host functions.

It is a [`jals-native`](../jals-native) package like any other — carrying the same
[`JavaSource`](https://docs.rs/jals-native) values and the same binding keys — and that is
deliberate. A standard library privileged into the crate that defines what a package *is* would be
a second way to publish Java, and the second way is always the one that drifts.

## One text, two readers

Every consumer reads the same Java. What differs is how faithfully those declarations are to what
will run, and that is a property of the **route**, not of the text:

| consumer | reads | as |
| --- | --- | --- |
| a `jals-wasm` build that links this | every unit | `Complete` — what it does not declare, the program does not have |
| a `javac` build, an editor session, `jals lint` | every unit | `Signatures` — the real JDK is a superset |
| any build's compile step | implementation units only | the Java it lowers |

What this replaces is a hand-written signature copy of the same API sitting beside the
implementation, with a test diffing their member sets and a comment asking maintainers not to edit
one to match the other. There is one member set now, so there is nothing to keep in step.

## The built-in set

`Builtin::packages(host)` is the one place the set is written, and every host calls it: `jals
build`, `jals run`, `jals test`, `jals lint`, the language server and the browser playground. A
package added there is offered by all of them at once — and, because the same `JavaPackage` values
are what an index reads, by the editor beside the build too. Which of them a *project* selects is
still the manifest's answer (`[build] platform`, `[build] native-packages`).

| package | what | selected by |
| --- | --- | --- |
| `java.base` | `java.lang`, `java.util`, `java.io` | `[build] platform`, which defaults to it |
| `jals.io` | a `char[]`-based printer, the third-party-package demonstration | `[build] native-packages` |

## The host functions

Every one is an operation **Java cannot express** and a dependency-free `no_std` crate can. A thing
this package could do in Java, it does in Java — which is why `Math.sqrt` and every integer text
conversion are on the other side of the seam.

| count | what | why Java cannot |
| --- | --- | --- |
| 2 | `PrintStream.writeUnits`, `flushStream` | there is no output on this target but the host's |
| 2 | `System.currentTimeMillis`, `nanoTime` | there is no clock on this target but the host's |
| 4 | `Double`/`Float` bit casts, both directions | a reinterpretation is not an arithmetic operation |
| 4 | `Double`/`Float` render and parse | the shortest round-tripping decimal is a hard problem `core` already solves |
| 7 | `ArrayList`'s storage | a `Vec` is not a thing Java can name |

`tests/bindings.rs` asserts that count against `JavaPackage::binding_count`. A number stated in a
document and checked by nothing is a number that will be wrong.

A binding **cannot allocate**: a wasm embedder has no `struct.new` of its own. So the two that
produce text write into an array *the module* allocated and return how many characters they wrote,
which is why `Double.toChars` takes a `char[]` and `Double.toString` is the Java wrapper around it.

## The native classes

`java.util.ArrayList` is a **native class**: an instance carries an `int` handle naming an entry in
the host's table, and its methods are thin wrappers over seven `static native` ones. The storage is
a `Vec<HostValue>` in Rust, and the elements are Java references the host **roots** — so an object
added in one native call comes back out of a later one as the same object, and the collector cannot
reclaim it in between.

`indexOf`, `contains` and `remove(Object)` are *not* host functions: they are loops over `getElement`
written in Java, because their equality is `Object.equals` and that is a dispatch the module already
performs (`String` compares by value, a class that overrides nothing by reference). The bounds
checks are Java too, so `list.get(0)` on an empty list throws `IndexOutOfBoundsException` a program
can catch rather than trapping.

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
- **`Map`, `Set`, `HashMap`, `HashSet`, `Optional` and `Objects` are declarations only.** A
  container nobody has implemented is a type a program can still *name*, which is what an editor
  needs; it is not a type a module can call.
- **Reflection, and a body for `Enum` or `Record`.** A constant's `ordinal()` and a record's
  accessors are synthesised per declaration, so there is no single body either could carry. Both are
  *declared*, so a program can still name them.
- **`Math`'s transcendentals.** `sin`, `exp` and `log` need either a polynomial table this package
  would have to be trusted about or a host binding each. `sqrt` is exact and is here — reduce into
  `[1, 4)`, five Newton passes, then a Dekker exact-residual correction. It agrees with the JDK on
  every input tried. `hypot` does not, and that is stated rather than hidden: it rounds three times
  where the JDK rounds once, so it is within about two units in the last place rather than the one
  the JDK's javadoc promises.
- **Full Unicode case mapping.** `Character` and `String` map ASCII and say so, rather than shipping
  a half-Unicode answer that looks general.
- **`System.exit`.** A wasm module does not *run*: it is called, and it returns.
- **`Thread`, `Runtime` and `Process` are declarations only.** A module has one thread, which it
  does not own, and no process to start or halt. They are here so a `javac` build's analysis
  resolves the JDK types a project names.
- **`StringBuffer` and `Cloneable` are declarations too**, for two different reasons. An
  implementation of the first would be `StringBuilder` under a second name — a module has one thread
  and nothing for the synchronization to guard — and the second declares no member at all. Both are
  here because a program that names one is writing correct Java.
- **Stack traces.** There is no walkable frame list, which is why `Throwable` renders through
  `typeName()` — a method every subclass overrides — instead of `getClass().getName()`.

## Where the public API is not the JDK's

Every `javac` build reads this Java as its record of the JDK, and a `jals`-backend build emits calls
from it verbatim. So every **public** member has to be one the JDK declares with the same descriptor,
and `jals-javac/tests/stdlib_oracle.rs` checks exactly that against `ct.sym`. Two ledgers name the
exceptions and fail in both directions, so they can only shrink: `DIVERGENCES` for the members the
JDK does not declare, and `PRIVATE_TYPES` for the nested implementation types `ct.sym` does not
record because the JDK keeps its own private.

Private and package-private members are this package's implementation and are not compared. Neither
is `protected` — `typeName()` among them — because the index gives it no bit of its own.

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
