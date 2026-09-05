//! Running what the `jals-wasm` backend emitted, in this process.
//!
//! The counterpart to [`Runtime`](crate::Runtime), and deliberately not a second implementation of
//! it: that seam hands a main class and a classpath to a `java` process, and every one of its types
//! is built on `std::path::PathBuf`. A wasm module has none of those. What running one needs is the
//! module's bytes, an export name, and the arguments for it — so this is its own request type, with
//! no host path in it, which is what lets the browser reach the same code `jals run` does.
//!
//! # There is no `main`
//!
//! wasm has no entry-point convention, and the one Java has cannot be lowered here: `main` takes a
//! `String[]`, and a wasm host has no `java.base` to supply `String`. So the entry point is
//! *named*: an exported function, called by the name the source spells it with. The
//! [`jals_javac::wasm`] backend exports every `static` method that is not a constructor, which is
//! wider than "public" and is why an export can turn out to take a parameter no command line can
//! write — a reference to an object the embedder's collector owns. That is refused with the
//! position that caused it rather than mis-parsed.
//!
//! Naming no export at all is still a run: instantiating a module executes its start function,
//! which is where this backend lowers a class's `static` initialisers.
//!
//! # One engine, no selection
//!
//! [`BackendSelection`](crate::BackendSelection) exists because three backends implement one
//! contract and the browser genuinely lacks one of them. Here there is one engine, portable, that
//! both hosts enable — so a trait and an `Absent` arm would be a vocabulary with no second
//! implementer and an unreachable branch. A second engine is when the seam is worth having.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use jals_progress::{Activity, Outcome, Progress};
use tinywasm::types::WasmType;
use tinywasm::{ExternItem, ModuleInstance, Store};

/// What to run, and what to call in it.
pub struct WasmRunRequest<'a> {
    /// The module, as the backend emitted it.
    pub module: &'a [u8],
    /// The exported function to call, or `None` to instantiate and stop.
    pub invoke: Option<&'a str>,
    /// The arguments for that export, unparsed.
    ///
    /// Text rather than typed values because the types are the *module's* to declare: the engine
    /// reads the export's signature and interprets each string against the parameter in its
    /// position. A caller that parsed them first would have to know the signature to do it, which
    /// is the thing it is calling this to find out.
    pub args: &'a [String],
    /// Where the run reports what it is doing.
    pub progress: &'a Progress,
}

/// One value a module handed back.
///
/// This crate's own vocabulary rather than the engine's, so that swapping the engine — which is
/// pinned to an unreleased revision — never reaches the two hosts that read this.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WasmValue {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    /// A reference came back. Not rendered as a value: a reference is an object the embedder's
    /// collector owns — this backend's `new` is a `struct.new` into it — so there is nothing
    /// outside the engine to print. Saying so beats printing an address that means nothing.
    Reference,
    /// A 128-bit vector. Nothing this backend emits returns one; the arm is here because a wasm
    /// function can, and silently dropping a result would be worse than naming it.
    Vector,
}

impl fmt::Display for WasmValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::I32(value) => write!(f, "{value}"),
            Self::I64(value) => write!(f, "{value}"),
            Self::F32(value) => write!(f, "{value}"),
            Self::F64(value) => write!(f, "{value}"),
            Self::Reference => f.write_str("<reference>"),
            Self::Vector => f.write_str("<v128>"),
        }
    }
}

/// What a run did.
#[derive(Debug, Clone, PartialEq)]
pub enum WasmRunOutcome {
    /// No export was named. The module was instantiated, which runs its start function — the
    /// lowering of every `static` initialiser the project declares.
    Instantiated,
    /// An export was called and returned these values. Empty for a `void` method.
    Returned(Vec<WasmValue>),
}

/// Why a run did not happen, or did not finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WasmRunError {
    /// The bytes are not a module this engine accepts.
    Parse(String),
    /// The module parsed but could not be instantiated — which includes a start function that
    /// trapped, since instantiating runs it.
    Instantiate(String),
    /// Nothing is exported under that name.
    ///
    /// Carries the names that *are* exported, because the one thing a caller cannot see from here
    /// is what happened to the name it asked for: an export name is bare, with no owner in it, so
    /// two `static` methods sharing a name — an overload pair, or one method per class — collide
    /// and the second is dropped when the module is built. The list is the only evidence of that.
    NoSuchExport {
        name: String,
        available: Vec<String>,
    },
    /// The export takes a parameter no command line can supply: a reference, or a vector.
    ///
    /// Not a defect in the module. Every `static` method that is not a constructor is exported, so
    /// a `static int get(Point p)` is exported exactly like a scalar one — it just cannot be
    /// reached this way.
    UnsupportedParameter {
        name: String,
        position: usize,
        ty: &'static str,
    },
    /// The export takes a different number of arguments than were given.
    ArgumentCount {
        name: String,
        expected: usize,
        given: usize,
    },
    /// An argument did not parse as the type the export declares in that position.
    Argument {
        name: String,
        position: usize,
        expected: &'static str,
        given: String,
    },
    /// The call trapped.
    Trap(String),
}

impl fmt::Display for WasmRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(message) => write!(f, "the module could not be parsed: {message}"),
            Self::Instantiate(message) => {
                write!(f, "the module could not be instantiated: {message}")
            }
            Self::NoSuchExport { name, available } => {
                write!(f, "the module exports no function named `{name}`")?;
                if available.is_empty() {
                    return f.write_str(" (it exports no functions at all)");
                }
                write!(f, "; it exports {}", available.join(", "))
            }
            Self::UnsupportedParameter { name, position, ty } => write!(
                f,
                "`{name}` takes {ty} at position {position}, which cannot be written as an argument"
            ),
            Self::ArgumentCount {
                name,
                expected,
                given,
            } => write!(f, "`{name}` takes {expected} argument(s) and got {given}"),
            Self::Argument {
                name,
                position,
                expected,
                given,
            } => write!(
                f,
                "argument {position} of `{name}` is {expected}, and `{given}` is not one"
            ),
            Self::Trap(message) => write!(f, "the call trapped: {message}"),
        }
    }
}

/// Runs a `jals-wasm` module with the embedded interpreter.
///
/// A namespace rather than a value: the engine holds no configuration of its own, and a `Store` is
/// built per run because a run is the whole lifetime of the module's state.
pub struct WasmRunner;

impl WasmRunner {
    /// Instantiate the module, and call the named export when there is one.
    pub fn run(request: &WasmRunRequest<'_>) -> Result<WasmRunOutcome, WasmRunError> {
        let task = request
            .progress
            .begin(Activity::Run, request.invoke.unwrap_or("module"));
        match Self::execute(request) {
            Ok(outcome) => {
                task.finish(Outcome::Completed);
                Ok(outcome)
            }
            // Explicit rather than left to `Drop`, which reports `Abandoned` — that says the
            // emitter has a hole in it, not that the run failed.
            Err(error) => {
                task.finish(Outcome::Failed);
                Err(error)
            }
        }
    }

    /// What [`run`](Self::run) would do, for `--dry-run`/`-v`.
    ///
    /// Takes the selection rather than a [`WasmRunRequest`], unlike
    /// [`Backend::describe`](crate::Backend::describe) which takes its request: a `--dry-run`
    /// compiles nothing, so at the point this is asked there are no module bytes to put in one.
    pub fn describe(invoke: Option<&str>, args: &[String]) -> String {
        match invoke {
            Some(name) if args.is_empty() => format!("tinywasm: invoke `{name}`"),
            Some(name) => format!("tinywasm: invoke `{name}` with {}", args.join(" ")),
            None => "tinywasm: instantiate the module, running its static initialisers".to_owned(),
        }
    }

    /// The run itself, so [`run`](Self::run) has one place to end the progress unit from.
    fn execute(request: &WasmRunRequest<'_>) -> Result<WasmRunOutcome, WasmRunError> {
        let module = tinywasm::parse_bytes(request.module)
            .map_err(|error| WasmRunError::Parse(error.to_string()))?;
        let mut store = Store::default();
        // Instantiating runs the start function, which is where a class's `static` initialisers
        // are lowered — so this is already an execution, whether or not an export is named next.
        let instance = ModuleInstance::instantiate(&mut store, &module, None)
            .map_err(|error| WasmRunError::Instantiate(error.to_string()))?;

        let Some(name) = request.invoke else {
            return Ok(WasmRunOutcome::Instantiated);
        };
        let func = instance
            .func_untyped(&store, name)
            .map_err(|_| WasmRunError::NoSuchExport {
                name: name.to_owned(),
                available: Self::exported_functions(&instance),
            })?;
        let signature = func
            .ty(&store)
            .map_err(|error| WasmRunError::Instantiate(error.to_string()))?;
        let params = signature.params().to_vec();
        let results = signature.results().len();
        if params.len() != request.args.len() {
            return Err(WasmRunError::ArgumentCount {
                name: name.to_owned(),
                expected: params.len(),
                given: request.args.len(),
            });
        }

        let mut arguments = Vec::with_capacity(params.len());
        for (position, (text, ty)) in request.args.iter().zip(&params).enumerate() {
            arguments.push(Self::argument(text, *ty, name, position)?);
        }
        // The engine writes into a buffer the caller sizes, and `tinywasm::WasmValue` has no
        // `Default` — the placeholder is overwritten by every result the call produces.
        let mut returned = vec![tinywasm::WasmValue::I32(0); results];
        func.call(&mut store, &arguments, &mut returned)
            .map_err(|error| WasmRunError::Trap(error.to_string()))?;
        Ok(WasmRunOutcome::Returned(
            returned.iter().map(Self::value).collect(),
        ))
    }

    /// The names of every function the module exports, in module order.
    fn exported_functions(instance: &ModuleInstance) -> Vec<String> {
        instance
            .exports()
            .filter_map(|(name, item)| matches!(item, ExternItem::Func(_)).then(|| name.to_owned()))
            .collect()
    }

    /// One argument, read against the type the export declares in that position.
    fn argument(
        text: &str,
        ty: WasmType,
        name: &str,
        position: usize,
    ) -> Result<tinywasm::WasmValue, WasmRunError> {
        let invalid = || WasmRunError::Argument {
            name: name.to_owned(),
            position,
            expected: Self::type_name(ty),
            given: text.to_owned(),
        };
        match ty {
            // `i32` covers Java's `boolean`, `byte`, `short`, `char` and `int` alike — the JVM and
            // wasm both widen all five — so an argument is read as the wasm type that is actually
            // there. A `boolean` is `0` or `1`, which is what the lowering stores.
            WasmType::I32 => text
                .parse()
                .map(tinywasm::WasmValue::I32)
                .map_err(|_| invalid()),
            WasmType::I64 => text
                .parse()
                .map(tinywasm::WasmValue::I64)
                .map_err(|_| invalid()),
            WasmType::F32 => text
                .parse()
                .map(tinywasm::WasmValue::F32)
                .map_err(|_| invalid()),
            WasmType::F64 => text
                .parse()
                .map(tinywasm::WasmValue::F64)
                .map_err(|_| invalid()),
            WasmType::V128 | WasmType::Ref(_) => Err(WasmRunError::UnsupportedParameter {
                name: name.to_owned(),
                position,
                ty: Self::type_name(ty),
            }),
        }
    }

    /// A parameter type's name, for a message.
    ///
    /// Written out rather than derived: the engine's types carry no `Debug` in this configuration,
    /// and a reference's rendering should say what a reader can act on — that it is an object,
    /// not which heap type it points at.
    const fn type_name(ty: WasmType) -> &'static str {
        match ty {
            WasmType::I32 => "an i32",
            WasmType::I64 => "an i64",
            WasmType::F32 => "an f32",
            WasmType::F64 => "an f64",
            WasmType::V128 => "a v128",
            WasmType::Ref(_) => "a reference",
        }
    }

    /// One returned value, in this crate's vocabulary.
    const fn value(value: &tinywasm::WasmValue) -> WasmValue {
        match value {
            tinywasm::WasmValue::I32(value) => WasmValue::I32(*value),
            tinywasm::WasmValue::I64(value) => WasmValue::I64(*value),
            tinywasm::WasmValue::F32(value) => WasmValue::F32(*value),
            tinywasm::WasmValue::F64(value) => WasmValue::F64(*value),
            tinywasm::WasmValue::V128(_) => WasmValue::Vector,
            tinywasm::WasmValue::Ref(_) => WasmValue::Reference,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{Backend, BackendOptions, BackendRequest, BackendSource};
    use crate::jals_backend::JalsBackend;
    use jals_storage::{CacheKey, CacheNamespace, ContentDigest, RelativePath};

    /// Compile one Java source with the wasm backend and hand back the module.
    ///
    /// The whole fixture is in-crate: the backend that produced the bytes lives here, so a test
    /// needs no external tool and no committed binary — which is also what lets it run in the CI
    /// cell that has neither a JVM nor a wasm engine on the host.
    fn module(text: &str) -> Vec<u8> {
        let bytes = text.as_bytes().to_vec();
        let tree = [BackendSource {
            path: RelativePath::parse("Main.java").expect("a valid path"),
            key: CacheKey::new(
                CacheNamespace::FrontendOutput,
                ContentDigest::of(b"test"),
                ContentDigest::of(&bytes),
            ),
            bytes,
        }];
        let options = BackendOptions::default();
        let request = BackendRequest {
            tree: &tree,
            classpath: &[],
            options: &options,
            progress: &Progress::SILENT,
        };
        let backend = JalsBackend::wasm();
        let outcome =
            jals_exec::block_on_inline(backend.compile(&request)).expect("the backend ran");
        assert!(
            outcome.success(),
            "the wasm backend refused the fixture: {:?}",
            outcome.messages
        );
        let (path, bytes) = outcome.artifacts.into_iter().next().expect("one module");
        assert_eq!(path.to_string(), JalsBackend::WASM_MODULE);
        bytes
    }

    fn run(
        module: &[u8],
        invoke: Option<&str>,
        args: &[String],
    ) -> Result<WasmRunOutcome, WasmRunError> {
        WasmRunner::run(&WasmRunRequest {
            module,
            invoke,
            args,
            progress: &Progress::SILENT,
        })
    }

    #[test]
    fn a_static_method_answers_through_its_export() {
        let module = module(
            "public class Main {\n\
             \x20   public static int add(int a, int b) { return a + b; }\n\
             }\n",
        );
        let args = ["3".to_owned(), "4".to_owned()];
        assert_eq!(
            run(&module, Some("add"), &args),
            Ok(WasmRunOutcome::Returned(vec![WasmValue::I32(7)]))
        );
    }

    /// The object the lowering allocates is the host collector's, so a method that news one up and
    /// reads it back is the round trip that proves the GC types survived the engine.
    #[test]
    fn an_allocation_round_trips_through_the_hosts_collector() {
        let module = module(
            "public class Main {\n\
             \x20   int x;\n\
             \x20   Main(int x) { this.x = x; }\n\
             \x20   int get() { return x; }\n\
             \x20   public static int roundTrip(int n) { Main m = new Main(n); return m.get(); }\n\
             }\n",
        );
        let args = ["7".to_owned()];
        assert_eq!(
            run(&module, Some("roundTrip"), &args),
            Ok(WasmRunOutcome::Returned(vec![WasmValue::I32(7)]))
        );
    }

    /// Naming no export is still a run: instantiating executes the start function.
    #[test]
    fn naming_no_export_instantiates_the_module() {
        let module = module("public class Main { public static int one() { return 1; } }\n");
        assert_eq!(run(&module, None, &[]), Ok(WasmRunOutcome::Instantiated));
    }

    /// A missing name reports what the module does export, which is the only evidence a caller
    /// gets when two `static` methods of one name collided into a single export.
    #[test]
    fn a_missing_export_lists_the_ones_that_are_there() {
        let module = module(
            "public class Main {\n\
             \x20   public static int one() { return 1; }\n\
             \x20   public static int two() { return 2; }\n\
             }\n",
        );
        let Err(WasmRunError::NoSuchExport { name, available }) = run(&module, Some("three"), &[])
        else {
            panic!("expected the export to be missing");
        };
        assert_eq!(name, "three");
        assert!(
            available.iter().any(|export| export == "one")
                && available.iter().any(|export| export == "two"),
            "expected both exports to be listed, got {available:?}"
        );
    }

    #[test]
    fn the_argument_count_is_the_exports_and_not_the_callers() {
        let module =
            module("public class Main { public static int add(int a, int b) { return a + b; } }\n");
        let args = ["1".to_owned()];
        assert_eq!(
            run(&module, Some("add"), &args),
            Err(WasmRunError::ArgumentCount {
                name: "add".to_owned(),
                expected: 2,
                given: 1,
            })
        );
    }

    #[test]
    fn an_argument_is_read_against_the_declared_type() {
        let module =
            module("public class Main { public static int twice(int n) { return n + n; } }\n");
        let args = ["four".to_owned()];
        assert_eq!(
            run(&module, Some("twice"), &args),
            Err(WasmRunError::Argument {
                name: "twice".to_owned(),
                position: 0,
                expected: "an i32",
                given: "four".to_owned(),
            })
        );
    }

    /// Every `static` method is exported, visibility and parameter types included, so an export
    /// taking an object is reachable by name and callable by nothing. It is refused with the
    /// position that caused it rather than mis-parsed.
    #[test]
    fn an_export_taking_a_reference_is_refused_by_position() {
        let module = module(
            "public class Main {\n\
             \x20   int x;\n\
             \x20   Main(int x) { this.x = x; }\n\
             \x20   public static int read(Main m) { return m.x; }\n\
             }\n",
        );
        let args = ["0".to_owned()];
        assert_eq!(
            run(&module, Some("read"), &args),
            Err(WasmRunError::UnsupportedParameter {
                name: "read".to_owned(),
                position: 0,
                ty: "a reference",
            })
        );
    }

    /// A trap is the call failing inside the engine rather than the request being wrong, so it is
    /// its own answer. Integer division by zero is the one every wasm engine agrees on.
    #[test]
    fn a_trapping_call_reports_the_trap() {
        let module =
            module("public class Main { public static int div(int a, int b) { return a / b; } }\n");
        let args = ["1".to_owned(), "0".to_owned()];
        let Err(WasmRunError::Trap(_)) = run(&module, Some("div"), &args) else {
            panic!("dividing by zero traps");
        };
    }

    #[test]
    fn bytes_that_are_not_a_module_are_a_parse_failure() {
        let Err(WasmRunError::Parse(_)) = run(b"not a module", None, &[]) else {
            panic!("expected a parse failure");
        };
    }
}
