# `jals-progress`

What a run is doing, as data.

Long work in this workspace happens in portable crates — a jar is fetched, remapped and decompiled
by `jals-classpath`, a task plan is executed by `jals-project`, a tree is compiled by `jals-build` —
while the only thing that can draw a progress bar is the host that started them. This crate is the
seam between the two, and it is deliberately small: an emitter says *what work it is doing*, and a
consumer decides what that looks like.

```rust
use std::sync::Arc;

use jals_progress::{Activity, Event, Outcome, Progress, Sink};

struct Trace;
impl Sink for Trace {
    fn emit(&self, event: &Event) {
        // A host's display, JSON stream, or timing ledger.
        let _ = event;
    }
}

let progress = Progress::to(Arc::new(Trace)).for_package(
    jals_progress::PackageRef::new("hello", Some("0.1.0")),
);
let task = progress.begin_bounded(Activity::Fetch, "client.jar", 52_428_800);
task.set_done(1_048_576);
task.finish(Outcome::Completed);
```

## What is here

| Item | What it is |
| --- | --- |
| `Activity`, `Outcome`, `PackageRef`, `Unit`, `Event` | The vocabulary. Facts about work, with no presentation in them. |
| `Progress` | The cheap-clone handle an emitter holds. Silent by default. |
| `Task` | One unit of work in flight. RAII: it ends exactly once. |
| `Ticker` | The counting half of a `Task`, for a `fan_out` worker. |
| `Sink` | Where events go. Implemented by the consumer. |
| `Timeline`, `Span`, `ReportMeta` | The ledger behind `--timings`, and its HTML and JSON renderings. |

## Four properties worth keeping

**Facts, not presentation.** `Activity` is `Fetch`, never "Downloading"; `Outcome` is `Fresh`, never
a colour. The terminal's verbs live in `jals-cli`, exactly as `jals-hir` states a fact and the
`jals-lint` rule that reports it owns the wording. `Activity::label` is the one concession, because
a written report has to put some word on a row — and it is a *name*, not a status verb.

**Silent by default, and free when silent.** `Progress::SILENT` allocates nothing and every method
on it is one branch. That is what makes it threadable through code a test, the browser, or the
language server drives with nobody watching, and what lets `Progress::is_live` guard a loop that
would otherwise format a string per archive member.

**A unit ends exactly once.** `Task::finish` states the outcome; `Task::fresh` lets a step deep
inside the work end its *caller's* unit from a memo hit; `Drop` reports `Outcome::Abandoned` for the
error path that returned without saying anything. `Abandoned` means the emitter has a hole in it and
not that the build failed, so an error path calls `finish(Outcome::Failed)` explicitly.

**No clock.** Portable code here cannot read one — the same reason `jals-classpath`'s retry jitter
is derived from a locator rather than drawn — so a host stamps each event as it arrives and hands
the number to `Timeline::record`. `cargo`'s own `--timings` records host-side for the same reason.

## Why it is a value and not part of `Exec`

`exec: &Exec` already reaches every function that wants to report, which is exactly the temptation.
Two things rule it out:

- `Exec` is `!Send`, so it cannot cross into the `fan_out` workers this is most worth reporting
  from — a jar's parallel decode, and a remap of tens of thousands of classes. `Ticker` exists for
  precisely that boundary.
- CPU crates in this workspace deliberately take **no** execution parameter at all: cooperative
  yielding is runtime-free so that parsing, inference and formatting never hold an `Exec`. Tying
  reporting to `Exec` would deny it to the crates most likely to want it next.

So `Progress` travels as its own value. Where a call already has an options struct it rides that
one — `TaskRuntime`, `GraphPreprocess`, `BackendRequest` — and where there is none it is a
parameter, next to the `Fetcher` it is threaded alongside.

## Portability

`no_std + alloc`, and featureless on purpose: there is no configuration in which this crate stops
being portable, so a plain `cargo check` is the portability check. Its only dependencies are `serde`
and `serde_json`, both already `no_std` here. The ledger and its HTML rendering are pure string
building, so CI's `wasm32-wasip1` cell runs this crate's tests with neither a terminal nor a clock
in reach.
