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
//! which is where this backend lowers a class's `static` initialisers. A project with no static
//! state has no start function, so that run executes nothing — which is why
//! [`WasmRunOutcome::Instantiated`] must not be rendered as a claim that it did.
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

use jals_native::{Args, NativeBindings, NativeError, NativeHost, NativeValue, RefSlot, Results};
use jals_progress::{Activity, Outcome, Progress};
use tinywasm::types::{ImportType, WasmType};
use tinywasm::{ExternItem, FuncContext, HostFunction, Imports, ModuleInstance, RefValue, Store};

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
    /// The implementations of every `native` method the module imports.
    ///
    /// Empty for a project that selected no package, which is every module with no import section
    /// — so passing [`NativeBindings::new`] is not a degraded mode, it is what "this module needs
    /// nothing from the host" looks like.
    pub natives: &'a NativeBindings,
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
            // `{:?}` and not `{}` for the two float arms. `Display` never uses exponent notation
            // and drops a whole value's fractional part, so `Float.MAX_VALUE` came out as 39
            // digits, `1e300` as 301 of them, and `42.0f` as `42` — indistinguishable from an
            // `i32` on the stdout this crate's callers hand to a script. `Debug` is the
            // round-trippable rendering: `3.4028235e38`, `1e300`, `42.0`, `-0.0`.
            Self::F32(value) => write!(f, "{value:?}"),
            Self::F64(value) => write!(f, "{value:?}"),
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
    ///
    /// "Which runs its start function" is conditional on there being one: the backend emits a
    /// start section only for a project with static state, so this is not a claim that any of the
    /// project's code executed. Do not render it as one.
    Instantiated,
    /// An export was called and returned these values. Empty for a `void` method.
    Returned(Vec<WasmValue>),
}

/// Why a run did not happen, or did not finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WasmRunError {
    /// The bytes are not a module this engine accepts.
    Parse(String),
    /// The module parsed but could not be instantiated: it is malformed, or a link failed, or a
    /// segment trapped. A start function that trapped or threw is *not* here — that is the
    /// project's own code running, and it comes back as [`Trap`](Self::Trap) or
    /// [`Exception`](Self::Exception) like any other execution failure.
    Instantiate(String),
    /// Nothing is exported under that name.
    ///
    /// Carries the names that *are* exported, because the one thing a caller cannot see from here
    /// is what happened to the name it asked for: an export name is bare, with no owner in it, so
    /// two `static` methods sharing a name — an overload pair, or one method per class — collide
    /// and the second is dropped when the module is built. The list is the only evidence of that.
    /// The module imports a `native` method nothing supplies an implementation for.
    ///
    /// Reported when the module is linked, before any of its code runs, and carrying every key the
    /// selection *does* bind — which is the only evidence a reader gets that a signature was
    /// spelled two ways, since the import's own name carries the descriptor.
    UnresolvedImport {
        /// The declaring class's internal name, as the module imports it.
        module: String,
        /// The method's name with its descriptor, as the module imports it.
        name: String,
        /// Every `<owner>.<signature>` the selected packages bind.
        available: Vec<String>,
    },
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
    /// The code threw and nothing caught it.
    ///
    /// Its own answer rather than a [`Trap`](Self::Trap), because the two are different failures
    /// and this is the one a Java program reaches on purpose: the backend lowers every `throw`
    /// onto the module's tag, and an uncaught one leaves the engine as `Error::Exception`. What
    /// the object *is* cannot be read from here — it belongs to the embedder's collector and the
    /// store is gone by the time a caller sees this — so the message says that rather than
    /// pretending a trap occurred.
    Exception,
    /// The export was found, and the engine then refused the handle it had just produced.
    ///
    /// Its own answer rather than folded into [`Instantiate`](Self::Instantiate), which means
    /// *linking* failed, or into [`Trap`](Self::Trap), which means the project's own code did.
    /// This is neither: reading an export's signature can fail only when the handle and the store
    /// disagree, and one store is built per run and never leaves this crate — so a caller seeing
    /// it has nothing to fix in their module or their source, and a message sending them to
    /// either would be the wrong place. Kept rather than unwrapped because a panic in a library
    /// is worse than an answer nobody expects to read.
    Signature { name: String, message: String },
}

impl WasmRunError {
    /// Whether the project's own code ran and did not return normally.
    ///
    /// The two variants [`execution_failure`](WasmRunner::execution_failure) produces, and nothing
    /// else: every other variant here is the request or the module being wrong, which is this
    /// crate failing to *reach* the code rather than the code failing. A test runner inverts
    /// `#[should_fail]` on this and on nothing else — an export that is not there, reported as a
    /// pass because the test was expected to fail, is a missing test claiming to have run.
    ///
    /// Crate-internal: `WasmTestLauncher` is the only thing that has to tell the two apart, and a
    /// host reads a verdict rather than re-deriving one.
    ///
    /// Gated on `native` for exactly that reason. `wasm_test` is the only caller and is itself
    /// `#[cfg(all(feature = "native", feature = "wasm-run"))]`, so in the browser's configuration
    /// — `wasm-run` with no `native` — this method has none, and an item reachable solely from a
    /// `native`-gated module has to carry that gate itself or it is dead code there.
    #[cfg(feature = "native")]
    pub(crate) const fn is_execution_failure(&self) -> bool {
        matches!(self, Self::Trap(_) | Self::Exception)
    }
}

impl fmt::Display for WasmRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(message) => write!(f, "the module could not be parsed: {message}"),
            Self::Instantiate(message) => {
                write!(f, "the module could not be instantiated: {message}")
            }
            Self::UnresolvedImport {
                module,
                name,
                available,
            } => {
                write!(
                    f,
                    "the module imports `{name}` from `{module}`, and no selected native package \
                     supplies it"
                )?;
                if available.is_empty() {
                    return f.write_str(" (no package is selected)");
                }
                write!(f, "; the selection binds {}", available.join(", "))
            }
            Self::NoSuchExport { name, available } => {
                write!(f, "the module exports no function named `{name}`")?;
                if available.is_empty() {
                    return f.write_str(" (it exports no functions at all)");
                }
                write!(f, "; it exports {}", available.join(", "))
            }
            // `position + 1` in both messages below: the field is a zero-based index into the
            // signature, but every other number here is a human count — `ArgumentCount` says
            // "takes 2 argument(s)" — and one message counting from zero beside one counting from
            // one sends a reader to edit the wrong argument.
            Self::UnsupportedParameter { name, position, ty } => write!(
                f,
                "`{name}` takes {ty} at argument {}, which cannot be written as an argument",
                position + 1
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
                "argument {} of `{name}` is {expected}, and `{given}` is not one",
                position + 1
            ),
            Self::Trap(message) => write!(f, "the call trapped: {message}"),
            Self::Exception => f.write_str("the code threw an exception and nothing caught it"),
            Self::Signature { name, message } => write!(
                f,
                "the engine exports `{name}` but would not describe it: {message}"
            ),
        }
    }
}

/// Bytes this engine has already accepted as a module.
///
/// Decoding and validating is by far the most expensive step here — linear in the module's size,
/// and tens of milliseconds for a project-sized one — while instantiating is microseconds. A
/// caller that runs *one* module many times therefore pays it once rather than once per call:
/// that is `WasmTestLauncher`, whose whole suite is one module and one call per test. (Named in
/// plain text rather than linked, because that type exists only in the `native` configuration and
/// an intra-doc link from here would not resolve in the browser's.)
///
/// Opaque and crate-internal, so the engine's own types stay inside this crate exactly as
/// [`WasmValue`] keeps them out of the two hosts. A `tinywasm::Module` is an `Arc` newtype, so
/// cloning one is a refcount bump and it crosses a fan-out worker as itself.
#[derive(Clone)]
pub(crate) struct ParsedModule(tinywasm::Module);

// The engine derives `Debug` only under its own `debug` feature, which this build does not enable,
// and a launcher holding one still has to be `Debug`. There is nothing a reader could act on in a
// decoded module anyway.
impl fmt::Debug for ParsedModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ParsedModule")
    }
}

/// The engine, as a native binding sees it.
///
/// Built per call and thrown away with it, which is what makes a [`RefSlot`] meaningful: the slots
/// index *this* call's live references, so one from another call names nothing and cannot be made
/// to.
struct EngineHost<'a> {
    ctx: FuncContext<'a>,
    /// Every non-null reference this call has made live, in the order slots were handed out.
    refs: Vec<RefValue>,
}

impl EngineHost<'_> {
    /// A package's refusal, as the engine's error. It becomes a trap, which is what a JVM does with
    /// a `native` method that cannot be linked or does not return.
    fn trap(error: &NativeError) -> tinywasm::Error {
        tinywasm::Error::Other(alloc::format!("{error}"))
    }

    /// One engine value as the vocabulary a package reads, registering a reference if it is one.
    fn decode(&mut self, value: &tinywasm::WasmValue) -> Result<NativeValue, NativeError> {
        Ok(match value {
            tinywasm::WasmValue::I32(value) => NativeValue::I32(*value),
            tinywasm::WasmValue::I64(value) => NativeValue::I64(*value),
            tinywasm::WasmValue::F32(value) => NativeValue::F32(*value),
            tinywasm::WasmValue::F64(value) => NativeValue::F64(*value),
            tinywasm::WasmValue::Ref(RefValue::Null) => NativeValue::Null,
            tinywasm::WasmValue::Ref(reference) => {
                self.refs.push(reference.clone());
                let slot = u32::try_from(self.refs.len() - 1).unwrap_or(u32::MAX);
                NativeValue::Ref(RefSlot::new(slot))
            }
            // A `v128` is not a type this backend emits — Java has no vector primitive — so a
            // package can never be handed one, and inventing a reading for it would be a lie about
            // what the module declared.
            tinywasm::WasmValue::V128(_) => {
                return Err(NativeError::Message(alloc::string::String::from(
                    "a v128 argument has no reading in a native package",
                )));
            }
        })
    }

    /// The reverse, for a value a binding wrote.
    fn encode(&self, value: NativeValue) -> Result<tinywasm::WasmValue, NativeError> {
        Ok(match value {
            NativeValue::I32(value) => tinywasm::WasmValue::I32(value),
            NativeValue::I64(value) => tinywasm::WasmValue::I64(value),
            NativeValue::F32(value) => tinywasm::WasmValue::F32(value),
            NativeValue::F64(value) => tinywasm::WasmValue::F64(value),
            NativeValue::Null => tinywasm::WasmValue::Ref(RefValue::Null),
            NativeValue::Ref(slot) => tinywasm::WasmValue::Ref(self.reference(slot)?.clone()),
        })
    }

    /// The reference a slot names, or a refusal naming what went wrong.
    fn reference(&self, slot: RefSlot) -> Result<&RefValue, NativeError> {
        self.refs
            .get(slot.index() as usize)
            .ok_or(NativeError::NotAnArray)
    }

    /// The array a slot names.
    fn array(&self, slot: RefSlot) -> Result<tinywasm::ArrayRef, NativeError> {
        match self.reference(slot)? {
            RefValue::Any(value) => value.as_array().ok_or(NativeError::NotAnArray),
            _ => Err(NativeError::NotAnArray),
        }
    }
}

impl NativeHost for EngineHost<'_> {
    fn array_len(&mut self, slot: RefSlot) -> Result<u32, NativeError> {
        let array = self.array(slot)?;
        let len = array
            .len(self.ctx.store())
            .map_err(|error| NativeError::Call(error.to_string()))?;
        Ok(u32::try_from(len).unwrap_or(u32::MAX))
    }

    fn array_get(&mut self, slot: RefSlot, index: u32) -> Result<NativeValue, NativeError> {
        let array = self.array(slot)?;
        let value = array
            .get(self.ctx.store_mut(), index as usize)
            .map_err(|error| NativeError::Call(error.to_string()))?;
        self.decode(&value)
    }

    fn array_set(
        &mut self,
        slot: RefSlot,
        index: u32,
        value: NativeValue,
    ) -> Result<(), NativeError> {
        let array = self.array(slot)?;
        let encoded = self.encode(value)?;
        array
            .set(self.ctx.store_mut(), index as usize, encoded)
            .map_err(|error| NativeError::Call(error.to_string()))
    }

    fn call_export(
        &mut self,
        name: &str,
        args: &[NativeValue],
        results: &mut [NativeValue],
    ) -> Result<(), NativeError> {
        let instance = self.ctx.module().clone();
        let func = instance
            .func_untyped(self.ctx.store(), name)
            .map_err(|error| NativeError::Call(error.to_string()))?;
        let encoded: Vec<tinywasm::WasmValue> = args
            .iter()
            .map(|value| self.encode(*value))
            .collect::<Result<_, _>>()?;
        let mut returned = alloc::vec![tinywasm::WasmValue::I32(0); results.len()];
        self.ctx
            .call_untyped(&func, &encoded, &mut returned)
            .map_err(|error| NativeError::Call(error.to_string()))?;
        for (slot, value) in results.iter_mut().zip(&returned) {
            *slot = self.decode(value)?;
        }
        Ok(())
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
        Self::reporting(request.progress, request.invoke, || {
            let module = Self::parse(request.module)?;
            Self::invoke(&module, request.invoke, request.args, request.natives)
        })
    }

    /// [`run`](Self::run) for a module this engine has already accepted.
    ///
    /// The parse is the caller's, once, rather than this function's, every time — see
    /// [`ParsedModule`].
    #[cfg(feature = "native")]
    pub(crate) fn run_parsed(
        module: &ParsedModule,
        invoke: Option<&str>,
        args: &[String],
        natives: &NativeBindings,
        progress: &Progress,
    ) -> Result<WasmRunOutcome, WasmRunError> {
        Self::reporting(progress, invoke, || {
            Self::invoke(module, invoke, args, natives)
        })
    }

    /// Decode and validate the bytes, without running anything.
    pub(crate) fn parse(bytes: &[u8]) -> Result<ParsedModule, WasmRunError> {
        tinywasm::parse_bytes(bytes)
            .map(ParsedModule)
            .map_err(|error| WasmRunError::Parse(error.to_string()))
    }

    /// One run as one progress unit, so both entry points end it in the same place.
    fn reporting(
        progress: &Progress,
        invoke: Option<&str>,
        body: impl FnOnce() -> Result<WasmRunOutcome, WasmRunError>,
    ) -> Result<WasmRunOutcome, WasmRunError> {
        let task = progress.begin(Activity::Run, invoke.unwrap_or("module"));
        match body() {
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
        // Every arm names the instantiate step, because every run performs it: an export is
        // reached only after the module's start function has already executed, and that step is
        // the one that can fail before the named export is ever looked up. Describing a run as
        // only "invoke `f`" understated it.
        match invoke {
            Some(name) if args.is_empty() => {
                format!("tinywasm: instantiate the module, then invoke `{name}`")
            }
            Some(name) => format!(
                "tinywasm: instantiate the module, then invoke `{name}` with {}",
                args.join(" ")
            ),
            None => "tinywasm: instantiate the module, running any static initialisers it has"
                .to_owned(),
        }
    }

    /// The run itself, minus the decode: instantiate, then call the named export when there is one.
    fn invoke(
        module: &ParsedModule,
        invoke: Option<&str>,
        args: &[String],
        natives: &NativeBindings,
    ) -> Result<WasmRunOutcome, WasmRunError> {
        let module = &module.0;
        let imports = Self::link(module, natives)?;
        let mut store = Store::default();
        // Instantiating in two halves rather than through `instantiate`, which is exactly these
        // two calls. Only the first is *linking* — a malformed module, an unknown import, a
        // segment that traps. The second runs the start function, which is where a class's
        // `static` initialisers are lowered, so it is already the project's own code executing:
        // folding its failure into `Instantiate` reported a divide-by-zero in a `static {}` block
        // as "the module could not be instantiated", which sends the reader to the encoding.
        let instance = ModuleInstance::instantiate_no_start(&mut store, module, Some(&imports))
            .map_err(|error| WasmRunError::Instantiate(error.to_string()))?;
        instance
            .start(&mut store)
            .map_err(Self::execution_failure)?;

        let Some(name) = invoke else {
            return Ok(WasmRunOutcome::Instantiated);
        };
        let func = instance
            .func_untyped(&store, name)
            .map_err(|_| WasmRunError::NoSuchExport {
                name: name.to_owned(),
                available: Self::exported_functions(&instance),
            })?;
        // Not `Instantiate`: that variant says linking failed, and linking succeeded two lines
        // ago. `Function::ty` fails only when the handle and the store disagree, which is a fact
        // about this crate's own bookkeeping rather than about the module or the project.
        let signature = func.ty(&store).map_err(|error| WasmRunError::Signature {
            name: name.to_owned(),
            message: error.to_string(),
        })?;
        let params = signature.params().to_vec();
        let results = signature.results().len();
        // Before the count, not after it. An export taking a reference cannot be called with any
        // number of arguments, so reporting the arity first tells the caller to invent one for a
        // parameter the next message exists to refuse — and the arity is the only thing they can
        // act on, so they do.
        for (position, ty) in params.iter().enumerate() {
            if matches!(ty, WasmType::V128 | WasmType::Ref(_)) {
                return Err(WasmRunError::UnsupportedParameter {
                    name: name.to_owned(),
                    position,
                    ty: Self::type_name(*ty),
                });
            }
        }
        if params.len() != args.len() {
            return Err(WasmRunError::ArgumentCount {
                name: name.to_owned(),
                expected: params.len(),
                given: args.len(),
            });
        }

        let mut arguments = Vec::with_capacity(params.len());
        for (position, (text, ty)) in args.iter().zip(&params).enumerate() {
            arguments.push(Self::argument(text, *ty, name, position)?);
        }
        // The engine writes into a buffer the caller sizes, and `tinywasm::WasmValue` has no
        // `Default` — the placeholder is overwritten by every result the call produces.
        let mut returned = vec![tinywasm::WasmValue::I32(0); results];
        func.call(&mut store, &arguments, &mut returned)
            .map_err(Self::execution_failure)?;
        Ok(WasmRunOutcome::Returned(
            returned.iter().map(Self::value).collect(),
        ))
    }

    /// Build the import set the module declares, out of the implementations the selection supplies.
    ///
    /// The type each host function is defined under is **the one the module itself declared for
    /// that import**, read back out of its import section. Nothing here re-derives a wasm type
    /// from a Java descriptor, and that is the whole reason the two halves of a native package
    /// cannot disagree about one: the engine links a host function only when its type is *equal*
    /// to the import's, and passing the import's own type makes that equality structural rather
    /// than something a mapping table has to keep true.
    ///
    /// What is left to check is therefore only whether a name is bound at all — and because the
    /// import's field name carries the method's descriptor, a Rust half that spelled the signature
    /// differently shows up exactly here, as an unresolved import listing what *is* registered.
    fn link(module: &tinywasm::Module, natives: &NativeBindings) -> Result<Imports, WasmRunError> {
        let mut imports = Imports::new();
        for import in module.imports() {
            let ImportType::Func(signature) = import.ty else {
                // The backend emits function imports and nothing else. A module carrying another
                // kind did not come from it, and guessing at one is worse than saying so.
                return Err(WasmRunError::UnresolvedImport {
                    module: import.module.to_owned(),
                    name: import.name.to_owned(),
                    available: natives.keys().map(|(o, s)| format!("{o}.{s}")).collect(),
                });
            };
            let Some(binding) = natives.get(import.module, import.name) else {
                return Err(WasmRunError::UnresolvedImport {
                    module: import.module.to_owned(),
                    name: import.name.to_owned(),
                    available: natives.keys().map(|(o, s)| format!("{o}.{s}")).collect(),
                });
            };
            let binding = binding.clone();
            imports.define(
                import.module,
                import.name,
                HostFunction::from_untyped(signature, move |ctx, args, results| {
                    Self::dispatch(&binding, ctx, args, results)
                }),
            );
        }
        Ok(imports)
    }

    /// One call into a native binding: decode the arguments, run it, encode what it wrote back.
    fn dispatch(
        binding: &jals_native::NativeFn,
        ctx: FuncContext<'_>,
        args: &[tinywasm::WasmValue],
        results: &mut [tinywasm::WasmValue],
    ) -> tinywasm::Result<()> {
        let mut host = EngineHost {
            ctx,
            refs: Vec::new(),
        };
        let decoded: Vec<NativeValue> = args
            .iter()
            .map(|value| host.decode(value))
            .collect::<Result<_, _>>()
            .map_err(|error| EngineHost::trap(&error))?;
        let mut written = alloc::vec![NativeValue::Null; results.len()];
        binding(&mut host, Args::new(&decoded), Results::new(&mut written))
            .map_err(|error| EngineHost::trap(&error))?;
        for (slot, value) in results.iter_mut().zip(&written) {
            *slot = host
                .encode(*value)
                .map_err(|error| EngineHost::trap(&error))?;
        }
        Ok(())
    }

    /// One failure of the project's own code, as this crate's vocabulary.
    ///
    /// A `throw` and a trap leave the engine as different variants and are different things to
    /// tell a reader about, so they stay apart here. Everything else the engine can return at a
    /// call is a trap as far as a caller is concerned.
    fn execution_failure(error: tinywasm::Error) -> WasmRunError {
        match error {
            tinywasm::Error::Exception(_) => WasmRunError::Exception,
            error => WasmRunError::Trap(error.to_string()),
        }
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

    /// A package publishing `Host.answer()`, over `sink`.
    ///
    /// The whole shape of a native package in eight lines, which is what makes it a fixture: the
    /// Java declares one `native` method, the Rust binds the two strings the compiler derives from
    /// that declaration, and nothing between the halves restates a type.
    fn answering_package(answer: i32) -> jals_native::JavaPackage {
        let mut package = jals_native::JavaPackage::new("test.host", 1);
        package.source(
            "test/host/Host.java",
            "package test.host;\npublic final class Host { public static native int answer(); }\n",
            jals_native::SourceKind::Implementation,
        );
        package.bind(
            "test/host/Host",
            "answer()I",
            move |_host, _args, mut results: jals_native::Results<'_>| {
                results.set(0, jals_native::NativeValue::I32(answer));
                Ok(())
            },
        );
        package
    }

    /// One selection holding [`answering_package`].
    fn answering_selection(answer: i32) -> jals_native::PackageSelection {
        let mut resolver = jals_native::StaticResolver::new("test");
        resolver.add(answering_package(answer));
        jals_native::ResolverChain::new()
            .push(Box::new(resolver))
            .select(&["test.host".to_owned()])
            .expect("just registered")
    }

    /// Compile one Java source with the wasm backend and hand back the module.
    ///
    /// The whole fixture is in-crate: the backend that produced the bytes lives here, so a test
    /// needs no external tool and no committed binary — which is also what lets it run in the CI
    /// cell that has neither a JVM nor a wasm engine on the host.
    fn module(text: &str) -> Vec<u8> {
        module_with(text, jals_native::PackageSelection::empty())
    }

    /// [`module`], with a native package selected.
    fn module_with(text: &str, natives: jals_native::PackageSelection) -> Vec<u8> {
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
        let backend = JalsBackend::wasm(crate::Assertions::Disabled, natives);
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
        run_with(module, invoke, args, &NativeBindings::new())
    }

    fn run_with(
        module: &[u8],
        invoke: Option<&str>,
        args: &[String],
        natives: &NativeBindings,
    ) -> Result<WasmRunOutcome, WasmRunError> {
        WasmRunner::run(&WasmRunRequest {
            module,
            invoke,
            args,
            natives,
            progress: &Progress::SILENT,
        })
    }

    /// The whole seam, end to end and in this process: a `native` method becomes an import, the
    /// runner links it against the package that declared it, and the project's call reaches Rust.
    #[test]
    fn a_native_method_is_linked_against_the_package_that_declares_it() {
        let selection = answering_selection(42);
        let module = module_with(
            "import test.host.Host;\n\
             public class Main {\n\
             \x20   public static int run() { return Host.answer() + 1; }\n\
             }\n",
            selection.clone(),
        );
        let outcome = run_with(&module, Some("run"), &[], &selection.bindings())
            .expect("the module links and runs");
        assert!(
            matches!(outcome, WasmRunOutcome::Returned(ref values) if values == &[WasmValue::I32(43)])
        );
    }

    /// A package's own `static` methods are compiled into the module and are not its surface.
    #[test]
    fn a_packages_methods_are_not_module_exports() {
        let selection = answering_selection(1);
        let module = module_with(
            "public class Main { public static int run() { return 0; } }\n",
            selection.clone(),
        );
        let Err(WasmRunError::NoSuchExport { available, .. }) =
            run_with(&module, Some("absent"), &[], &selection.bindings())
        else {
            panic!("the export is missing");
        };
        assert_eq!(available, vec!["run".to_owned()]);
    }

    /// Nothing bound is a *link* failure, reported before any of the module's code runs and
    /// carrying what the selection does bind.
    ///
    /// This is also the whole signature check. The import's field name carries the method's
    /// descriptor, so a Rust half that spelled the signature differently arrives here rather than
    /// as a type mismatch somebody has to notice — which is why the test binds a real package
    /// under a wrong descriptor rather than binding nothing.
    #[test]
    fn an_import_nothing_supplies_is_refused_with_what_is_bound() {
        let module = module_with(
            "import test.host.Host;\n\
             public class Main { public static int run() { return Host.answer(); } }\n",
            answering_selection(1),
        );

        let mut resolver = jals_native::StaticResolver::new("test");
        let mut package = jals_native::JavaPackage::new("test.host", 1);
        package.source(
            "test/host/Host.java",
            "package test.host;\n",
            jals_native::SourceKind::Implementation,
        );
        // `()J` where the declaration says `()I`: one character, and the whole difference between
        // a linked module and this.
        package.bind("test/host/Host", "answer()J", |_, _, _| Ok(()));
        resolver.add(package);
        let wrong = jals_native::ResolverChain::new()
            .push(Box::new(resolver))
            .select(&["test.host".to_owned()])
            .expect("just registered")
            .bindings();

        let Err(error @ WasmRunError::UnresolvedImport { .. }) =
            run_with(&module, Some("run"), &[], &wrong)
        else {
            panic!("the import is unresolved");
        };
        let WasmRunError::UnresolvedImport {
            module: owner,
            name,
            available,
        } = &error
        else {
            unreachable!()
        };
        assert_eq!(owner, "test/host/Host");
        assert_eq!(name, "answer()I");
        assert_eq!(available, &vec!["test/host/Host.answer()J".to_owned()]);
        assert!(error.to_string().contains("answer()J"));

        // And with nothing selected at all, the message says that rather than listing nothing.
        let Err(error) = run_with(&module, Some("run"), &[], &NativeBindings::new()) else {
            panic!("the import is unresolved");
        };
        assert!(error.to_string().contains("no package is selected"));
    }

    /// A binding may read the arrays the module hands it, which is what a package's Java is
    /// written on top of.
    #[test]
    fn a_binding_reads_an_array_the_module_allocated() {
        let mut package = jals_native::JavaPackage::new("test.sum", 1);
        package.source(
            "test/sum/Sum.java",
            "package test.sum;\npublic final class Sum { public static native int of(int[] values); }\n",
            jals_native::SourceKind::Implementation,
        );
        package.bind(
            "test/sum/Sum",
            "of([I)I",
            |host: &mut dyn jals_native::NativeHost,
             args: jals_native::Args<'_>,
             mut results: jals_native::Results<'_>| {
                let slot = args.reference(0)?;
                let total: i32 = host.array_i32(slot)?.iter().sum();
                results.set(0, jals_native::NativeValue::I32(total));
                Ok(())
            },
        );
        let mut registry = jals_native::StaticResolver::new("test");
        registry.add(package);
        let selection = jals_native::ResolverChain::new()
            .push(Box::new(registry))
            .select(&["test.sum".to_owned()])
            .expect("just registered");

        let module = module_with(
            "import test.sum.Sum;\n\
             public class Main {\n\
             \x20   public static int run() { return Sum.of(new int[]{1, 2, 3, 4}); }\n\
             }\n",
            selection.clone(),
        );
        let outcome = run_with(&module, Some("run"), &[], &selection.bindings())
            .expect("the module links and runs");
        assert!(
            matches!(outcome, WasmRunOutcome::Returned(ref values) if values == &[WasmValue::I32(10)])
        );
    }

    /// A binding that refuses is a trap, which is what a JVM does with a `native` method that
    /// cannot answer — never a silent zero.
    #[test]
    fn a_binding_that_refuses_traps_rather_than_answering() {
        let mut package = jals_native::JavaPackage::new("test.no", 1);
        package.source(
            "test/no/No.java",
            "package test.no;\npublic final class No { public static native int answer(); }\n",
            jals_native::SourceKind::Implementation,
        );
        package.bind("test/no/No", "answer()I", |_, _, _| {
            Err(jals_native::NativeError::Message("said no".to_owned()))
        });
        let mut registry = jals_native::StaticResolver::new("test");
        registry.add(package);
        let selection = jals_native::ResolverChain::new()
            .push(Box::new(registry))
            .select(&["test.no".to_owned()])
            .expect("just registered");

        let module = module_with(
            "import test.no.No;\n\
             public class Main { public static int run() { return No.answer(); } }\n",
            selection.clone(),
        );
        let Err(error) = run_with(&module, Some("run"), &[], &selection.bindings()) else {
            panic!("the binding refused");
        };
        assert!(error.to_string().contains("said no"), "{error}");
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
        // And with no argument at all, which is what a caller actually types first. The arity is
        // the wrong thing to report here: no number of arguments makes this export callable, so
        // an `ArgumentCount` would send them to supply one for a parameter this refuses.
        assert_eq!(
            run(&module, Some("read"), &[]),
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

    /// A `static` initialiser that traps is the *project's* code failing, not a module that could
    /// not be instantiated — the two send a reader to different places. This is the only test with
    /// a start section at all: the backend emits one solely for a class with static state.
    #[test]
    fn a_trapping_static_initialiser_is_an_execution_failure() {
        let module = module(
            "public class Main {\n\
             \x20   static int x;\n\
             \x20   static { x = divide(1, 0); }\n\
             \x20   static int divide(int a, int b) { return a / b; }\n\
             \x20   public static int get() { return x; }\n\
             }\n",
        );
        let Err(WasmRunError::Trap(_)) = run(&module, None, &[]) else {
            panic!("a trapping start function is a trap, not a failure to instantiate");
        };
    }

    /// A `throw` nothing catches is its own answer. It is the failure a Java program reaches on
    /// purpose, and calling it a trap said the engine broke rather than the code threw.
    #[test]
    fn an_uncaught_throw_is_not_reported_as_a_trap() {
        let module = module(
            "public class Main {\n\
             \x20   public static int parse(int n) {\n\
             \x20       if (n < 0) { throw new Boom(); }\n\
             \x20       return n;\n\
             \x20   }\n\
             }\n\
             class Boom extends RuntimeException { }\n",
        );
        let args = ["-1".to_owned()];
        assert_eq!(
            run(&module, Some("parse"), &args),
            Err(WasmRunError::Exception)
        );
    }

    /// `describe` has no other caller in the workspace — `jals run --dry-run` is its one production
    /// use — so this is what holds its three arms honest. Every arm names the instantiate step,
    /// because every run performs it before it looks an export up.
    #[test]
    fn every_described_run_names_the_instantiate_step() {
        let args = ["3".to_owned(), "4".to_owned()];
        for described in [
            WasmRunner::describe(None, &[]),
            WasmRunner::describe(Some("add"), &[]),
            WasmRunner::describe(Some("add"), &args),
        ] {
            assert!(
                described.contains("instantiate"),
                "an export is reached only after the module is instantiated: {described}"
            );
        }
        assert!(WasmRunner::describe(Some("add"), &args).contains("3 4"));
    }

    #[test]
    fn bytes_that_are_not_a_module_are_a_parse_failure() {
        let Err(WasmRunError::Parse(_)) = run(b"not a module", None, &[]) else {
            panic!("expected a parse failure");
        };
    }
}
