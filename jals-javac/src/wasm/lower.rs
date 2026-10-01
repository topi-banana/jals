//! Java to one WebAssembly module.
//!
//! # Memory management is the host's
//!
//! A Java class becomes a wasm `struct` type and `new` becomes `struct.new`; inheritance becomes
//! *declared* subtyping, so a `(ref $Sub)` is usable wherever a `(ref $Super)` is expected without
//! a conversion or a header word. Nothing in this backend allocates linear memory, keeps a free
//! list, traces, or frees — the embedder's collector owns every object from the moment it is
//! created. That is the whole reason to target the GC proposal rather than linear memory: a
//! hand-written collector would have to scan a stack it cannot see.
//!
//! # One module for the whole project
//!
//! Unlike the JVM backend, which emits one class file per declared type, this one takes every
//! source at once and emits a single module. wasm has no dynamic loading and no classpath: a call
//! from one type to another is a `call` to a function index, which only exists if both were
//! compiled together.
//!
//! # Scope
//!
//! Primitives, user-declared classes (fields, constructors, methods), and the control flow that
//! goes with them: arithmetic with Java's numeric promotions, the bitwise and shift operators,
//! comparisons at every width, reference identity and `instanceof`, casts, and the unary operators.
//! Exceptions (`throw` / `try` / `catch`, over one declared tag) and interface dispatch (a
//! `ref.test` chain over the classes this module compiles) are in too.
//!
//! Library types are **a linked package's** to supply: a class the project does not declare is
//! replayed into the layout and its members become imports, and with no package linked the name is
//! refused rather than given a representation nothing implements. `jals-platform` is the package
//! that provides `java.base`. Nothing is supplied at run time by the host — a package is the same
//! compiler's output, linked at instantiation rather than merged into the module (see
//! [`LinkedLibrary`]).
//!
//! # Where wasm and the JVM genuinely differ
//!
//! Most of the two backends' disagreements are spellings. Three are not:
//!
//! - **A shift's count.** `i64.shl` takes two `i64`s where `lshl` takes a `long` and an `int`, so the
//!   count is converted to the *result's* width here and left alone there.
//! - **Float-to-integer conversion traps.** `i32.trunc_f64_s` refuses a NaN, where JLS §5.1.3 wants a
//!   0 — so the saturating `trunc_sat` forms are the ones that mean what Java means.
//! - **There is no integer negation.** `-n` on an `int` is `0 - n`, which puts the zero on the stack
//!   *before* the operand rather than after it.

use alloc::borrow::ToOwned as _;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString as _};
use alloc::vec::Vec;
use core::cell::RefCell;
use core::ops::Range;

use jals_hir::{
    ClassTy, DefId, DefKind, FileId, ItemId, MemberId, Namespace, Primitive, ProjectIndex, Ty,
    TypedFile,
};
use jals_syntax::SyntaxKind::{
    ANNOTATION_TYPE_DECL, CLASS_BODY, CLASS_DECL, CONSTRUCTOR_DECL, ENUM_BODY, ENUM_DECL,
    FIELD_DECL, INITIALIZER, INTERFACE_DECL, LAMBDA_EXPR, METHOD_DECL, METHOD_REF_EXPR,
    RECORD_DECL,
};
use jals_syntax::ast::{self, AstNode as _};
use jals_syntax::{SyntaxNode, SyntaxToken};

use crate::desc::Descriptor;
use crate::facts::{ArmLabels, Facts, Literal};
use crate::facts::{Numeric, Operator, Unary};
use crate::wasm::abi::{self, ClassType, ExportType, LibraryAbi, Realm, Source};
use crate::wasm::encode::{
    CompType, ExportKind, FieldType, Func, Global, HeapType, Module, RefType, StorageType, SubType,
    ValType,
};
use crate::wasm::insn::{Insn, Instr, NumOp, NumericVal as _};
use crate::wasm::positions::{self, Position, Positions};

/// Why a project could not be compiled to wasm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WasmError {
    /// A construct this backend does not emit.
    Unsupported(&'static str),
    /// A name the index did not resolve.
    Unresolved(String),
    /// A type with no wasm representation here — a library type no linked package provides, or a
    /// class this compile never lays out.
    NoRepresentation(String),
    /// A method this module declares but supplies no body for, called somewhere a body is needed.
    ///
    /// Distinct from [`NoRepresentation`](Self::NoRepresentation), which is about a type this
    /// backend cannot spell at all: the owner here *is* a project type, and what is missing is the
    /// implementation — a `native` method, or an interface method whose only implementation in the
    /// source is a lambda or a method reference this backend does not lower into a struct. Reported
    /// under its own name because reporting it as the other one sends a reader looking for a library
    /// dependency in a file that has none.
    NoImplementation(String),
    /// A length or an index outgrew the `u32` the binary format spells it with. Not reachable from
    /// a project that fits in memory, and reported rather than truncated because a wrong length is
    /// bytes an engine reads as something else.
    TooLarge,
    /// A failure with the source it happened at: the file the host handed the compile, and the byte
    /// range within it.
    ///
    /// Attached by the statement and expression boundaries — see [`Lowering::stmt`] — rather than at
    /// each error site, so it is the innermost construct whose lowering failed that is named, and a
    /// failure outside any body (a class's field types, the module's own lengths) carries none. The
    /// innermost attachment is the one kept: an expression's boundary runs before its parent's on
    /// the way out, and the parent is not where a report should point.
    Located {
        /// What went wrong.
        error: Box<Self>,
        /// The file's identity as the host numbered the inputs, which is how the host can turn it
        /// back into a path.
        file: FileId,
        /// The byte range within that file.
        range: Range<usize>,
    },
}

impl WasmError {
    /// This error, with the source it happened at when it does not already carry one.
    ///
    /// Called from a boundary lowering is about to leave. The first call on the way out of a body
    /// is the innermost construct that failed, and every enclosing boundary after it keeps that
    /// one rather than replacing it with its own coarser span.
    fn at(self, file: FileId, range: Range<usize>) -> Self {
        match self {
            error @ Self::Located { .. } => error,
            error => Self::Located {
                error: Box::new(error),
                file,
                range,
            },
        }
    }

    /// The failure itself, without the source a boundary attached.
    ///
    /// What a caller that *classifies* errors matches on — the wasm corpus decides whether a
    /// construct is outside the subset it tests — while a caller that reports one formats the
    /// error it was given, whose [`Display`](core::fmt::Display) is the same either way.
    pub fn kind(&self) -> &Self {
        match self {
            Self::Located { error, .. } => error.kind(),
            error => error,
        }
    }

    /// Where in the source this failure happened, when a boundary knew: the file the host handed
    /// the compile, and the byte range within it.
    ///
    /// The file id is the host's — the same one [`TypedFile::file`] answers — and only the host can
    /// turn it back into a path.
    pub fn location(&self) -> Option<(FileId, Range<usize>)> {
        match self {
            Self::Located { file, range, .. } => Some((*file, range.clone())),
            _ => None,
        }
    }
}

impl core::fmt::Display for WasmError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unsupported(what) => write!(f, "{what} is not compiled to wasm yet"),
            Self::Unresolved(name) => write!(f, "`{name}` did not resolve"),
            Self::NoRepresentation(ty) => write!(
                f,
                "`{ty}` has no wasm representation: this module lays out the classes the project \
                 declares and the classes a linked package provides, and no package linked into \
                 this compile provides this one"
            ),
            Self::NoImplementation(what) => write!(
                f,
                "`{what}` is declared in this project but no body for it is compiled into the \
                 module, so a call to it has nothing to reach"
            ),
            Self::TooLarge => f.write_str("the module exceeded a WebAssembly format limit"),
            Self::Located { error, .. } => error.fmt(f),
        }
    }
}

impl core::error::Error for WasmError {}

/// A source fact this backend could not be given. Both variants are ones `WasmError` already
/// spells, so the `&'static str` of an `Unsupported` reaches a caller verbatim — several are pinned
/// by name in the integration tests.
impl From<crate::facts::FactError> for WasmError {
    fn from(error: crate::facts::FactError) -> Self {
        match error {
            crate::facts::FactError::Unsupported(what) => Self::Unsupported(what),
            crate::facts::FactError::Unresolved(name) => Self::Unresolved(name),
        }
    }
}

type Result<T> = core::result::Result<T, WasmError>;

/// The parameter an `append` overload has to take, for one operand of a concatenation.
///
/// The operand's *static* type names one overload and no other (JLS §15.18.1), and the two shapes
/// a parameter can have are a primitive — matched by the primitive itself — and a class, matched by
/// fully-qualified name. A class is not matched by identity because the same class has two
/// spellings in one index: a name written in source resolves to the class the index holds, while a
/// `String` inferred as an operand's type may be the *external* one the operator synthesised.
enum Appended {
    /// A primitive parameter, matched exactly — except that `byte` and `short` have already become
    /// `int` by the time a concatenation sees them, so `Appended::Primitive` never holds either.
    Primitive(Primitive),
    /// A class parameter, matched by fully-qualified name.
    Named(&'static str),
}

impl Appended {
    /// The spelling a diagnostic uses for the overload that is missing.
    fn name(&self) -> alloc::string::String {
        match self {
            Self::Primitive(primitive) => alloc::format!("{}", Ty::Primitive(*primitive)),
            Self::Named(name) => (*name).to_owned(),
        }
    }
}

/// A method the module defines: where its body is and what it compiled to.
struct Method {
    /// The declaring class, or `None` for a `static` method that needs no receiver.
    owner: Option<ItemId>,
    /// The declaration node, for the second pass that lowers its body.
    node: SyntaxNode,
    /// Which input the node came from.
    input: usize,
    /// Index into the type section for the function's signature.
    signature: u32,
    /// Index in the function section.
    index: u32,
    /// The exported name, when this is a `public static` method.
    export: Option<String>,
    /// A constructor initialises `this` rather than returning a value.
    is_constructor: bool,
    /// The class this constructor's extra first parameter holds, when it belongs to an inner class.
    encloses: Option<ItemId>,
    /// How many trailing parameters hold captured locals.
    captures: usize,
    /// Whether this is the synthesised constructor of a class that declares none: its `node` is the class body
    /// and its only work is the initialisers.
    initialises: Option<ItemId>,
    /// The interface method this body implements, when the "method" is a lambda expression rather than a
    /// declaration. A lambda's captures are *fields*, not parameters, so only its own parameters are bound.
    lambda: Option<MemberId>,
    /// The type the function's signature results in, which is what a `return` narrows its value to —
    /// and whose presence is what decides whether the body needs a trailing `unreachable`.
    ///
    /// One field rather than a `bool` beside it: the two were always set together from the same
    /// `results` vector, so a site that set one and forgot the other would emit either a trailing
    /// `unreachable` on a `void` function or none on a value-returning one — a module the validator
    /// rejects for a reason the pair hid.
    result: Option<ValType>,
}

/// Compiles a whole project to one WebAssembly module.
pub struct CompileWasm;

/// What a compile does with the source beyond lowering it.
///
/// One struct rather than a second entry point per choice, so a caller states what it wants and a
/// new choice reaches every caller as a field with a default rather than as a signature they all
/// have to be edited for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WasmOptions {
    /// Whether `assert` evaluates its condition and traps when it is false.
    ///
    /// Off by default, which is what a JVM does with an `assert` unless it was started with `-ea`
    /// — so an ordinary `jals build` behaves the same on both backends. There is no `-ea` here to
    /// turn it on later: a wasm host has no flag for it and this backend emits no
    /// `$assertionsDisabled` global to read one into, so the decision is the compile's.
    ///
    /// A test run is what turns it on, and for the reason the JVM test runner prepends `-ea`: a
    /// suite written with `assert` and compiled without this passes without checking anything,
    /// which is the failure that looks exactly like success.
    pub assertions: bool,

    /// Whether every statement of the *project's own* inputs records where it was written, so a
    /// *run* can report a line.
    ///
    /// On, each such statement stores its index into the module's exported `$jals$position` global
    /// before its own code runs, and the module carries a `jals.positions` table saying what each
    /// index means. The compile's own errors know where they happened without this — an error is
    /// attached at the lowering that raised it — while a trap has only what the code left behind,
    /// and the global is it.
    ///
    /// The sources a native package publishes are excluded, because the global is the *last*
    /// statement entered: with a package's own statements writing it too, a trap raised there — a
    /// host refusal on the caller's behalf — would report a line of a file the caller cannot edit.
    /// Excluded, the last write is the project statement that called in.
    ///
    /// Off by default: two instructions per statement and one exported global are a real cost, and
    /// the host that reads them back is the build-script engine, where a failure with no line in
    /// it is a failure nobody can act on.
    pub positions: bool,
}

/// What the module being built *is*.
///
/// The two surfaces differ in exactly two things: which declarations reach the export section,
/// and whether the compile has to synthesize the entry points a consumer needs — a constructor
/// factory, a `static` field's accessor pair — because the in-module shape of those (a `this`
/// parameter, a global) is not callable from outside. Everything else — every type, every body —
/// is the same work either way, which is why this is a parameter and not a second lowering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Surface {
    /// The module a `jals run` executes: bare-name exports for `static` methods, no factories.
    Project,
    /// The module another compile links against: canonical member keys, factories, accessors.
    Library,
}

/// What a library surface publishes, as [`CompileWasm::build`] hands it back.
///
/// The three lists travel together because they are one answer: a consumer replays the classes'
/// structs, knows the interfaces by name, and imports the exports. A project has none of the
/// three — its surface is the bare-name exports its own pass recorded.
#[derive(Debug, Default)]
struct LibrarySurface {
    /// Every class the module declared and the type index its struct occupies.
    classes: Vec<ClassType>,
    /// Every interface the module declared, by internal name.
    interfaces: Vec<String>,
    /// Every function the module exports and the type its signature occupies.
    functions: Vec<ExportType>,
    /// The dispatch realm, when the module lowered at least one virtual call a consumer could
    /// answer — see [`Realm`].
    realm: Option<Realm>,
}

/// A library a project links against: what to call it and what it said about itself.
///
/// The ABI is carried by the module's own `jals.library` section, decoded by the host — a
/// `no_std` compiler has no wasm parser, and the section is written by the compile that encoded
/// the very types a consumer replays.
#[derive(Debug, Clone, Copy)]
pub struct LinkedLibrary<'a> {
    /// The link name, which every import from this library is spelled with.
    pub name: &'a str,
    /// What the library's `jals.library` section said.
    pub abi: &'a LibraryAbi,
}

impl CompileWasm {
    /// Emit the module's bytes. `index` must have been built over exactly `inputs` and
    /// `libraries`.
    ///
    /// [`module`](Self::module) with its encoding run; a module whose own lengths do not fit the
    /// `u32` the format spells them with is refused rather than truncated.
    pub fn project(
        inputs: &[TypedFile<'_>],
        libraries: &[TypedFile<'_>],
        index: &ProjectIndex,
        options: WasmOptions,
    ) -> Result<Vec<u8>> {
        Self::module(inputs, libraries, index, options)?
            .finish()
            .ok_or(WasmError::TooLarge)
    }

    /// Emit the module, before it becomes bytes. `index` must have been built over exactly `inputs`.
    ///
    /// The whole project is one module: the target has no dynamic loading and no classpath, so
    /// there is nothing for a second one to be. Handing back the [`Module`] rather than only its
    /// encoding is what lets a caller ask what was emitted — the same footing
    /// [`Assembler`](crate::jvm::Assembler) gives the other backend, and the only way to assert a
    /// lowering without an engine to run it.
    /// How many locals a function may declare beyond its parameters.
    ///
    /// The format itself allows far more, and every engine caps it — 50 000 is what `wasmparser`
    /// (and so `wasm-tools` and `wasmtime`) enforces. A body over the cap is refused for the same
    /// reason a method over the JVM's 64 KiB code limit is: the target says no, and saying so here
    /// is the difference between a gap and a module that will not load.
    const MAX_LOCALS: usize = 50_000;

    /// Push one function, refusing a body whose locals walk past what an engine will load.
    ///
    /// One function rather than a check beside each `push`, because the bodies that reach a module
    /// are not one loop: a method's, a synthesised one's, and a class initialiser's. The last is the
    /// *most* likely to be over the cap — it gathers every `static {}` block and every computed
    /// `static` field initialiser of a class into a single function — and it was the one a check
    /// living inside the methods loop did not cover, so a `static` initialiser with 50 000 locals
    /// was written out as a module `wasm-tools` refuses to validate while the identical body in an
    /// ordinary method was correctly reported as [`WasmError::TooLarge`].
    fn push_func(module: &mut Module, func: Func) -> Result<()> {
        if func.locals.len() > Self::MAX_LOCALS {
            return Err(WasmError::TooLarge);
        }
        module.funcs.push(func);
        Ok(())
    }

    pub fn module(
        inputs: &[TypedFile<'_>],
        libraries: &[TypedFile<'_>],
        index: &ProjectIndex,
        options: WasmOptions,
    ) -> Result<Module> {
        Self::build(inputs, libraries, &[], index, options, Surface::Project)
            .map(|(module, _)| module)
    }

    /// Compile a package as a **linked library**: the module another compile links against.
    ///
    /// The code is the same code [`module`](Self::module) would emit — every type, every body —
    /// and what differs is the surface and the artifact:
    ///
    /// - Every non-private method and constructor is exported under a canonical key derived from
    ///   its declaration alone (`owner#name+descriptor`), so a consumer can name it without a
    ///   lookup table.
    /// - A constructor is exported as a *factory*, because the in-module shape — a `this`
    ///   parameter and a `void` result — is not callable from outside. The factory allocates,
    ///   runs the constructor (or the class's initialisers, for the constructor the language gives
    ///   a class that declares none), and returns the object. Its **body** is exported beside the
    ///   factory under the factory's key with `#init` appended: a consumer's `super(…)` has an
    ///   object already and takes the `(this, params) -> ()` shape, which is the one a factory
    ///   cannot offer.
    /// - A `static` field is exported as an accessor pair, because the in-module shape is a
    ///   global and a global's name is not a Java signature.
    /// - The module's exception tag is exported, so a consumer can catch what the library throws.
    /// - Its **dispatch realm**, when it lowered any open virtual call a consumer class could
    ///   answer: one slot per dispatched method, a struct holding them, and the `$jals$link` entry
    ///   that installs it — see [`Realm`]. A library whose calls all close over its own classes
    ///   publishes none.
    /// - The [`LibraryAbi`] travels in a custom section beside the code: the type groups a
    ///   consumer must replay, the class-to-struct map, the interface names, the realm, and the
    ///   package's Java for the index.
    ///
    /// `sources` is the same Java that was compiled, and it is the package's API: a consumer's
    /// index reads the text, not a copy of it.
    pub fn library(
        inputs: &[TypedFile<'_>],
        index: &ProjectIndex,
        options: WasmOptions,
        package: &str,
        version: u32,
        sources: Vec<Source>,
    ) -> Result<(Module, LibraryAbi)> {
        let (mut module, surface) =
            Self::build(inputs, &[], &[], index, options, Surface::Library)?;
        let abi = LibraryAbi {
            package: package.to_owned(),
            version,
            sources,
            classes: surface.classes,
            interfaces: surface.interfaces,
            functions: surface.functions,
            realm: surface.realm,
            groups: module.groups().to_vec(),
            types: module.types().to_vec(),
        };
        module.add_custom_section(abi::CUSTOM_SECTION.to_owned(), abi.write());
        Ok((module, abi))
    }

    /// Compile a project that links against precompiled libraries.
    ///
    /// `index` must have been built over `inputs` **and** the libraries' published Java, because
    /// resolution reads the same declarations the library compiled. Each library's types are
    /// replayed into the module before the project's own — canonicalisation is per group, so the
    /// two modules meet only where the declarations are identical — and every member the project
    /// can name becomes an import from the library's link name.
    pub fn project_linked(
        inputs: &[TypedFile<'_>],
        libraries: &[TypedFile<'_>],
        linked: &[LinkedLibrary<'_>],
        index: &ProjectIndex,
        options: WasmOptions,
    ) -> Result<Vec<u8>> {
        Self::build(inputs, libraries, linked, index, options, Surface::Project)?
            .0
            .finish()
            .ok_or(WasmError::TooLarge)
    }

    fn build(
        inputs: &[TypedFile<'_>],
        libraries: &[TypedFile<'_>],
        linked: &[LinkedLibrary<'_>],
        index: &ProjectIndex,
        options: WasmOptions,
        surface: Surface,
    ) -> Result<(Module, LibrarySurface)> {
        // Everything below reads one list. A library class is laid out, has its bodies lowered and
        // is called exactly as a project class is — the *only* thing the two lists decide is which
        // declarations reach the export section, which is what `exported` carries into
        // `collect_methods`. `TypedFile` is `Copy`, so joining them costs one pointer-sized copy
        // each and saves every pass below from taking two slices and an index rule.
        let project_inputs = inputs.len();
        let inputs: Vec<TypedFile<'_>> = inputs
            .iter()
            .copied()
            .chain(libraries.iter().copied())
            .collect();
        let inputs = inputs.as_slice();
        let mut module = Module::new();
        let mut layout = Layout {
            object: index.item_by_fqn("java.lang.Object"),
            assertions: options.assertions,
            ..Layout::default()
        };

        // A linked library's types are replayed *first*, in the groups its own compile declared:
        // canonicalisation is per group, so the two modules meet only where the declarations are
        // identical. Everything the project declares then goes after a boundary of its own.
        for library in linked {
            Self::replay_library(library, index, &mut module, &mut layout)?;
        }

        // Pass 1: every class *reserves* a struct type index, in an order where a supertype comes
        // first so its field prefix is known when the subtype is laid out. Only the index is fixed
        // here — the body waits, because a field of array type needs an array type index and an
        // array's element may be one of these classes.
        let mut interface_items = Vec::new();
        let mut inner_items = Vec::new();
        let mut captured_items = Vec::new();
        let classes = Self::classes_in_order(
            inputs,
            index,
            &mut interface_items,
            &mut inner_items,
            &mut captured_items,
        )?;
        for (item, enclosing) in inner_items {
            layout.inner.insert(item, enclosing);
        }
        for (item, captured) in captured_items {
            layout.captures.insert(item, captured);
        }
        // What the library surface gates on is "was this item compiled here", and that is every type
        // declaration — interfaces included. `classes` alone is the wrong set: it deliberately
        // excludes interfaces, which have no struct, and gating on it silently dropped every
        // interface's `default`/`static` method and implicitly-static field out of the surface.
        let compiled: BTreeSet<ItemId> = classes
            .iter()
            .chain(interface_items.iter())
            .copied()
            .collect();
        // An interface has no struct type, so it is registered before any class is laid out: a field or
        // a parameter of interface type has to resolve to *something* while the structs are built.
        for &item in &interface_items {
            layout.interfaces.insert(item);
        }
        // One tag carries every Java exception: every one of them is a reference, so what a `catch`
        // tests is the class of the payload, not which tag raised it. A module that links a library
        // exporting one *imports* it instead of declaring its own — two tags with the same payload
        // type are still two different tags, and a `try_table` naming the local one would never see
        // the library's `throw`. Which of the two this module gets is settled in
        // `collect_linked_imports`, where the replayed payload's type index is known; a module with
        // no linked tag declares one of its own, which costs three bytes even if nothing throws.
        if !linked
            .iter()
            .any(|library| library.abi.export_type("$jals$tag").is_some())
        {
            let payload = module.add_type(SubType::plain(CompType::Func {
                params: alloc::vec![ValType::Ref(RefType::nullable(HeapType::Any))],
                results: Vec::new(),
            }));
            module.tags.push(payload);
            layout.tag =
                Some(u32::try_from(module.tags.len() - 1).map_err(|_| WasmError::TooLarge)?);
            layout.tag_type = Some(payload);
        }
        for &item in &classes {
            layout.reserve_class(item, index, &mut module);
        }
        // Then every array type the program mentions. wasm has one declared type per element
        // type, and a body cannot introduce one mid-lowering, so they are collected from the
        // types the analyses already recorded rather than discovered while emitting.
        for input in inputs {
            for node in input.root().descendants() {
                if let Some(ty) = input.type_of_expr(Facts::span(&node)) {
                    layout.declare_array(ty, &mut module)?;
                }
            }
            for def in input.analysis().defs() {
                let ty = input.type_of_def(def.id).clone();
                layout.declare_array(&ty, &mut module)?;
            }
        }
        // Then the string literals, whose characters are module data rather than an instruction. A
        // segment has to exist before any body names it, and the `char[]` it copies into has to
        // exist before that.
        for input in inputs {
            Self::collect_literals(input, &mut layout, &mut module)?;
        }
        // Every `native` method becomes a host import, and every import occupies the function
        // index space *before* the first defined function — so they are all declared here, in a
        // sweep of their own, ahead of anything that asks the module for an index.
        for input in inputs {
            Self::collect_imports(input, index, &mut layout, &mut module)?;
        }
        // Then every member a linked library exports that the project could call. Imports occupy
        // the function index space before the first defined function, so — like a `native`
        // declaration — they are all declared before any index is handed out.
        Self::collect_linked_imports(linked, index, &mut layout, &mut module)?;
        // Now every index is known, so the struct bodies can name array types and vice versa.
        for &item in &classes {
            layout.fill_class(item, index, &mut module)?;
        }
        // Then every `static` field, which is module state rather than a struct slot. After the
        // arrays, because a `static int[]` field's global needs its array type to exist.
        // `(field, initialiser)` for every `static` field whose value has to be *computed*, plus the
        // `static { … }` blocks, all of which run in the start function.
        let mut deferred = Vec::new();
        for input in inputs {
            layout.declare_statics(input, index, &mut module, &mut deferred)?;
        }
        // An `enum`'s constants are `static final` fields of the enum's own type, and the source writes
        // no initialiser for any of them: each one is an allocation, which is not a constant expression.
        // So they get globals here and are built in the start function, in declaration order.
        let mut constants = Vec::new();
        for input in inputs {
            layout.declare_constants(input, index, &mut module, &mut constants)?;
        }

        // A *library* publishes a dispatch realm: a struct of function references its consumer
        // fills with dispatchers over its own classes. Both halves have to exist before the first
        // body is lowered — an arm names the struct type and the global — so the type is reserved
        // and the global declared here, and the struct is filled once the last body has registered
        // its slots. A library whose calls all close over its own classes ends with no slots and
        // publishes no realm; the reservation is then a fieldless struct nothing refers to.
        if surface == Surface::Library {
            let structure = module.reserve_type();
            let ty = ValType::Ref(RefType::nullable(HeapType::Concrete(structure)));
            let global = module.global_index(module.globals.len());
            module.globals.push(Global {
                ty,
                init: alloc::vec![Instr::RefNull(HeapType::Concrete(structure))],
            });
            layout.realm = Some(RealmBuild {
                structure,
                global,
                slots: RefCell::new(Vec::new()),
            });
        }

        // A compile that asked for positions records where each statement was written: one index
        // per statement, stored before that statement's own code runs, and a `jals.positions`
        // section saying what each index means. The global has to exist before any body stores to
        // it, and it is exported because an engine reads it back *after* a trap, when nothing else
        // can still name the stack. Initialised to `-1`: "no statement has run", which is what a
        // failure during instantiation reads as.
        if options.positions {
            let global = module.global_index(module.globals.len());
            module.globals.push(Global {
                ty: ValType::I32,
                init: alloc::vec![Instr::I32Const(-1)],
            });
            module.exports.push((
                positions::POSITION_GLOBAL.to_owned(),
                ExportKind::Global,
                global,
            ));
            layout.positions = Some(PositionsBuild {
                global,
                statements: RefCell::new(Vec::new()),
            });
        }

        // Pass 2: every method gets a signature and a function index, so a call emitted in pass 3
        // can name a function declared later in the source.
        // Everything the module imports is declared by now — a `native`'s host import and every
        // linked member — so the boundary between the import and defined halves of the function
        // index space is fixed here, and a member below it is one whose body this module does not
        // hold. See [`Layout::first_function`].
        layout.first_function = module.func_index(0);
        let mut methods = Vec::new();
        for (position, input) in inputs.iter().enumerate() {
            Self::collect_methods(
                input,
                position,
                surface == Surface::Project && position < project_inputs,
                index,
                &mut layout,
                &mut module,
                &mut methods,
            )?;
        }

        // A library's dispatch arms call at a method's declared type, and a stub's methods have no
        // declaration here to read one from. Made before any body is lowered, because a body is
        // where the first arm for one can appear.
        if layout.realm.is_some() {
            Self::collect_stub_types(
                &classes,
                &interface_items,
                &compiled,
                index,
                &mut layout,
                &mut module,
            );
        }

        // A record's canonical constructor and its accessors have no declaration to walk: the header
        // declares the components and the compiler owes the rest. They are synthesised here, after every
        // *declared* method has its index, so the indices stay in step with the order bodies are pushed.
        let synthesised = Self::record_members(inputs, index, &mut layout, &mut module, &methods)?;

        // Each class's initialisation is a function of its own, reserved before any body so a body can
        // call it. A body has to: JLS §12.4.1 initialises a class on its first *use*, and this module's
        // one start function cannot express that ordering — a class declared later may be read by one
        // declared earlier, which is what left an `enum` constant built from a `static` field that was
        // still zero.
        let blocks: Vec<(usize, ItemId, ast::Block)> = inputs
            .iter()
            .enumerate()
            .flat_map(|(position, input)| {
                Self::static_initializers(input.root(), input, index)
                    .into_iter()
                    .map(move |(owner, block)| (position, owner, block))
            })
            .collect();
        let state = StaticState {
            deferred: &deferred,
            constants: &constants,
            blocks: &blocks,
        };
        Self::reserve_class_inits(
            inputs,
            index,
            &mut layout,
            &mut module,
            &state,
            methods.len() + synthesised.len(),
        );
        let inits =
            Self::class_initializers(inputs, index, &layout, &state, &mut module, project_inputs)?;

        // Pass 3: bodies.
        for method in &methods {
            let input = &inputs[method.input];
            // Only the project's own inputs record statement positions; a native package's Java is
            // host code the script's author cannot edit. See [`WasmOptions::positions`].
            let body = Body::lower(method, input, index, &layout, method.input < project_inputs)?;
            // Every engine caps a function's locals, and a body that walks past the cap is a module
            // no engine loads. Said here rather than left to the validator: a generated source with
            // thousands of locals is a *refusal* like any other format limit, and emitting the bytes
            // anyway reports it as a compiler defect.
            Self::push_func(
                &mut module,
                Func {
                    type_index: method.signature,
                    locals: body.locals,
                    body: body.code,
                },
            )?;
            // A wasm export carries a bare name and no owner, so two `static` methods of one name —
            // an overload pair, or one method per class — name the same export. A module with two
            // is not a module at all, so the second is dropped: the name is ambiguous, and an
            // ambiguous export cannot be paired with a javac method either.
            if let Some(name) = &method.export
                && !module.exports.iter().any(|(exported, ..)| exported == name)
            {
                module
                    .exports
                    .push((name.clone(), ExportKind::Func, method.index));
            }
        }
        for func in synthesised {
            Self::push_func(&mut module, func)?;
        }
        for func in &inits {
            Self::push_func(&mut module, func.clone())?;
        }
        // Every class's initialisation, called in source order. A class whose initialisers read
        // another's have already run it by then — each one guards itself, so calling it again is free.
        if !inits.is_empty() {
            let mut insn = Insn::new();
            for &(function, _) in layout.class_inits.values() {
                insn.call(function);
            }
            let signature = module.add_type(SubType::plain(CompType::Func {
                params: Vec::new(),
                results: Vec::new(),
            }));
            let start = module.func_index(module.funcs.len());
            module.funcs.push(Func {
                type_index: signature,
                locals: Vec::new(),
                body: insn.into_body(),
            });
            module.start = Some(start);
        }
        // The consumer's side of every realm: one thunk per slot, and a start function that
        // installs the structs before the class initialisers run. Emitted last, because a thunk
        // names every function and struct the module holds.
        Self::link_realms(index, &layout, &mut module)?;
        // A linked library's surface is added last: its factories and accessors are functions the
        // lowering above knows nothing about, and they may only be pushed once every index they
        // name exists.
        let published = match surface {
            Surface::Project => LibrarySurface::default(),
            Surface::Library => Self::export_library(
                &classes,
                &interface_items,
                &compiled,
                index,
                &layout,
                &mut module,
            )?,
        };
        // The table the instrumented statements wrote their indices into. Emitted once every body
        // has had its say, and not at all when there is nothing to say: a module whose inputs
        // declare no statements carries no section, and a reader finds none.
        if let Some(build) = &layout.positions {
            let statements = build.statements.borrow();
            if !statements.is_empty() {
                module.add_custom_section(
                    positions::CUSTOM_SECTION.to_owned(),
                    Positions::encode(&statements),
                );
            }
        }
        Ok((module, published))
    }

    /// Add the entry points a linked library's consumers call.
    ///
    /// The keys are derived from the declaration alone — the owner's internal name, the member's
    /// name, the JVM descriptor — so neither side of a link needs a lookup table and neither can
    /// drift from the other. Two shapes are not direct exports:
    ///
    /// - A **constructor** takes a `this` and returns nothing, which no consumer can call. A
    ///   factory is synthesized beside it: allocate, run the constructor, hand back the object.
    ///   Its *body* is exported too, under the factory's key with `#init` appended, because a
    ///   consumer's `super(…)` has an object already and the factory would allocate a second one.
    /// - A **`static` field** is a wasm global in this module, and a global's name is not a Java
    ///   signature. An accessor pair is synthesized instead, each triggering the class's
    ///   initialiser first — the same thing an in-module read does.
    ///
    /// Host imports (a `native` method's function) are skipped: they are what the library's own
    /// embedder supplies, not what the library defines.
    ///
    /// What is described here goes into the [`LibraryAbi`]: the class-to-struct map, the interface
    /// names, and the export list a consumer imports through.
    fn export_library(
        classes: &[ItemId],
        interfaces: &[ItemId],
        compiled: &BTreeSet<ItemId>,
        index: &ProjectIndex,
        layout: &Layout,
        module: &mut Module,
    ) -> Result<LibrarySurface> {
        let first_defined = module.func_index(0);
        let mut functions = Vec::new();

        for (&member, &function) in &layout.functions {
            let info = index.member(member);
            let owner = info.owner;
            if !compiled.contains(&owner) || info.modifiers.is_private || function < first_defined {
                continue;
            }
            match info.kind {
                DefKind::Method => {
                    let key = Self::member_key(owner, member, index)?;
                    let type_index = Self::defined_type_index(module, function, first_defined)?;
                    module
                        .exports
                        .push((key.clone(), ExportKind::Func, function));
                    functions.push(ExportType {
                        name: key,
                        type_index,
                    });
                }
                DefKind::Constructor => {
                    let Some(&structure) = layout.structs.get(&owner) else {
                        continue;
                    };
                    let (factory, type_index) = Self::constructor_factory(
                        member, function, structure, layout, module, index,
                    )?;
                    let key = Self::member_key(owner, member, index)?;
                    module
                        .exports
                        .push((key.clone(), ExportKind::Func, factory));
                    functions.push(ExportType {
                        name: key.clone(),
                        type_index,
                    });
                    // The body beside the factory. A consumer's `super(…)` has an object already
                    // and the factory would allocate a second one, so the constructor's own
                    // `(this, arguments…) -> ()` shape is exported too, under a name the consumer
                    // derives from the same key.
                    let init_key = Self::initializer_key(&key);
                    let body_type = Self::defined_type_index(module, function, first_defined)?;
                    module
                        .exports
                        .push((init_key.clone(), ExportKind::Func, function));
                    functions.push(ExportType {
                        name: init_key,
                        type_index: body_type,
                    });
                }
                _ => {}
            }
        }

        // A class that writes no constructor still has one — the default JLS §8.8.9 gives it — and
        // the index synthesises the member. Nothing lowered a *function* for it, so the factory is
        // built here: allocate, run the class's initialisers when it has any, return. This is the
        // common Java shape (`class Counter { private int count = 3; }`), and without it a consumer
        // finds the class in the map and no way to construct one.
        //
        // The factory stores the object in a local and re-reads it: the receiver has to survive
        // the initialiser call, and a copy left on the stack by a class with nothing to run is a
        // module no validator accepts.
        for &item in classes {
            if layout.constructors(index, item).next().is_some() {
                continue;
            }
            let Some(member) = index.own_members(item).iter().copied().find(|&member| {
                index.member(member).kind == DefKind::Constructor
                    && index.member(member).params.is_empty()
            }) else {
                continue;
            };
            let Some(&structure) = layout.structs.get(&item) else {
                continue;
            };
            // An inner class's factory takes its enclosing instance first, exactly as a declared
            // constructor's does: the synthesized initialiser has no parameter for it — its
            // signature is the object alone — so the factory writes the synthetic field itself,
            // and a consumer's `outer.new Inner()` passes the instance it named.
            let enclosing = layout
                .inner
                .get(&item)
                .copied()
                .zip(layout.outer.get(&item).copied());
            let params = match enclosing {
                Some((outer, _)) => alloc::vec![layout.class_ref(outer)?],
                None => Vec::new(),
            };
            let slot = u32::try_from(params.len()).map_err(|_| WasmError::TooLarge)?;
            let result = layout.class_ref(item)?;
            let factory_ty = module.add_type(SubType::plain(CompType::Func {
                params,
                results: alloc::vec![result],
            }));
            let mut insn = Insn::new();
            insn.struct_new_default(structure).local_set(slot);
            if let Some((_, field)) = enclosing {
                insn.local_get(slot)
                    .local_get(0)
                    .struct_set(structure, field);
            }
            // The initialisers to run are not always this class's: a class with none of its own
            // still runs its nearest ancestor's, and the in-module `new` asks the same question
            // through [`Body::inherited_initialiser`] — shared here so a factory and a `new`
            // cannot disagree about which chain runs. A library subclass whose initialisers all
            // live in an ancestor runs none of them when this asks only about the class itself,
            // and the object comes out with every inherited field at its default.
            let initializer = Body::inherited_initialiser(item, index, layout)?;
            if let Some(init) = initializer {
                insn.local_get(slot).call(init);
            }
            insn.local_get(slot);
            let factory = module.func_index(module.funcs.len());
            Self::push_func(
                module,
                Func {
                    type_index: factory_ty,
                    locals: alloc::vec![result],
                    body: insn.into_body(),
                },
            )?;
            let key = Self::member_key(item, member, index)?;
            module
                .exports
                .push((key.clone(), ExportKind::Func, factory));
            functions.push(ExportType {
                name: key.clone(),
                type_index: factory_ty,
            });
            // What the factory ran is also what a consumer's implicit `super()` runs, so the
            // function is exported for it under the initializer key, exactly as a declared
            // constructor's body is.
            if let Some(init) = initializer {
                let init_key = Self::initializer_key(&key);
                let type_index = Self::defined_type_index(module, init, first_defined)?;
                module
                    .exports
                    .push((init_key.clone(), ExportKind::Func, init));
                functions.push(ExportType {
                    name: init_key,
                    type_index,
                });
            }
        }

        // A `static` field is module state; the surface is a getter and a setter.
        for (&member, &global) in &layout.statics {
            let info = index.member(member);
            if info.kind != DefKind::Field
                || !info.modifiers.is_static
                || info.modifiers.is_private
                || !compiled.contains(&info.owner)
            {
                continue;
            }
            let ty = layout.val_type(&index.resolved_member_ty(member))?;
            let owner_name = Descriptor::internal_name_of(info.owner, index);
            let name = &info.name;
            let init = layout.class_inits.get(&info.owner).map(|&(f, _)| f);

            let get_ty = module.add_type(SubType::plain(CompType::Func {
                params: Vec::new(),
                results: alloc::vec![ty],
            }));
            let mut get = Insn::new();
            if let Some(init) = init {
                get.call(init);
            }
            get.global_get(global);
            let get_index = module.func_index(module.funcs.len());
            Self::push_func(
                module,
                Func {
                    type_index: get_ty,
                    locals: Vec::new(),
                    body: get.into_body(),
                },
            )?;
            let get_key = alloc::format!("{owner_name}#{name}#get");
            module
                .exports
                .push((get_key.clone(), ExportKind::Func, get_index));
            functions.push(ExportType {
                name: get_key,
                type_index: get_ty,
            });

            let put_ty = module.add_type(SubType::plain(CompType::Func {
                params: alloc::vec![ty],
                results: Vec::new(),
            }));
            let mut put = Insn::new();
            if let Some(init) = init {
                put.call(init);
            }
            put.local_get(0).global_set(global);
            let put_index = module.func_index(module.funcs.len());
            Self::push_func(
                module,
                Func {
                    type_index: put_ty,
                    locals: Vec::new(),
                    body: put.into_body(),
                },
            )?;
            let put_key = alloc::format!("{owner_name}#{name}#put");
            module
                .exports
                .push((put_key.clone(), ExportKind::Func, put_index));
            functions.push(ExportType {
                name: put_key,
                type_index: put_ty,
            });
        }

        // One tag covers every Java throw in the module, so one export lets a consumer catch them
        // all: the payload is the thrown reference, and the *class* of it is what a catch tests.
        // The export alone is not enough to import — a tag is imported *at a type*, and the type
        // has to be the one this module declared — so the payload's index travels beside it, under
        // the same name, and the consumer's import sweep reads it there.
        if let Some(tag) = layout.tag {
            module
                .exports
                .push(("$jals$tag".to_owned(), ExportKind::Tag, tag));
            if let Some(payload) = layout.tag_type {
                functions.push(ExportType {
                    name: "$jals$tag".to_owned(),
                    type_index: payload,
                });
            }
        }

        // The dispatch realm, when this module lowered at least one virtual call a consumer could
        // answer. The struct's type was reserved before the first body — an arm names it — and its
        // slots were registered while bodies were lowered; here the struct is filled, `$jals$link`
        // is emitted, and both travel in the ABI. A library whose calls all closed over its own
        // classes has no slots and publishes nothing: the reservation stays a fieldless struct
        // nothing refers to.
        let realm = match &layout.realm {
            None => None,
            Some(build) => {
                let slots = build.slots.borrow();
                if slots.is_empty() {
                    None
                } else {
                    let fields = slots
                        .iter()
                        .map(|&(_, ty)| FieldType {
                            storage: StorageType::Val(ValType::Ref(RefType::nullable(
                                HeapType::Concrete(ty),
                            ))),
                            mutable: false,
                        })
                        .collect();
                    module.set_type(build.structure, SubType::plain(CompType::Struct(fields)));
                    let ty = ValType::Ref(RefType::nullable(HeapType::Concrete(build.structure)));
                    let link_type = module.add_type(SubType::plain(CompType::Func {
                        params: alloc::vec![ty],
                        results: Vec::new(),
                    }));
                    let link = module.func_index(module.funcs.len());
                    let mut insn = Insn::new();
                    insn.local_get(0).global_set(build.global);
                    Self::push_func(
                        module,
                        Func {
                            type_index: link_type,
                            locals: Vec::new(),
                            body: insn.into_body(),
                        },
                    )?;
                    module
                        .exports
                        .push(("$jals$link".to_owned(), ExportKind::Func, link));
                    functions.push(ExportType {
                        name: "$jals$link".to_owned(),
                        type_index: link_type,
                    });
                    let slots = slots
                        .iter()
                        .map(|&(member, type_index)| {
                            Ok(abi::Slot {
                                name: Self::member_key(index.member(member).owner, member, index)?,
                                type_index,
                            })
                        })
                        .collect::<Result<Vec<_>>>()?;
                    Some(Realm {
                        structure: build.structure,
                        link: link_type,
                        slots,
                    })
                }
            }
        };

        // The class-to-struct map a consumer needs to represent a value of the type at all.
        let mut map = Vec::new();
        for &item in classes {
            if let Some(&type_index) = layout.structs.get(&item) {
                map.push(ClassType {
                    name: Descriptor::internal_name_of(item, index),
                    index: type_index,
                    // An inner class's factory leads with its enclosing instance, so which class
                    // that instance is has to travel: without it the consumer's `outer.new Inner()`
                    // emits a call one argument short.
                    enclosing: layout
                        .inner
                        .get(&item)
                        .map(|&outer| Descriptor::internal_name_of(outer, index)),
                });
            }
        }
        // A name each, because a name is all an interface has: no struct to index and no enclosing
        // instance to construct one through. What the consumer does with it is the same thing this
        // module does — hold its values as `anyref` and dispatch its methods over the classes that
        // implement it.
        let interfaces = interfaces
            .iter()
            .map(|&item| Descriptor::internal_name_of(item, index))
            .collect();
        Ok(LibrarySurface {
            classes: map,
            interfaces,
            functions,
            realm,
        })
    }

    /// The export name of a constructor's **body**, beside the factory that allocates and calls it.
    ///
    /// One more `#` than a member key ever has — a key holds exactly one — so the pair cannot
    /// collide with each other or with a method's, and the two consumers that derive the name (this
    /// module's export loop and a consumer's import loop) both derive it from the same key.
    fn initializer_key(key: &str) -> String {
        alloc::format!("{key}#init")
    }

    /// The type index a defined function carries, which is what an export entry records.
    fn defined_type_index(module: &Module, function: u32, first_defined: u32) -> Result<u32> {
        let defined = usize::try_from(function.saturating_sub(first_defined))
            .map_err(|_| WasmError::TooLarge)?;
        module
            .funcs
            .get(defined)
            .map(|func| func.type_index)
            .ok_or(WasmError::Unsupported(
                "an exported member with no function",
            ))
    }

    /// A constructor as a consumer can call it: allocate, run the constructor, return the object.
    ///
    /// The parameters are the constructor's own, minus the `this` its in-module shape leads with —
    /// which is what makes this work for an inner class too, where the enclosing instance is one of
    /// them and simply becomes an explicit factory parameter.
    fn constructor_factory(
        member: MemberId,
        function: u32,
        structure: u32,
        layout: &Layout,
        module: &mut Module,
        index: &ProjectIndex,
    ) -> Result<(u32, u32)> {
        let type_index = Self::defined_type_index(module, function, module.func_index(0))?;
        let Some(SubType {
            comp: CompType::Func { params, .. },
            ..
        }) = module
            .types()
            .get(usize::try_from(type_index).map_err(|_| WasmError::TooLarge)?)
        else {
            return Err(WasmError::Unsupported(
                "a constructor whose type is no function",
            ));
        };
        let params = params.get(1..).unwrap_or_default().to_vec();
        let result = layout.class_ref(index.member(member).owner)?;

        let factory_ty = module.add_type(SubType::plain(CompType::Func {
            params: params.clone(),
            results: alloc::vec![result],
        }));
        // One local holding the object between allocating it and returning it; it sits after the
        // factory's own parameters, so its index is their count.
        let slot = u32::try_from(params.len()).map_err(|_| WasmError::TooLarge)?;
        let mut insn = Insn::new();
        insn.struct_new_default(structure)
            .local_set(slot)
            .local_get(slot);
        for position in 0..slot {
            insn.local_get(position);
        }
        insn.call(function).local_get(slot);
        let factory = module.func_index(module.funcs.len());
        Self::push_func(
            module,
            Func {
                type_index: factory_ty,
                locals: alloc::vec![result],
                body: insn.into_body(),
            },
        )?;
        Ok((factory, factory_ty))
    }

    /// `owner#name+descriptor`, the one spelling a linked member is exported and imported under.
    ///
    /// A constructor is spelled `<init>`, as the class-file format spells it: the index keeps the
    /// simple name the source wrote, and two members named `Counter` — the class's own name and a
    /// method that happens to share it — would otherwise key the same.
    fn member_key(owner: ItemId, member: MemberId, index: &ProjectIndex) -> Result<String> {
        let owner = Descriptor::internal_name_of(owner, index);
        let info = index.member(member);
        let name = if info.kind == DefKind::Constructor {
            "<init>".to_owned()
        } else {
            info.name.clone()
        };
        let descriptor =
            Descriptor::method_descriptor(member, index, info.kind == DefKind::Constructor)
                .map_err(|_| WasmError::NoRepresentation(Self::member_path(member, index)))?;
        Ok(alloc::format!("{owner}#{name}{descriptor}"))
    }

    /// Replay a library's declared groups verbatim and record where its classes landed.
    ///
    /// The group boundaries are reproduced exactly, because they are the identity: two modules
    /// share a type only when both declared the same *group*, so a consumer that merged the
    /// library's types into a group of its own would have the same declarations and still link
    /// against nothing. The consumer's own types go after a boundary this leaves behind.
    fn replay_library(
        library: &LinkedLibrary<'_>,
        index: &ProjectIndex,
        module: &mut Module,
        layout: &mut Layout,
    ) -> Result<()> {
        let base = u32::try_from(module.types().len()).map_err(|_| WasmError::TooLarge)?;
        module.begin_group();
        let mut next = 1usize;
        for (local, ty) in library.abi.types.iter().enumerate() {
            if library.abi.groups.get(next) == Some(&local) {
                module.begin_group();
                next += 1;
            }
            module.add_type(Self::rebase(ty, base));
            // An array type the library declares is this module's declaration of that array type
            // too. wasm array types are *invariant*, so a `char[]` built here and passed to a
            // library method has to be the very type that method's signature names: a structurally
            // equal declaration in the project's own group is a different heap type, and passing it
            // is a module the validator refuses. Recording it here is what makes `new char[n]` in
            // the project produce a value the library accepts.
            if let CompType::Array(element) = &ty.comp
                && let StorageType::Val(value) = element.storage
                && !layout
                    .arrays
                    .iter()
                    .any(|&(candidate, _)| candidate == value)
            {
                let index = base.saturating_add(u32::try_from(local).unwrap_or(u32::MAX));
                layout.arrays.push((value, index));
            }
        }
        module.begin_group();
        for class in &library.abi.classes {
            let Some(item) = index.item_by_fqn(&Self::fqn_of_internal(&class.name)) else {
                continue;
            };
            layout
                .structs
                .insert(item, base.saturating_add(class.index));
            layout
                .external_classes
                .insert(item, library.name.to_owned());
            // The struct's field list is kept beside it, as the prefix a project subclass has to
            // extend: its own fields have to start after exactly this many slots, and the list is
            // the library's own answer — synthetic entries included — rather than a re-derivation
            // from the declarations. See [`Slot::External`].
            let type_index =
                usize::try_from(base.saturating_add(class.index)).unwrap_or(usize::MAX);
            if let Some(CompType::Struct(fields)) =
                module.types().get(type_index).map(|ty| &ty.comp)
            {
                layout
                    .fields
                    .insert(item, fields.iter().copied().map(Slot::External).collect());
            }
            // The enclosing link is replayed too: an inner class the project constructs is reached
            // as `outer.new Inner()`, and the factory's first parameter is the enclosing instance.
            if let Some(outer) = class
                .enclosing
                .as_deref()
                .and_then(|name| index.item_by_fqn(&Self::fqn_of_internal(name)))
            {
                layout.inner.insert(item, outer);
            }
        }
        // An interface the library declares is an interface here too: its values are held as
        // `anyref`, and a call through it dispatches over the replayed classes that implement it.
        // Without this the consumer reports the type as one it cannot represent — there is no
        // struct to find, and the name is what says there should not be one.
        for name in &library.abi.interfaces {
            let Some(item) = index.item_by_fqn(&Self::fqn_of_internal(name)) else {
                continue;
            };
            layout.interfaces.insert(item);
        }
        Ok(())
    }

    /// A declared type with every concrete index shifted by the replay's base.
    fn rebase(ty: &SubType, base: u32) -> SubType {
        let comp = match &ty.comp {
            CompType::Func { params, results } => CompType::Func {
                params: params
                    .iter()
                    .copied()
                    .map(|ty| Self::rebase_val(ty, base))
                    .collect(),
                results: results
                    .iter()
                    .copied()
                    .map(|ty| Self::rebase_val(ty, base))
                    .collect(),
            },
            CompType::Struct(fields) => CompType::Struct(
                fields
                    .iter()
                    .map(|field| Self::rebase_field(field, base))
                    .collect(),
            ),
            CompType::Array(element) => CompType::Array(Self::rebase_field(element, base)),
        };
        SubType {
            is_final: ty.is_final,
            supertype: ty.supertype.map(|supertype| supertype.saturating_add(base)),
            comp,
        }
    }

    const fn rebase_field(field: &FieldType, base: u32) -> FieldType {
        let StorageType::Val(value) = field.storage;
        FieldType {
            storage: StorageType::Val(Self::rebase_val(value, base)),
            mutable: field.mutable,
        }
    }

    const fn rebase_val(ty: ValType, base: u32) -> ValType {
        match ty {
            ValType::Ref(reference) => ValType::Ref(RefType {
                nullable: reference.nullable,
                heap: match reference.heap {
                    HeapType::Concrete(index) => HeapType::Concrete(index.saturating_add(base)),
                    heap => heap,
                },
            }),
            primitive => primitive,
        }
    }

    /// `demo/Outer$Inner` as the index spells a fully-qualified name: `demo.Outer.Inner`.
    fn fqn_of_internal(name: &str) -> String {
        name.replace(['/', '$'], ".")
    }

    /// Declare an import for every member a linked library exports and the project can name.
    ///
    /// The whole surface is imported in one sweep, because imports occupy the function index space
    /// *before* every defined function: discovering a call while lowering a body would be too late
    /// to give it an index. What the library does not export — a private member, an abstract
    /// method, a `native` declaration only its own embedder implements — has no import, and the
    /// call site that names it fails where it is written.
    fn collect_linked_imports(
        libraries: &[LinkedLibrary<'_>],
        index: &ProjectIndex,
        layout: &mut Layout,
        module: &mut Module,
    ) -> Result<()> {
        let mut base = 0u32;
        for library in libraries {
            // Classes and interfaces alike. An interface's `default` and `static` methods and its
            // implicitly-`static final` fields are exports exactly as a class's are; an abstract
            // method has no export at all, so the lookup below finds nothing for it and the call
            // site dispatches over the replayed classes that implement it instead.
            let names = library
                .abi
                .classes
                .iter()
                .map(|class| class.name.as_str())
                .chain(library.abi.interfaces.iter().map(String::as_str));
            for name in names {
                let Some(item) = index.item_by_fqn(&Self::fqn_of_internal(name)) else {
                    continue;
                };
                for &member in index.own_members(item) {
                    let info = index.member(member);
                    if info.modifiers.is_private {
                        continue;
                    }
                    match info.kind {
                        DefKind::Method => {
                            let key = Self::member_key(item, member, index)?;
                            let Some(local) = library.abi.export_type(&key) else {
                                continue;
                            };
                            let function = module.add_shared_import(
                                library.name.to_owned(),
                                key,
                                local.saturating_add(base),
                            );
                            layout.functions.insert(member, function);
                        }
                        DefKind::Constructor => {
                            let key = Self::member_key(item, member, index)?;
                            let Some(local) = library.abi.export_type(&key) else {
                                continue;
                            };
                            let function = module.add_shared_import(
                                library.name.to_owned(),
                                key.clone(),
                                local.saturating_add(base),
                            );
                            layout.external_constructors.insert(member, function);
                            // The body beside the factory, for the `super(…)` that has an object
                            // already. A library compiled before this carried no such export; the
                            // consumer then simply has no initializer to call, which is the same
                            // state the refusal above it describes.
                            let init_key = Self::initializer_key(&key);
                            let Some(local) = library.abi.export_type(&init_key) else {
                                continue;
                            };
                            let function = module.add_shared_import(
                                library.name.to_owned(),
                                init_key,
                                local.saturating_add(base),
                            );
                            layout.external_initializers.insert(member, function);
                        }
                        DefKind::Field if info.modifiers.is_static => {
                            let owner = Descriptor::internal_name_of(item, index);
                            let get_key = alloc::format!("{owner}#{}#get", info.name);
                            let put_key = alloc::format!("{owner}#{}#put", info.name);
                            let (Some(get), Some(put)) = (
                                library.abi.export_type(&get_key),
                                library.abi.export_type(&put_key),
                            ) else {
                                continue;
                            };
                            let get_index = module.add_shared_import(
                                library.name.to_owned(),
                                get_key,
                                get.saturating_add(base),
                            );
                            let put_index = module.add_shared_import(
                                library.name.to_owned(),
                                put_key,
                                put.saturating_add(base),
                            );
                            layout
                                .external_statics
                                .insert(member, (get_index, put_index));
                        }
                        _ => {}
                    }
                }
            }
            // The one export that is not a member key: the tag every `throw` in the library names.
            // A consumer's `try_table` catches a tag *instance*, so it has to be this one — a tag
            // of its own with the same payload would never see the throw. The import's type is the
            // replayed payload, which is exactly why the two modules declared matching groups.
            if let Some(local) = library.abi.export_type("$jals$tag") {
                // One tag is all this lowering has: `layout.tag` is a single index and every
                // `throw`/`try_table` reads it. A second library's tag would be a second instance,
                // and silently not catching it is the failure mode this import exists to remove.
                if layout.tag.is_some() {
                    return Err(WasmError::Unsupported(
                        "more than one linked library exports an exception tag",
                    ));
                }
                let tag = module.add_tag_import(
                    library.name.to_owned(),
                    "$jals$tag".to_owned(),
                    local.saturating_add(base),
                );
                layout.tag = Some(tag);
            }
            // The dispatch realm, when the library published one: `$jals$link` becomes an import,
            // and the slots become the fields of the struct this module builds and installs in its
            // start function. Each slot's member is found here — an abstract method has no export
            // and is found only this way.
            if let Some(realm) = &library.abi.realm {
                let link_type = realm.link.saturating_add(base);
                let link = module.add_shared_import(
                    library.name.to_owned(),
                    "$jals$link".to_owned(),
                    link_type,
                );
                let slots = realm
                    .slots
                    .iter()
                    .map(|slot| RealmSlotImport {
                        member: Self::slot_member(&slot.name, index),
                        ty: slot.type_index.saturating_add(base),
                    })
                    .collect();
                layout.realms.push(RealmImport {
                    link,
                    structure: realm.structure.saturating_add(base),
                    slots,
                });
            }
            base = base.saturating_add(u32::try_from(library.abi.types.len()).unwrap_or(u32::MAX));
        }
        Ok(())
    }

    /// The member a realm slot answers, found by the key the library published.
    ///
    /// The key is a [`member_key`](Self::member_key) exactly — `owner#name+descriptor` — so the
    /// owner's item is one lookup and the member is the one of its declarations whose own key
    /// matches. The search walks declarations rather than the import list because an abstract
    /// method has no export: a slot for `Throwable.className`, which the library's `toString`
    /// dispatches to, is nothing but a declaration to go on.
    fn slot_member(key: &str, index: &ProjectIndex) -> Option<MemberId> {
        let (owner, _) = key.split_once('#')?;
        let item = index.item_by_fqn(&Self::fqn_of_internal(owner))?;
        index
            .own_members(item)
            .iter()
            .copied()
            // Methods only: a field's key is built from its type the same way a method's is from
            // its return type, so `ArrayList`'s `size` field and `size()` method *share* a key — and
            // the field comes first. A realm slot is always a method: nothing else is dispatched.
            .find(|&member| {
                index.member(member).kind == DefKind::Method
                    && Self::member_key(item, member, index).ok().as_deref() == Some(key)
            })
    }

    /// Fill and install every linked library's dispatch realm, then run the class initialisers.
    ///
    /// The consumer is the module that *knows* both halves: the replayed library classes and the
    /// project's own. One thunk per slot answers for both — the same `ref.test` chain a virtual
    /// call uses, over this module's structs and most-derived first, with the library's own
    /// implementation as the fallthrough — and the start function builds one struct per library and
    /// installs it before any initialiser runs, so a library call a static initialiser makes sees
    /// the same dispatch a later call does.
    ///
    /// Nothing here when no library published a realm: the module is the module it was.
    fn link_realms(index: &ProjectIndex, layout: &Layout, module: &mut Module) -> Result<()> {
        if layout.realms.is_empty() {
            return Ok(());
        }
        let old_start = module.start;
        let mut thunks: Vec<Vec<u32>> = Vec::new();
        for realm in &layout.realms {
            let mut indices = Vec::new();
            for slot in &realm.slots {
                match slot.member {
                    Some(member) => {
                        indices.push(Self::dispatch_thunk(
                            index, layout, module, member, slot.ty,
                        )?);
                    }
                    None => indices.push(Self::trap_thunk(module, slot.ty)?),
                }
            }
            thunks.push(indices);
        }
        // A start function takes nothing and returns nothing: a type of its own, because the
        // replayed groups have no `[] -> []` to borrow and the function is called by the engine
        // rather than from any body.
        let signature = module.add_type(SubType::plain(CompType::Func {
            params: Vec::new(),
            results: Vec::new(),
        }));
        let mut insn = Insn::new();
        for (realm, indices) in layout.realms.iter().zip(&thunks) {
            for &thunk in indices {
                insn.ref_func(thunk);
            }
            insn.struct_new(realm.structure).call(realm.link);
        }
        if let Some(start) = old_start {
            insn.call(start);
        }
        let link = module.func_index(module.funcs.len());
        Self::push_func(
            module,
            Func {
                type_index: signature,
                locals: Vec::new(),
                body: insn.into_body(),
            },
        )?;
        module.start = Some(link);
        Ok(())
    }

    /// One field of a linked library's realm: a function that answers the library's virtual call
    /// with this module's classes first and the library's own imported implementation last.
    ///
    /// The chain is the one a virtual call emits, over *this* module's structs — every project
    /// class and every replayed library class, most-derived first — and the fallthrough is the
    /// import the library exported under the member's key. An arm narrows each argument to its own
    /// function's parameters, exactly as an in-module arm does: the plan was made over the
    /// *declared* method, and an override is free to declare narrower ones.
    fn dispatch_thunk(
        index: &ProjectIndex,
        layout: &Layout,
        module: &mut Module,
        member: MemberId,
        ty: u32,
    ) -> Result<u32> {
        let Some(SubType {
            comp: CompType::Func { params, results },
            ..
        }) = module
            .types()
            .get(usize::try_from(ty).map_err(|_| WasmError::TooLarge)?)
        else {
            return Err(WasmError::Unsupported(
                "a realm slot whose type is no function",
            ));
        };
        let params = params.clone();
        let result = results.first().copied();
        let mut insn = Insn::new();
        match result {
            Some(result) => insn.block_typed(result),
            None => insn.block(),
        };
        let leave = insn.depth();
        for (item, over) in layout.overriders(index, member) {
            let (Some(&function), Some(&struct_type)) =
                (layout.functions.get(&over), layout.structs.get(&item))
            else {
                continue;
            };
            insn.local_get(0);
            insn.ref_test(HeapType::Concrete(struct_type), false);
            insn.if_();
            // The receiver was cast to the *tested* class, which is a subtype of whatever class
            // declares the arm's function, so the call's own receiver needs no narrowing.
            insn.local_get(0);
            insn.ref_cast(HeapType::Concrete(struct_type), false);
            let over_tys = index.resolved_param_tys(over);
            for (position, &held) in params.get(1..).unwrap_or_default().iter().enumerate() {
                let local = 1 + u32::try_from(position).map_err(|_| WasmError::TooLarge)?;
                insn.local_get(local);
                if let Some(ty) = over_tys.get(position) {
                    let want = layout.val_type(ty)?;
                    Self::narrow_slot(held, want, &mut insn)?;
                }
            }
            insn.call(function);
            insn.br(insn.depth() - leave);
            insn.end();
        }
        match layout.functions.get(&member) {
            Some(&function) => {
                insn.local_get(0);
                let owner = layout.class_ref(index.member(member).owner)?;
                Self::narrow_slot(params.first().copied().unwrap_or(owner), owner, &mut insn)?;
                for position in 0..params.len().saturating_sub(1) {
                    insn.local_get(1 + u32::try_from(position).map_err(|_| WasmError::TooLarge)?);
                }
                insn.call(function);
            }
            // The method is abstract, or the library exported nothing for it: every class that
            // could satisfy the call is already in the chain above, so reaching here means the
            // receiver is of a type nothing implemented — a trap, the same answer the library's own
            // no-function fallthrough gives.
            None => {
                insn.unreachable();
            }
        }
        insn.end();
        let thunk = module.func_index(module.funcs.len());
        Self::push_func(
            module,
            Func {
                type_index: ty,
                locals: Vec::new(),
                body: insn.into_body(),
            },
        )?;
        Ok(thunk)
    }

    /// A realm field this module has nothing to answer with: a function that traps, at the slot's
    /// own type.
    ///
    /// A slot whose member this module cannot represent at all is the case — a type in its
    /// declaration that has no representation here, which is the same refusal the corresponding
    /// call site gives. The library only calls a slot it dispatched to, and this module has no such
    /// call: the trap is unreachable in exactly the way a no-function fallthrough is.
    fn trap_thunk(module: &mut Module, ty: u32) -> Result<u32> {
        let thunk = module.func_index(module.funcs.len());
        Self::push_func(
            module,
            Func {
                type_index: ty,
                locals: Vec::new(),
                body: {
                    let mut body = Insn::new();
                    body.unreachable();
                    body.into_body()
                },
            },
        )?;
        Ok(thunk)
    }

    /// Bring a dispatch thunk's value from the type the slot holds down to the type an arm's
    /// function wants.
    ///
    /// Only reference narrowing can appear here: both types are the *erased* ones a call site uses,
    /// so a primitive mismatch is a descriptor mismatch that cannot exist, and a parameter an
    /// override narrows is a reference — a type variable's `anyref` down to the class it was
    /// instantiated at, which is the everyday shape. Equality is the common case and costs nothing;
    /// anything else is a bug worth a refusal rather than a module the validator rejects with
    /// nothing said on this side.
    fn narrow_slot(from: ValType, to: ValType, insn: &mut Insn) -> Result<()> {
        if from == to {
            return Ok(());
        }
        match (from, to) {
            (
                ValType::Ref(RefType {
                    heap: HeapType::Any,
                    ..
                }),
                ValType::Ref(target),
            ) => {
                insn.ref_cast(target.heap, target.nullable);
                Ok(())
            }
            _ => Err(WasmError::Unsupported(
                "a dispatch arm whose parameter types do not narrow",
            )),
        }
    }

    /// Give every class with static state a function index and a "has run" flag.
    ///
    /// Reserved before any body is lowered, because a body calls one: a `static` field read has to
    /// initialise the class that declares it first, and that class may be declared *after* the one
    /// reading it. The flag is what makes calling it again free, and what makes the re-entrant call a
    /// class's own initialiser produces a no-op rather than a loop — the same answer §12.4.2 gives.
    fn reserve_class_inits(
        inputs: &[TypedFile<'_>],
        index: &ProjectIndex,
        layout: &mut Layout,
        module: &mut Module,
        state: &StaticState<'_>,
        mut next: usize,
    ) {
        let StaticState {
            deferred,
            constants,
            blocks,
        } = *state;
        // Source order, so a module with no cross-class dependency runs exactly what it used to.
        let mut owners = Vec::new();
        for (position, input) in inputs.iter().enumerate() {
            let mut push = |owner: ItemId| {
                if !owners.contains(&owner) {
                    owners.push(owner);
                }
            };
            for node in input.root().descendants() {
                if !Self::declares_a_type(&node) {
                    continue;
                }
                let Ok(owner) = Layout::owner_of(&node, input, index) else {
                    continue;
                };
                let has_constants = constants.iter().any(|&(_, item, _)| item == owner);
                let has_deferred = deferred
                    .iter()
                    .any(|(member, _)| index.member(*member).owner == owner);
                let has_blocks = blocks
                    .iter()
                    .any(|&(at, item, _)| at == position && item == owner);
                if has_constants || has_deferred || has_blocks {
                    push(owner);
                }
            }
        }
        for owner in owners {
            let mut init = Insn::new();
            init.i32_const(0);
            module.globals.push(Global {
                ty: ValType::I32,
                init: init.into_body(),
            });
            let flag = u32::try_from(module.globals.len() - 1).unwrap_or(0);
            layout
                .class_inits
                .insert(owner, (module.func_index(next), flag));
            next += 1;
        }
    }

    /// One function per class with static state: its `enum` constants, then its computed `static`
    /// field initialisers and `static { … }` blocks, in source order (§8.9.3, §12.4.2).
    fn class_initializers(
        inputs: &[TypedFile<'_>],
        index: &ProjectIndex,
        layout: &Layout,
        state: &StaticState<'_>,
        module: &mut Module,
        project_inputs: usize,
    ) -> Result<Vec<Func>> {
        let StaticState {
            deferred,
            constants,
            blocks,
        } = *state;
        let signature = module.add_type(SubType::plain(CompType::Func {
            params: Vec::new(),
            results: Vec::new(),
        }));
        let mut out = Vec::new();
        for (&owner, &(_, flag)) in &layout.class_inits {
            let mut insn = Insn::new();
            // The guard, and the flag set *before* the body: a class whose initialiser reaches back to
            // its own statics gets the values written so far rather than a second run.
            insn.block();
            insn.global_get(flag);
            insn.br_if(0);
            insn.i32_const(1);
            insn.global_set(flag);
            let mut locals = Vec::new();
            for (position, input) in inputs.iter().enumerate() {
                let mut lowering =
                    Lowering::for_static(input, index, layout, locals, position < project_inputs);
                // Every constant first: a `static { … }` block or a field initialiser may name one, and
                // §8.9.3 builds them before either runs.
                for (member, item, node) in constants {
                    if *item != owner || index.member(*member).file != input.file() {
                        continue;
                    }
                    let global = *layout
                        .statics
                        .get(member)
                        .ok_or(WasmError::Unsupported("an `enum` constant with no global"))?;
                    lowering.enum_constant(*item, node, &mut insn)?;
                    insn.global_set(global);
                }
                // A field initialiser and a `static { … }` block are one sequence in *source* order
                // (§12.4.2), not two: `static int a = 1; static { a = 2; } static int b = a;` leaves `b`
                // as 2, and running every field before every block left it as 1.
                let mut sequence: Vec<(usize, StaticStep<'_>)> = Vec::new();
                for entry in deferred {
                    let info = index.member(entry.0);
                    if info.owner == owner && info.file == input.file() {
                        sequence.push((info.name_range.start, StaticStep::Field(entry)));
                    }
                }
                for (at, item, block) in blocks {
                    if *at == position && *item == owner {
                        sequence.push((
                            usize::from(block.syntax().text_range().start()),
                            StaticStep::Block(block),
                        ));
                    }
                }
                sequence.sort_by_key(|&(at, _)| at);
                for (_, step) in sequence {
                    match step {
                        StaticStep::Field((member, value)) => {
                            let global = *layout
                                .statics
                                .get(member)
                                .ok_or(WasmError::Unsupported("a `static` field with no global"))?;
                            let declared = index.resolved_member_ty(*member);
                            lowering.assign_static(value, &declared, global, &mut insn)?;
                        }
                        StaticStep::Block(block) => lowering.block(block, &mut insn)?,
                    }
                }
                locals = lowering.locals;
            }
            insn.end();
            out.push(Func {
                type_index: signature,
                locals,
                body: insn.into_body(),
            });
        }
        Ok(out)
    }

    /// A record's canonical constructor and one accessor per component, written out rather than walked.
    ///
    /// A component is declared once, in the header, and stands for a field, an accessor, and a
    /// constructor parameter — none of which the body writes. The index already synthesises all three
    /// (that is what makes `r.x()` resolve), so what is missing here is only the code, and it is short
    /// enough to write directly: the constructor stores each parameter into its slot, and an accessor
    /// reads one back.
    ///
    /// `equals`, `hashCode`, and `toString` are *not* synthesised. All three come from
    /// `java.lang.Record` and two of them involve a `String`, which has no wasm representation by this
    /// backend's design — a call to one reports rather than being guessed at.
    fn record_members(
        inputs: &[TypedFile<'_>],
        index: &ProjectIndex,
        layout: &mut Layout,
        module: &mut Module,
        methods: &[Method],
    ) -> Result<Vec<Func>> {
        let mut out = Vec::new();
        for input in inputs {
            for node in input.root().descendants() {
                if node.kind() != RECORD_DECL {
                    continue;
                }
                let owner = Layout::owner_of(&node, input, index)?;
                let struct_type = layout.structs[&owner];
                let this = layout.class_ref(owner)?;
                let components: Vec<MemberId> = layout
                    .fields
                    .get(&owner)
                    .into_iter()
                    .flatten()
                    .filter_map(|slot| match slot {
                        Slot::Declared(member) => Some(*member),
                        // A library's replayed fields and the synthetic ones carry no Java member
                        // this module can name, so a record's components — the ones it declared —
                        // are exactly the declared slots.
                        Slot::External(_) | Slot::Enclosing(_) | Slot::Capture(_) => None,
                    })
                    .collect();

                // The canonical constructor, unless the body wrote one: `this` then one parameter per
                // component, each stored into its own slot.
                let declared_constructor = index.own_members(owner).iter().any(|&id| {
                    let m = index.member(id);
                    m.kind == DefKind::Constructor && m.name_range != (0..0)
                });
                if !declared_constructor
                    && let Some(&ctor) = index
                        .own_members(owner)
                        .iter()
                        .find(|&&id| index.member(id).kind == DefKind::Constructor)
                {
                    let mut params = alloc::vec![this];
                    let mut body = Insn::new();
                    for (position, &component) in components.iter().enumerate() {
                        let ty = layout.val_type(&index.resolved_member_ty(component))?;
                        params.push(ty);
                        let slot = u32::try_from(position + 1).map_err(|_| WasmError::TooLarge)?;
                        let field = layout
                            .field_slot(owner, component)
                            .ok_or(WasmError::Unsupported("a record component with no slot"))?;
                        body.local_get(0)
                            .local_get(slot)
                            .struct_set(struct_type, field);
                    }
                    let signature = module.add_type(SubType::plain(CompType::Func {
                        params,
                        results: Vec::new(),
                    }));
                    let function = module.func_index(methods.len() + out.len());
                    layout.member_types.insert(ctor, signature);
                    layout.functions.insert(ctor, function);
                    out.push(Func {
                        type_index: signature,
                        locals: Vec::new(),
                        body: body.into_body(),
                    });
                }

                // One accessor per component, unless the body declared it by hand.
                for &component in &components {
                    let name = index.member(component).name.clone();
                    let accessor = index.own_members(owner).iter().copied().find(|&id| {
                        let m = index.member(id);
                        m.kind == DefKind::Method && m.name == name && m.params.is_empty()
                    });
                    let Some(accessor) = accessor else { continue };
                    if layout.functions.contains_key(&accessor) {
                        continue;
                    }
                    let ty = layout.val_type(&index.resolved_member_ty(component))?;
                    let field = layout
                        .field_slot(owner, component)
                        .ok_or(WasmError::Unsupported("a record component with no slot"))?;
                    let mut body = Insn::new();
                    body.local_get(0).struct_get(struct_type, field);
                    let signature = module.add_type(SubType::plain(CompType::Func {
                        params: alloc::vec![this],
                        results: alloc::vec![ty],
                    }));
                    let function = module.func_index(methods.len() + out.len());
                    layout.member_types.insert(accessor, signature);
                    layout.functions.insert(accessor, function);
                    out.push(Func {
                        type_index: signature,
                        locals: Vec::new(),
                        body: body.into_body(),
                    });
                }
            }
        }
        Ok(out)
    }

    /// Every `static { … }` block in `root`, in source order, with the type that declares it.
    ///
    /// The owner is what groups a block with the field initialisers it runs beside: JLS §12.4.2 runs
    /// one class's static initialisers as one sequence, and a block that reads a field of *another*
    /// class is what makes the grouping observable.
    fn static_initializers(
        root: &SyntaxNode,
        input: &TypedFile<'_>,
        index: &ProjectIndex,
    ) -> Vec<(ItemId, ast::Block)> {
        let mut out = Vec::new();
        for node in root.descendants() {
            if node.kind() != INITIALIZER
                || !Facts::has_modifier(&node, jals_syntax::SyntaxKind::STATIC_KW)
            {
                continue;
            }
            let owner = node
                .ancestors()
                .find(Self::declares_a_type)
                .and_then(|declaration| ast::Decl::name_token_of(&declaration))
                .and_then(|name| {
                    index.item_by_decl(input.file(), usize::from(name.text_range().start()))
                });
            if let (Some(owner), Some(block)) = (owner, node.children().find_map(ast::Block::cast))
            {
                out.push((owner, block));
            }
        }
        out
    }

    /// Whether a node is a type declaration, which is what a member's owner is found by walking to.
    fn declares_a_type(node: &SyntaxNode) -> bool {
        matches!(
            node.kind(),
            CLASS_DECL | INTERFACE_DECL | ENUM_DECL | RECORD_DECL | ANNOTATION_TYPE_DECL
        )
    }

    /// Every project class, supertypes first.
    ///
    /// A struct's fields start with its supertype's, so the supertype's layout has to be settled
    /// first. The order is a depth-first walk of the `extends` chain; a cycle is impossible in a
    /// well-formed program and is simply not revisited here.
    fn classes_in_order(
        inputs: &[TypedFile<'_>],
        index: &ProjectIndex,
        interfaces: &mut Vec<ItemId>,
        inner: &mut Vec<(ItemId, ItemId)>,
        captures: &mut Vec<(ItemId, Vec<(DefId, Ty)>)>,
    ) -> Result<Vec<ItemId>> {
        let mut declared = Vec::new();
        for input in inputs {
            for node in Self::type_declarations(input.root()) {
                let Some(item) = Self::item_of(&node, input, index)? else {
                    continue;
                };
                // An `@interface` **is** an interface (JLS §9.6) and is laid out as one: its
                // elements are abstract methods, so nothing declares a function for them, and its
                // uses are metadata a wasm host has no reflection to read. Refusing the declaration
                // instead stopped every file that merely *declared* one — a sixth of this backend's
                // own corpus — over a type nothing in the module ever calls.
                if matches!(node.kind(), INTERFACE_DECL | ANNOTATION_TYPE_DECL) {
                    interfaces.push(item);
                    continue;
                }
                // The set is the shared fact; the type beside each is this backend's, because the
                // layout wants a `ValType` where the JVM wants a descriptor. It is read from the
                // *declaring* input, which is the only place it is known — the layout is built long
                // before any body is lowered.
                let captured: Vec<(DefId, Ty)> = Facts::of(*input)
                    .captured_by(&node)
                    .into_iter()
                    .map(|id| (id, input.type_of_def(id).clone()))
                    .collect();
                if !captured.is_empty() {
                    captures.push((item, captured));
                }
                // All three arms, not just the nested one. A local class and an anonymous body in
                // an instance context hold an enclosing instance too, and asking `is_inner_class`
                // — which is arm one alone — is what left them without one: an uplevel field read
                // reported the name as unresolved, and an uplevel *call* pushed local 0 without
                // checking whose `this` it was and produced a module the validator rejects.
                //
                // The enclosing *type* is the nearest one, found by the shared walk. The two-hop
                // `parent().parent()` this replaces lands on a type declaration only when the class
                // sits directly in a `CLASS_BODY`, and `Layout::owner_of` then demands a name token
                // — neither of which a local or anonymous class has.
                if Facts::holds_enclosing_instance(&node, item, index) {
                    inner.push((item, Facts::enclosing_type_of(&node, input.file(), index)?));
                }
                declared.push(item);
            }
        }

        let mut ordered: Vec<ItemId> = Vec::with_capacity(declared.len());
        for &item in &declared {
            Self::push_with_supertypes(item, index, &declared, &mut ordered);
        }
        Ok(ordered)
    }

    /// Every type declaration in `root`, nested ones included.
    ///
    /// wasm's type space is flat and has no naming convention to satisfy, so a `static` nested class is
    /// simply another struct type — there is nothing for it to be nested *in*. Walking only the root's
    /// children dropped every one of them silently: the type never existed, and a call to one of its
    /// methods reported an unresolved name that pointed nowhere useful.
    ///
    /// A class inside a *block* is a local class, and wasm's flat type space has nothing to say about
    /// where it was written — so it is laid out like any other. What it may *not* do is capture a local:
    /// each capture becomes a synthetic field and a trailing constructor parameter, which is how a
    /// class outlives the frame the local lived in.
    fn type_declarations(root: &SyntaxNode) -> impl Iterator<Item = SyntaxNode> + '_ {
        use jals_syntax::SyntaxKind::{
            ANNOTATION_TYPE_DECL, ENUM_DECL, INTERFACE_DECL, RECORD_DECL,
        };
        root.descendants().filter(|node| {
            matches!(
                node.kind(),
                CLASS_DECL | INTERFACE_DECL | ENUM_DECL | RECORD_DECL | ANNOTATION_TYPE_DECL
            ) || Facts::is_anonymous_body(node)
                || Self::is_functional(node)
        })
    }

    /// Whether `node` is a lambda or a method reference — the two forms the index gives a one-method class
    /// item to, and which a backend with no `invokedynamic` emits as exactly that.
    fn is_functional(node: &SyntaxNode) -> bool {
        matches!(node.kind(), LAMBDA_EXPR | METHOD_REF_EXPR)
    }

    /// The item a type declaration declares, whether it has a name to look up or only a position.
    fn item_of(
        node: &SyntaxNode,
        input: &TypedFile<'_>,
        index: &ProjectIndex,
    ) -> Result<Option<ItemId>> {
        // A lambda and an anonymous body are both nameless, and the index keys each on its own start
        // offset — the only thing either has to be found by.
        if Facts::is_anonymous_body(node) || Self::is_functional(node) {
            return Ok(index.item_by_decl(input.file(), usize::from(node.text_range().start())));
        }
        let name =
            ast::Decl::name_token_of(node).ok_or(WasmError::Unsupported("a class with no name"))?;
        index
            .item_by_decl(input.file(), usize::from(name.text_range().start()))
            .ok_or_else(|| WasmError::Unresolved(name.text().into()))
            .map(Some)
    }

    /// Append `item` to `ordered`, its declared supertypes first.
    ///
    /// Walked rather than recursed. The recursion this replaces guarded on `ordered`, which a
    /// caller only appends to on the way *back out* — so an ancestor still being visited was
    /// invisible to it, and `class A extends B {}` beside `class B extends A {}` recursed until the
    /// stack ran out. That is an abort rather than a panic: nothing catches it, and the input
    /// parses and indexes perfectly, so it arrived through an editor as readily as through a build.
    ///
    /// The chain comes from [`ProjectIndex::superclasses`], which carries the cycle guard, and ends
    /// at the first type outside `declared`. The cutoff has always applied to the *parent* and never
    /// to `item` itself — leading with `item` is what makes that visible, where a `.filter()` on the
    /// step hid it.
    fn push_with_supertypes(
        item: ItemId,
        index: &ProjectIndex,
        declared: &[ItemId],
        ordered: &mut Vec<ItemId>,
    ) {
        if ordered.contains(&item) {
            return;
        }
        let chain: Vec<ItemId> = core::iter::once(item)
            .chain(
                index
                    .superclasses(item)
                    .take_while(|id| declared.contains(id)),
            )
            .collect();
        for &id in chain.iter().rev() {
            if !ordered.contains(&id) {
                ordered.push(id);
            }
        }
    }

    /// Declare a host import for every `native` method `input` declares.
    ///
    /// Java has had a word for "the body is not in this class file" since 1.0, and on this target
    /// it means an import: the module says what it needs, the embedder supplies it, and the
    /// engine refuses to instantiate a module whose needs are unmet. Nothing else in the lowering
    /// changes — the member lands in `layout.functions` exactly as a defined method does, so a
    /// call site emits the same `call` and never learns which kind of function it reached.
    ///
    /// # The two names
    ///
    /// The import's module name is the declaring class's **internal name** and its field name is
    /// the method's **name with its JVM descriptor**. Both are derived here from the declaration
    /// alone, which is what lets the host key its implementation table on the same two strings
    /// without either side restating the other's type mapping. A host that spells the descriptor
    /// differently therefore produces an import nothing satisfies — reported when the module is
    /// instantiated, with both spellings in hand — rather than a type mismatch somebody has to
    /// notice.
    ///
    /// The descriptor is read through [`Descriptor`](crate::desc::Descriptor), which is otherwise
    /// the JVM backend's. That is deliberate: the link symbol is the canonical spelling of a
    /// *Java signature*, the JVM's descriptor grammar is what that spelling is, and this backend
    /// writing its own erasure would be the second copy of a rule — the regression
    /// `no-wasm-into-jvm-lowering`'s note describes, arriving through the door that rule does not
    /// cover.
    ///
    /// # Why it is a sweep of its own
    ///
    /// Imports occupy the function index space *before* every defined function. So every one of
    /// them has to be declared before [`Module::func_index`] is asked for anything, and a single
    /// pass that declared imports and defined methods as it met them would hand out indices an
    /// import declared later then took.
    fn collect_imports(
        input: &TypedFile<'_>,
        index: &ProjectIndex,
        layout: &mut Layout,
        module: &mut Module,
    ) -> Result<()> {
        for class in Self::type_declarations(input.root()) {
            let Some(item) = Self::item_of(&class, input, index)? else {
                continue;
            };
            let Some(body) = class
                .children()
                .find(|child| matches!(child.kind(), CLASS_BODY | ENUM_BODY))
            else {
                continue;
            };
            for node in body.children() {
                if node.kind() != METHOD_DECL
                    || !Facts::has_modifier(&node, jals_syntax::SyntaxKind::NATIVE_KW)
                {
                    continue;
                }
                // A `native` method with a body is not a method this backend has to import — the
                // body is right there, and `collect_methods` will give it a function. Java rejects
                // the combination, but this backend never checks, so the honest reading of a
                // declaration that has both is "there is a body".
                if node.children().find_map(ast::Block::cast).is_some() {
                    continue;
                }
                let member_name = Self::member_name_token(&node, false)
                    .ok_or(WasmError::Unsupported("a member with no name"))?;
                let member = Facts::of(*input).member_at(&member_name)?;
                let is_static = index.member(member).modifiers.is_static;

                let mut params = Vec::new();
                if !is_static {
                    params.push(layout.class_ref(item)?);
                }
                for ty in index.resolved_param_tys(member) {
                    layout.declare_array(&ty, module)?;
                    params.push(layout.val_type(&ty)?);
                }
                let returned = index.resolved_member_ty(member);
                layout.declare_array(&returned, module)?;
                let results = match returned {
                    Ty::Void => Vec::new(),
                    ty => alloc::vec![layout.val_type(&ty)?],
                };
                let descriptor = Descriptor::method_descriptor(member, index, false)
                    .map_err(|_| WasmError::NoRepresentation(Self::member_path(member, index)))?;
                let owner = Descriptor::internal_name_of(item, index);
                let name = alloc::format!("{}{descriptor}", index.member(member).name);
                let function = module.add_import(owner, name, params, results);
                layout.functions.insert(member, function);
            }
        }
        Ok(())
    }

    /// Give every distinct string literal in `input` a passive data segment.
    ///
    /// A string literal is not a value this target can write into an instruction: it is an object
    /// built at run time from a `char[]`, and the characters live in module *data*. So each distinct
    /// text gets one passive segment — UTF-16 code units, because that is what a Java `String` is a
    /// sequence of, each stored as the 4-byte `i32` a `char` is here — and the expression that names
    /// the literal copies it out with `array.new_data`.
    ///
    /// Collected before any body is lowered because a data segment is module state and a body may
    /// not add one: the module's sections are fixed by then, so the expression only reports which
    /// segment it reads.
    fn collect_literals(
        input: &TypedFile<'_>,
        layout: &mut Layout,
        module: &mut Module,
    ) -> Result<()> {
        for node in input.root().descendants() {
            let Some(literal) = ast::Literal::cast(node) else {
                continue;
            };
            let Some(token) = literal.token() else {
                continue;
            };
            if token.kind() != jals_syntax::SyntaxKind::STRING_LITERAL {
                continue;
            }
            let text = Literal::text(token.text())?;
            // One segment per distinct text: the same literal twice is one copy in the module, and
            // a table of repeated labels costs what the labels cost rather than what the uses do.
            if layout.literals.iter().any(|(value, ..)| *value == text) {
                continue;
            }
            let mut bytes = Vec::with_capacity(text.len() * 4);
            let mut units = 0u32;
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&i32::from(unit).to_le_bytes());
                units += 1;
            }
            let data = module.add_data(bytes);
            layout.literals.push((text, data, units));
        }
        // The array the literal is copied into. Idempotent, and the same type a `char[]` the source
        // writes by hand gets — which is what lets a `String` constructor accept either.
        if !layout.literals.is_empty() {
            layout.declare_array(&Ty::Array(Box::new(Ty::Primitive(Primitive::Char))), module)?;
        }
        Ok(())
    }

    /// `<owner fqn>.<member name>`, the way every diagnostic in this module names a method.
    fn member_path(member: MemberId, index: &ProjectIndex) -> String {
        let owner = index.item(index.member(member).owner).fqn.as_str();
        alloc::format!("{owner}.{}", index.member(member).name)
    }

    /// The declared function type of a method, receiver first, or `None` when a type it names has
    /// no representation in this module.
    ///
    /// The same shape [`collect_methods`](Self::collect_methods) gives a body-carrying
    /// declaration, for the declarations that have no body to give one: an abstract method, and
    /// every method of a **stub** — a type the index knows and this module does not lay out. A
    /// dispatch arm calls a consumer-provided function at exactly this type, and a method with no
    /// function index has none to read.
    ///
    /// A receiver this module lays out is the owner's struct; a stub's is `anyref`, which is where
    /// a value of a type nothing here declares can live. A miss is not a refusal: a method nothing
    /// can call is a method whose type nothing needs, so no type is made and the call site keeps
    /// its own error.
    fn declared_member_type(
        member: MemberId,
        index: &ProjectIndex,
        layout: &Layout,
        module: &mut Module,
    ) -> Option<u32> {
        let info = index.member(member);
        let mut params = Vec::new();
        if !info.modifiers.is_static {
            let any = ValType::Ref(RefType::nullable(HeapType::Any));
            params.push(layout.class_ref(info.owner).unwrap_or(any));
        }
        for ty in index.resolved_param_tys(member) {
            params.push(layout.val_type(&ty).ok()?);
        }
        let results = match index.resolved_member_ty(member) {
            Ty::Void => Vec::new(),
            ty => alloc::vec![layout.val_type(&ty).ok()?],
        };
        Some(module.add_type(SubType::plain(CompType::Func { params, results })))
    }

    /// Record a function type for every open method of a **stub** type this module's classes reach.
    ///
    /// A dispatch realm's arm calls at the method's declared type, and the type has to exist before
    /// the body that dispatches is lowered. A method declared in this module has its type from
    /// [`collect_methods`](Self::collect_methods); one inherited from a stub — `Object.equals`, an
    /// interface a stub declares and a library class implements — has no declaration here at all,
    /// so the type is made from the declaration the index holds, through
    /// [`declared_member_type`](Self::declared_member_type).
    ///
    /// Only *supertypes* of what this module lays out are swept: a stub type named only in a
    /// signature — a parameter nothing implements — is not, and a dispatched call through it stays
    /// the refusal it was, naming the library type it needs.
    fn collect_stub_types(
        classes: &[ItemId],
        interfaces: &[ItemId],
        compiled: &BTreeSet<ItemId>,
        index: &ProjectIndex,
        layout: &mut Layout,
        module: &mut Module,
    ) {
        let mut seen = BTreeSet::new();
        let mut pending: Vec<ItemId> = Vec::new();
        for &item in classes.iter().chain(interfaces.iter()) {
            pending.extend(index.superclasses(item));
            pending.extend(index.direct_interfaces(item));
        }
        while let Some(item) = pending.pop() {
            if !seen.insert(item) {
                continue;
            }
            pending.extend(index.direct_interfaces(item));
            if compiled.contains(&item) {
                pending.extend(index.superclasses(item));
                continue;
            }
            // Only the stub types a value can actually live at: `Object` and the interfaces. A
            // stub *class* this module does not lay out has no representation at a call site
            // either, so its methods are not ones a realm could be asked about.
            if Some(item) != layout.object && index.item(item).kind != DefKind::Interface {
                continue;
            }
            for &member in index.own_members(item) {
                let info = index.member(member);
                if info.kind != DefKind::Method
                    || info.modifiers.is_static
                    || info.modifiers.is_private
                    || layout.member_types.contains_key(&member)
                {
                    continue;
                }
                if let Some(signature) = Self::declared_member_type(member, index, layout, module) {
                    layout.member_types.insert(member, signature);
                }
            }
        }
    }

    /// Register every method and constructor `input` declares.
    fn collect_methods(
        input: &TypedFile<'_>,
        position: usize,
        exported: bool,
        index: &ProjectIndex,
        layout: &mut Layout,
        module: &mut Module,
        out: &mut Vec<Method>,
    ) -> Result<()> {
        for class in Self::type_declarations(input.root()) {
            let Some(item) = Self::item_of(&class, input, index)? else {
                continue;
            };
            // A lambda has no body *node* of members: it declares exactly one method, the interface's, and
            // the lambda expression itself is that method's body.
            if Self::is_functional(&class) {
                // A lambda whose item declares no method is one the index could not give a single
                // abstract method to — it is typed by its *target*, and in argument position that
                // target is the parameter of an overload chosen after the index is built. Skipping
                // it left the struct laid out below with no body behind it: the creation emitted
                // `struct.new_default`, the object implemented nothing, and the call through the
                // interface found no override and became `unreachable` — a module that validated,
                // instantiated, and trapped. Refused instead, which is what naming the construct is
                // for.
                let Some(member) = index
                    .own_members(item)
                    .iter()
                    .copied()
                    .find(|&id| index.member(id).kind == DefKind::Method)
                else {
                    return Err(WasmError::Unsupported(
                        "a lambda or method reference with no single abstract method",
                    ));
                };
                let mut params = alloc::vec![layout.class_ref(item)?];
                for ty in index.resolved_param_tys(member) {
                    params.push(layout.val_type(&ty)?);
                }
                let results = match index.resolved_member_ty(member) {
                    Ty::Void => Vec::new(),
                    ty => alloc::vec![layout.val_type(&ty)?],
                };
                let result = results.first().copied();
                let signature = module.add_type(SubType::plain(CompType::Func { params, results }));
                let function = module.func_index(out.len());
                layout.member_types.insert(member, signature);
                layout.functions.insert(member, function);
                out.push(Method {
                    owner: Some(item),
                    node: class.clone(),
                    input: position,
                    signature,
                    index: function,
                    export: None,
                    is_constructor: false,
                    result,
                    encloses: None,
                    captures: 0,
                    initialises: None,
                    lambda: Some(member),
                });
                continue;
            }
            // An `enum`'s members live under an `ENUM_BODY`, after the constants and the `;`.
            let Some(body) = class
                .children()
                .find(|child| matches!(child.kind(), CLASS_BODY | ENUM_BODY))
            else {
                continue;
            };
            // A class that declares no constructor still has initialisers to run, and an initialiser *block*
            // reads its own fields through `this` — which only a function has a slot 0 to be. So one is
            // synthesised, and the `new` calls it.
            let declares_constructor = body
                .children()
                .any(|member| member.kind() == CONSTRUCTOR_DECL);
            let has_initialisers = body.children().any(|member| {
                member.kind() == INITIALIZER
                    || (member.kind() == FIELD_DECL
                        && member
                            .children()
                            .any(|child| ast::Expr::cast(child).is_some()))
            });
            // An interface has no instances, so it has no instance initialisers: its fields are
            // implicitly `static` (§9.3) and run in the class's initialisation, not a constructor's.
            if !declares_constructor && has_initialisers && class.kind() != INTERFACE_DECL {
                let signature = module.add_type(SubType::plain(CompType::Func {
                    params: alloc::vec![layout.class_ref(item)?],
                    results: Vec::new(),
                }));
                let function = module.func_index(out.len());
                layout.default_constructors.insert(item, function);
                out.push(Method {
                    owner: Some(item),
                    node: body.clone(),
                    input: position,
                    signature,
                    index: function,
                    export: None,
                    is_constructor: false,
                    result: None,
                    encloses: None,
                    captures: 0,
                    initialises: Some(item),
                    lambda: None,
                });
            }
            for node in body.children() {
                if !matches!(node.kind(), METHOD_DECL | CONSTRUCTOR_DECL) {
                    continue;
                }
                // An abstract method has no body, so there is no function to declare: a virtual call
                // reaches the *implementations* instead. Declaring one would put a signature with a
                // result type over an empty body, which no engine accepts. Its declared *type* is
                // recorded all the same: a library's dispatch realm calls a consumer-provided
                // function at exactly this type, and a method with no function index has no type to
                // read one from.
                let is_constructor = node.kind() == CONSTRUCTOR_DECL;
                let member_name = Self::member_name_token(&node, is_constructor)
                    .ok_or(WasmError::Unsupported("a member with no name"))?;
                let member = Facts::of(*input).member_at(&member_name)?;
                let is_static = index.member(member).modifiers.is_static;
                if !is_constructor && node.children().find_map(ast::Block::cast).is_none() {
                    // A type this backend cannot represent makes the declared type unrepresentable
                    // too, and a method nothing here can call is a method whose type nothing needs:
                    // the declaration is skipped rather than turned into a new refusal, and the
                    // refusal stays where it belongs, at the call site that would need it.
                    if let Some(signature) =
                        Self::declared_member_type(member, index, layout, module)
                    {
                        layout.member_types.insert(member, signature);
                    }
                    continue;
                }

                let mut params = Vec::new();
                // An inner class's constructor takes the enclosing instance right after `this`, and
                // stores it into the synthetic field before the body runs.
                let encloses = is_constructor
                    .then(|| layout.inner.get(&item).copied())
                    .flatten();
                if is_constructor || !is_static {
                    params.push(layout.class_ref(item)?);
                }
                if let Some(enclosing) = encloses {
                    params.push(layout.class_ref(enclosing)?);
                }
                for ty in index.resolved_param_tys(member) {
                    params.push(layout.val_type(&ty)?);
                }
                // The captures come after every declared parameter, so a declared one keeps its slot.
                let captured = is_constructor
                    .then(|| layout.captures.get(&item).cloned())
                    .flatten()
                    .unwrap_or_default();
                for (_, ty) in &captured {
                    params.push(layout.val_type(ty)?);
                }
                let results = if is_constructor {
                    Vec::new()
                } else {
                    match index.resolved_member_ty(member) {
                        Ty::Void => Vec::new(),
                        ty => alloc::vec![layout.val_type(&ty)?],
                    }
                };

                let result = results.first().copied();
                let signature = module.add_type(SubType::plain(CompType::Func { params, results }));
                let function = module.func_index(out.len());
                layout.member_types.insert(member, signature);
                layout.functions.insert(member, function);
                out.push(Method {
                    owner: (!is_static).then_some(item),
                    node: node.clone(),
                    input: position,
                    signature,
                    index: function,
                    result,
                    encloses,
                    captures: captured.len(),
                    initialises: None,
                    lambda: None,
                    // A native package's classes are compiled into this module but are not its
                    // surface: exporting their `static` methods would put library internals in
                    // the list `--invoke` offers, and — because the first export of a name wins
                    // and the second is dropped — would silently take a project method's export
                    // away from it. `exported` is which list the declaration came from, and it is
                    // the only thing the two lists decide.
                    export: (exported && is_static && !is_constructor)
                        .then(|| index.member(member).name.clone()),
                    is_constructor,
                });
            }
        }
        Ok(())
    }

    /// The name token of a class member, which is a method or a constructor here.
    ///
    /// Two kinds rather than one because `ConstructorDecl` is not a [`ast::Decl`] variant — a
    /// constructor declares no type and no field, so the grammar keeps it out of that enum. Both
    /// arms go through the node's own generated accessor.
    fn member_name_token(node: &SyntaxNode, is_constructor: bool) -> Option<SyntaxToken> {
        if is_constructor {
            ast::ConstructorDecl::cast(node.clone())?.name_token()
        } else {
            ast::MethodDecl::cast(node.clone())?.name_token()
        }
    }
}

/// How Java declarations map onto wasm types and indices.
#[derive(Default)]
struct Layout {
    /// Each class's struct type index.
    structs: BTreeMap<ItemId, u32>,
    /// Each class's instance fields, in slot order, including those inherited.
    fields: BTreeMap<ItemId, Vec<Slot>>,
    /// Each class's declared wasm supertype, as the *type index*
    /// [`reserve_class`](Layout::reserve_class) resolved it — not an [`ItemId`] to be looked up
    /// again later.
    ///
    /// One question, one read. `reserve_class` builds the field prefix from whichever parent was
    /// already reserved, and `fill_class` runs after *every* class is reserved, so re-deriving the
    /// parent there answered against a fuller map and could name a supertype whose fields the
    /// prefix does not extend. On a supertype cycle it named the type itself, and the module was
    /// rejected by the validator rather than merely wrong. Recording the index here also carries
    /// the ordering wasm requires for free: a parent found in `structs` at reserve time was
    /// reserved strictly earlier, so its index is strictly lower.
    parents: BTreeMap<ItemId, u32>,
    /// The same edge as [`parents`](Self::parents), by [`ItemId`] — the *declared wasm* supertype
    /// chain, which is what a value of this struct may be passed as.
    ///
    /// It is a subset of the index's superclass chain, not a copy of it: an ancestor the module does
    /// not declare has no struct to be passed at, and under a supertype cycle the index's chain
    /// climbs an edge wasm's declared subtyping does not have. Following it here is what keeps a
    /// super-constructor call well-typed. Each entry points at a type reserved strictly earlier, so
    /// the chain is finite by construction and needs no visited set.
    parent_item: BTreeMap<ItemId, ItemId>,
    /// Each method's function index.
    functions: BTreeMap<MemberId, u32>,
    /// The function index the module's *defined* functions start at; everything below it is an
    /// import. A member whose function sits below answers with the host's body or another module's,
    /// which is what keeps it out of a published realm: a consumer cannot call the body the library
    /// did not export, so a slot for it would be a slot with no fallback.
    first_function: u32,
    /// A non-`static` nested class and the class that encloses it. Its instance holds the enclosing one
    /// in a synthetic field, appended *after* its own — which keeps every real field's slot where
    /// `field_slot` computes it, and is why a class extending an inner class is reported instead.
    inner: BTreeMap<ItemId, ItemId>,
    /// The synthetic enclosing-instance field's index, for each inner class.
    outer: BTreeMap<ItemId, u32>,
    /// The function that runs a constructor-less class's initialisers, for the `new` to call. A block reads its
    /// own fields through `this`, and only a function has a slot 0 to be `this`.
    default_constructors: BTreeMap<ItemId, u32>,
    /// Every local class and the locals it captures, in source order. Each becomes a struct field and a
    /// *trailing* constructor parameter, which is how the class outlives the frame the local lived in.
    captures: BTreeMap<ItemId, Vec<(DefId, Ty)>>,
    /// The struct field index of the first capture, for each class that has any.
    capture_slot: BTreeMap<ItemId, u32>,
    /// The one exception tag, if the module throws anything. Every Java throw is a reference, so one
    /// tag carrying one reference covers all of them and the *class* of that reference is what a
    /// `catch` tests.
    ///
    /// For a defined tag this is its index in the tag space; when the tag is a linked library's it
    /// is the import's index in the same space, so every `throw` and `try_table` reads one value.
    tag: Option<u32>,
    /// The declared function type the defined tag's payload occupies, for a library's ABI.
    ///
    /// The two are different index spaces: `tag` names the tag, and this names the type a consumer
    /// must import the tag *at*. `None` when the tag is itself an import — the payload is then the
    /// library's replayed type and this module states nothing about it.
    tag_type: Option<u32>,
    /// [`WasmOptions::assertions`], carried here because the statement lowering is the only thing
    /// that reads it and every body already holds the layout.
    assertions: bool,
    /// The position table under construction, when [`WasmOptions::positions`] asked for one.
    ///
    /// Carried here for the same reason `assertions` is: the statement lowering is what writes it,
    /// and every body already holds the layout. `None` — the default — is a compile that emits no
    /// position instructions at all.
    positions: Option<PositionsBuild>,
    /// Every interface this module declares. An interface gets no struct type — wasm's declared
    /// subtyping is single-inheritance, so it could not be a supertype of two unrelated classes — so a
    /// value of interface type is held at the top of the reference hierarchy and narrowed at each use.
    interfaces: BTreeSet<ItemId>,
    /// `java.lang.Object`, when the index holds it.
    ///
    /// The one library type representable with no package linked: it is the root of Java's
    /// reference hierarchy and `anyref` is wasm's, so a value of it is held exactly where an
    /// interface-typed one is. Refusing it instead put every file that so much as declares an
    /// `Object` field outside the subset, over a type whose representation the target already has.
    object: Option<ItemId>,
    /// Each `static` field's global index. A Java `static` field is module state, which is what a
    /// wasm global is; an instance field is a struct slot instead.
    statics: BTreeMap<MemberId, u32>,
    /// `(initialiser function, "has run" flag global)` for each class with static state.
    ///
    /// A JVM initialises a class on its first *use* (JLS §12.4.1), and one start function cannot
    /// express that: a class declared later may be read by one declared earlier. So each class's
    /// initialisation is its own guarded function, called from the start function in source order and
    /// again from every `static` access — the guard makes all but the first call free.
    class_inits: BTreeMap<ItemId, (u32, u32)>,
    /// `(element type, array type index)`. A `Vec` because `ValType` has no ordering and a program
    /// has a handful of distinct element types.
    ///
    /// Starts out holding every array type a linked library declares, in the library's own index
    /// space: an array that crosses the boundary has to be the type the other side names, and a
    /// replayed declaration is the only one that does. The project's own arrays are declared after,
    /// and only for element types no library already provides.
    arrays: Vec<(ValType, u32)>,
    /// Every distinct string literal in the module: its text, the passive data segment holding its
    /// UTF-16 code units, and how many units there are.
    ///
    /// Keyed by text rather than by occurrence — two literals that read the same share one segment,
    /// which is what a constant pool would do — and collected before any body is lowered because a
    /// body cannot add a data segment: the module's is already written by then, so the expression
    /// only reports which one it reads.
    literals: Vec<(String, u32, u32)>,
    /// The classes a linked library declares, by item, with the library's link name. Their structs
    /// are already replayed into [`structs`](Self::structs), so `reserve_class` leaves them alone
    /// and a value of the type is a concrete reference rather than `anyref`.
    external_classes: BTreeMap<ItemId, String>,
    /// A linked library's constructors: the factory import that allocates and runs one.
    external_constructors: BTreeMap<MemberId, u32>,
    /// A linked library's constructor *bodies*: the import a `super(…)` calls to initialise an
    /// object that already exists.
    ///
    /// The factory beside it allocates, so it cannot be handed the `this` a constructor invocation
    /// has; the body is the same `(this, arguments…) -> ()` function an in-module constructor is,
    /// exported under a name of its own.
    external_initializers: BTreeMap<MemberId, u32>,
    /// A linked library's `static` fields: the accessor imports a read and a write call.
    external_statics: BTreeMap<MemberId, (u32, u32)>,
    /// Every method's declared function type index, whether or not this module lowers a body for
    /// it. A virtual call a *consumer* might answer needs the type at lowering time — its realm
    /// arm calls a function reference *at* that type — and an abstract method has no function
    /// index to read one from, so the type is recorded where it is declared instead: a declared
    /// method here, a synthesised accessor there, and a stub's own members for the types only
    /// `Object` and the stub interfaces have.
    member_types: BTreeMap<MemberId, u32>,
    /// The dispatch realm this module publishes, when it is a library. Set up before any body is
    /// lowered — the struct's type index and the global have to exist before an arm names them —
    /// and filled once every body has had its say.
    realm: Option<RealmBuild>,
    /// The dispatch realms this module *consumes*, when it links libraries: one per library that
    /// published one, in link order.
    realms: Vec<RealmImport>,
}

/// A virtual call's plan: what this module already knows how to answer, what the call produces,
/// and whether a realm is asked before either.
///
/// Three facts that travel together and are all decided in the one place that routes a call —
/// bundling them is what keeps the emitting function's signature readable.
#[derive(Debug, Clone, Copy)]
struct Dispatch<'a> {
    /// Every in-module class that overrides the method, most-derived first.
    overriders: &'a [(ItemId, MemberId)],
    /// The call's result type: `None` for a `void` method.
    ty: Option<ValType>,
    /// Whether the realm arm is emitted ahead of the chain.
    realm: bool,
}

/// A library's dispatch realm under construction.
///
/// The slots are collected while bodies are lowered — the first virtual call to a member is what
/// registers it — so the vector is behind a [`RefCell`]: a body holds the layout by shared
/// reference, and the slot list is the one thing a body adds to.
struct RealmBuild {
    /// The realm struct's reserved type index. Reserved before any body because an arm names the
    /// struct in `struct.get`, and filled once the slot list is final.
    structure: u32,
    /// The global `$jals$link` stores the installed realm in.
    global: u32,
    /// `(member, function type)` per slot, in first-call order.
    slots: RefCell<Vec<(MemberId, u32)>>,
}

/// A module's statement positions under construction.
///
/// The table is collected while bodies are lowered — every statement enters through
/// [`Lowering::stmt`], which is what makes one index per statement enough — so the vector is
/// behind a [`RefCell`] for the same reason the realm's slot list is: a body holds the layout by
/// shared reference, and the table is the one thing a body adds to.
struct PositionsBuild {
    /// The exported global every statement stores its index into.
    global: u32,
    /// Where each statement was written, index 0 first. The index is what the global holds.
    statements: RefCell<Vec<Position>>,
}

/// A linked library's realm as its consumer holds it.
struct RealmImport {
    /// The `$jals$link` import, which installs a struct built here.
    link: u32,
    /// The replayed realm struct type index.
    structure: u32,
    /// One slot per field, in field order.
    slots: Vec<RealmSlotImport>,
}

/// One field of a linked library's realm, as the consumer sees it.
struct RealmSlotImport {
    /// The member the slot answers, when this module's index holds it. `None` for a slot whose
    /// member is of a type this module cannot represent — the thunk then traps rather than
    /// dispatching, which is the same answer the library's own chain gives.
    member: Option<MemberId>,
    /// The replayed function type the field holds.
    ty: u32,
}

/// One value a call site pushes.
enum Arg<'e> {
    /// An argument, at the type its parameter declares.
    Value(&'e ast::Expr, ValType),
    /// An argument past the last declared parameter, which only a malformed call has: pushed at
    /// whatever type it has, so the validator names the mismatch rather than this layer guessing.
    Untyped(&'e ast::Expr),
    /// The array a variable-arity call builds out of its trailing arguments.
    Packed {
        values: &'e [ast::Expr],
        /// The element type each value is converted to.
        element: Ty,
        /// The array type index the values are gathered into.
        array: u32,
    },
}

/// One field of a class's wasm struct, in the order the struct declares them.
///
/// The synthetic ones are *in* the list rather than appended when the struct is written, because
/// wasm's declared subtyping requires a subtype's fields to extend its supertype's as a prefix. A
/// list holding only the declared fields put a subclass's first field on top of its superclass's
/// enclosing instance — which is why a subclass of an inner class had to be refused outright.
#[derive(Clone)]
enum Slot {
    /// A field the source declared.
    Declared(MemberId),
    /// One field of a linked library's class, kept as the wasm type — and mutability — the library
    /// declared it with.
    ///
    /// The slots were laid out by the library's own compile, synthetic entries and all, and the
    /// replayed struct type *is* that layout's answer, so the field is read from it rather than
    /// rebuilt from the declarations: rebuilding would guess at the order, and a wrong guess is a
    /// subtype whose fields do not extend its supertype's, which the validator refuses.
    ///
    /// The member is deliberately not recorded. An inherited library field therefore still reports
    /// the missing capability it always did — this module cannot name the slot — and what the entry
    /// is *for* is position: a project subclass's own fields have to start after exactly this many
    /// slots.
    External(FieldType),
    /// The enclosing instance an inner class holds, and the type of it.
    Enclosing(ItemId),
    /// One local a local or anonymous class captured, with the type it was captured at — read from
    /// the declaring file, which is the only place it is known.
    Capture(Ty),
}

impl Layout {
    /// Whether the member's function is an *import* — a `native`'s host body or a linked
    /// library's — rather than one this module lowers.
    ///
    /// Everything below [`first_function`](Self::first_function) is the import half of the
    /// function index space; a member with no function at all has nothing to be an import.
    fn imported(&self, member: MemberId) -> bool {
        self.functions
            .get(&member)
            .is_some_and(|&function| function < self.first_function)
    }

    /// Every class in this module that overrides `member`, most-derived first.
    ///
    /// wasm has no dynamic loading and no classpath: this backend compiles the *whole* project as one
    /// module, so the set of classes that can override a method is closed and known here. That is what
    /// makes dispatch by type test sound — and it is the only reason it is, which is why it is written
    /// down rather than assumed.
    ///
    /// Empty when nothing overrides the method, which is the common case and the one that keeps a
    /// direct `call`.
    fn overriders(&self, index: &ProjectIndex, member: MemberId) -> Vec<(ItemId, MemberId)> {
        let info = index.member(member);
        if info.kind != DefKind::Method {
            return Vec::new();
        }
        let owner = info.owner;
        let mut found: Vec<(ItemId, MemberId)> = Vec::new();
        for &item in self.structs.keys() {
            if item == owner || !index.is_subtype(item, owner) {
                continue;
            }
            // Only a definite override — the strict collapse, by name. A false positive here routes
            // a call to the wrong method (output that loads, validates, and runs wrongly, which no
            // later stage catches) while a false negative leaves the direct `call` a non-overridden
            // method would have had anyway. That is the opposite collapse from the bridge
            // emission's, and it is why the shared fact has three answers rather than two.
            //
            // **Inherited, not just declared.** `interface I { int f(); }` with
            // `class Base { public int f() { … } }` and `class C extends Base implements I {}` is a
            // `C` whose implementation of `I.f` is written in `Base`, and `C` declares nothing at
            // all. Scanning only `own_members` found no override, the call fell through to the
            // no-function arm below, and that arm — which reads "nothing implements this, so no such
            // object exists" — emitted `unreachable` against a receiver whose implementation is one
            // function away in the same module. Nearest-first, so a subclass's own override still
            // wins over the one it inherits, and only a body this module actually lowered counts:
            // dispatching to a member with no function index is a call to nothing.
            let over = index.members_of(item).into_iter().find(|&id| {
                id != member
                    && self.functions.contains_key(&id)
                    && index.implements_for(item, id, member).is_certain()
            });
            if let Some(over) = over {
                found.push((item, over));
            }
        }
        // Most-derived first, so a subclass's override is tested before its superclass's: testing the
        // other way round would let the base class's `ref.test` succeed for every descendant and answer
        // with the wrong method.
        found.sort_by(|&(a, _), &(b, _)| {
            index
                .is_subtype(a, b)
                .cmp(&index.is_subtype(b, a))
                .reverse()
        });
        found
    }

    /// The constructors of `item` this module actually lowered a function for.
    ///
    /// Not every indexed constructor is one. The index gives a class that writes none the **default**
    /// constructor JLS §8.8.9 says it has, and nothing lowers a body for that: whatever it would run
    /// — the field initialisers — is in [`default_constructors`](Self::default_constructors)
    /// instead. So "does this class have a constructor" and "is there a function to call" are two
    /// questions, and asking the first where the second was meant found a member with no function
    /// and stopped there: a `new` that skipped every initialiser above it, in a module that
    /// validates.
    fn constructors<'a>(
        &'a self,
        index: &'a ProjectIndex,
        item: ItemId,
    ) -> impl Iterator<Item = MemberId> + 'a {
        index
            .own_members(item)
            .iter()
            .copied()
            .filter(move |&member| {
                index.member(member).kind == DefKind::Constructor
                    && self.functions.contains_key(&member)
            })
    }

    /// Reserve `item`'s struct type index and work out which fields it holds.
    ///
    /// Reserving rather than declaring is what makes an array-typed field work: the field's *type*
    /// needs an array type index, and an array's element may be a class — so neither can be laid out
    /// before the other. Every type lives in one recursive group, so an index may be referred to
    /// before its body exists; [`fill_class`](Self::fill_class) writes the body once every index is
    /// known.
    ///
    /// The field list itself needs no types, only its supertype's list first — which is why
    /// `classes_in_order` still walks supertypes first.
    fn reserve_class(&mut self, item: ItemId, index: &ProjectIndex, module: &mut Module) {
        if self.structs.contains_key(&item) {
            return;
        }
        let parent = index
            .direct_superclass(item)
            .filter(|id| self.structs.contains_key(id));
        // The supertype's *whole* list, synthetic fields included: a subtype's fields extend its
        // supertype's as a prefix, so anything the supertype holds occupies a slot here too.
        let mut slots: Vec<Slot> = parent
            .and_then(|id| self.fields.get(&id))
            .cloned()
            .unwrap_or_default();
        for &member in index.own_members(item) {
            let info = index.member(member);
            if info.kind == DefKind::Field && !info.modifiers.is_static {
                slots.push(Slot::Declared(member));
            }
        }
        // Then this class's own synthetic fields, after every field it declares.
        if let Some(&enclosing) = self.inner.get(&item) {
            self.outer
                .insert(item, u32::try_from(slots.len()).unwrap_or(u32::MAX));
            slots.push(Slot::Enclosing(enclosing));
        }
        let captured = self.captures.get(&item).cloned().unwrap_or_default();
        if !captured.is_empty() {
            self.capture_slot
                .insert(item, u32::try_from(slots.len()).unwrap_or(u32::MAX));
            for (_, ty) in &captured {
                slots.push(Slot::Capture(ty.clone()));
            }
        }
        if let Some(id) = parent
            && let Some(&index) = self.structs.get(&id)
        {
            self.parents.insert(item, index);
            self.parent_item.insert(item, id);
        }
        self.structs.insert(item, module.reserve_type());
        self.fields.insert(item, slots);
    }

    /// Write `item`'s struct body: its supertype's fields followed by its own, at their wasm types.
    ///
    /// The order and the synthetic entries were settled by
    /// [`reserve_class`](Self::reserve_class) — which is what makes a subtype's fields a real prefix
    /// extension of its supertype's — so this only resolves each slot to a wasm type.
    ///
    /// The declared supertype is **read** from [`parents`](Self::parents) rather than re-derived
    /// from the index, and that is what keeps the two halves of one type consistent by
    /// construction: `reserve_class` chose the parent whose field list became this struct's prefix,
    /// while every class is reserved before any is filled, so asking the index again here answers
    /// against a fuller `structs` and may name a different type. `class A extends B {}` beside
    /// `class B extends A {}` — which parses and indexes — reserved `B` with no prefix and then
    /// declared `A` as its supertype, and `class C extends C {}` declared its own type index; both
    /// are modules a wasm validator refuses, and one bad type invalidates the whole module.
    fn fill_class(&self, item: ItemId, index: &ProjectIndex, module: &mut Module) -> Result<()> {
        let Some(&type_index) = self.structs.get(&item) else {
            return Ok(());
        };
        let slots = self.fields.get(&item).cloned().unwrap_or_default();
        let mut fields = Vec::with_capacity(slots.len());
        for slot in &slots {
            let field = match slot {
                Slot::Declared(member) => {
                    let ty = self.val_type(&index.resolved_member_ty(*member))?;
                    FieldType {
                        storage: StorageType::Val(ty),
                        // Every Java field is assignable unless `final`, and even a `final` one is
                        // written once by a constructor — after `struct.new_default` has already
                        // made it.
                        mutable: true,
                    }
                }
                Slot::Enclosing(enclosing) => FieldType {
                    storage: StorageType::Val(self.class_ref(*enclosing)?),
                    mutable: true,
                },
                Slot::Capture(ty) => FieldType {
                    storage: StorageType::Val(self.val_type(ty)?),
                    mutable: true,
                },
                // A library's field is copied exactly as the library declared it, mutability
                // included: a mutable field is invariant in wasm's declared subtyping, so
                // recomputing either half would put a field here that does not extend the
                // supertype's — a module the validator refuses.
                Slot::External(field) => *field,
            };
            fields.push(field);
        }
        module.set_type(
            type_index,
            SubType {
                is_final: false,
                supertype: self.parents.get(&item).copied(),
                comp: CompType::Struct(fields),
            },
        );
        Ok(())
    }

    /// Give every `static` field a mutable global, initialised in place.
    ///
    /// A global's initialiser is a *constant expression*: the format allows only a handful of
    /// instructions there, so anything a `<clinit>` would have to compute cannot live in one. A
    /// non-constant initialiser is reported rather than replaced by the type's default — silently
    /// dropping a `static` initialiser is a wrong value in a module that validates.
    fn declare_statics(
        &mut self,
        input: &TypedFile<'_>,
        index: &ProjectIndex,
        module: &mut Module,
        out: &mut Vec<(MemberId, ast::Expr)>,
    ) -> Result<()> {
        for node in input.root().descendants() {
            if node.kind() != FIELD_DECL {
                continue;
            }
            // Each declarator with the value written after its own `=`, which is not the same as
            // pairing names with expressions by index — see `Facts::declarators`.
            for (name, value) in Facts::declarators(&node) {
                let Ok(member) = Facts::of(*input).member_at(&name) else {
                    continue;
                };
                if !index.member(member).modifiers.is_static {
                    continue;
                }
                // A field whose type has no wasm representation gets no global, and no report either:
                // an unrepresentable type is reported where it is *used*, so a generated
                // `static final String` nothing reads stays inert — which is what keeps a project
                // compiling for wasm alongside a class the user never wrote.
                let Ok(ty) = self.val_type(&index.resolved_member_ty(member)) else {
                    continue;
                };
                let (init, deferred) = Self::constant_init(value.as_ref(), ty);
                module.globals.push(Global { ty, init });
                let global =
                    u32::try_from(module.globals.len() - 1).map_err(|_| WasmError::TooLarge)?;
                self.statics.insert(member, global);
                // A global's own initialiser is a constant expression, so anything that has to be
                // *computed* runs in the start function instead — over the global the default already
                // holds, which is exactly the order a `<clinit>` gives.
                if deferred && let Some(value) = value {
                    out.push((member, value));
                }
            }
        }
        Ok(())
    }

    /// Give every `enum` constant a global of its enum's own type.
    ///
    /// A constant is a `static final` field whose value the source never writes: it is an allocation,
    /// which no constant expression can hold, so the global starts as `null` and the start function
    /// builds it. A constant with a **body** is *reported* — it is an anonymous subclass, which is its
    /// own type.
    ///
    /// The two synthetic parameters a JVM `enum` constructor takes (`name`, `ordinal`) have nothing to
    /// carry here: `name()` and `ordinal()` come from `java.lang.Enum` and involve a `String`, which
    /// this backend has no representation for. So a constant's arguments go straight to the declared
    /// constructor, with nothing ahead of them.
    fn declare_constants(
        &mut self,
        input: &TypedFile<'_>,
        index: &ProjectIndex,
        module: &mut Module,
        out: &mut Vec<(MemberId, ItemId, SyntaxNode)>,
    ) -> Result<()> {
        for node in input.root().descendants() {
            if node.kind() != ENUM_DECL {
                continue;
            }
            let owner = Self::owner_of(&node, input, index)?;
            let body = node.children().find(|child| child.kind() == ENUM_BODY);
            for member in body.iter().flat_map(SyntaxNode::children) {
                let Some(constant) = ast::EnumConstant::cast(member.clone()) else {
                    continue;
                };
                let name = constant
                    .name_token()
                    .ok_or(WasmError::Unsupported("an `enum` constant with no name"))?;
                let id = Facts::of(*input).member_at(&name)?;
                let ty = self.class_ref(owner)?;
                let mut init = Insn::new();
                Self::default_value(ty, &mut init);
                module.globals.push(Global {
                    ty,
                    init: init.into_body(),
                });
                let global =
                    u32::try_from(module.globals.len() - 1).map_err(|_| WasmError::TooLarge)?;
                self.statics.insert(id, global);
                out.push((id, owner, member.clone()));
            }
        }
        Ok(())
    }

    /// The indexed item a type declaration declares.
    fn owner_of(node: &SyntaxNode, input: &TypedFile<'_>, index: &ProjectIndex) -> Result<ItemId> {
        let name = ast::Decl::name_token_of(node)
            .ok_or(WasmError::Unsupported("a type declaration with no name"))?;
        index
            .item_by_decl(input.file(), usize::from(name.text_range().start()))
            .ok_or_else(|| WasmError::Unresolved(name.text().into()))
    }

    /// The constant expression a `static` field's global is initialised with.
    ///
    /// No initialiser is the type's default, which is exactly Java's rule (§4.12.5). A literal folds
    /// into the same shape. Anything else — including a literal that would need a widening conversion,
    /// since a constant expression cannot hold one — is reported rather than replaced by the default.
    fn constant_init(value: Option<&ast::Expr>, ty: ValType) -> (Vec<Instr>, bool) {
        use jals_syntax::SyntaxKind::{
            CHAR_LITERAL, FALSE_KW, FLOAT_LITERAL, INT_LITERAL, NULL_KW, TRUE_KW,
        };
        // Anything a constant expression cannot hold falls back to the type's default *and* asks for a
        // start-function assignment, which is the order a `<clinit>` gives: the field holds its default
        // until the initialiser runs.
        let default = || {
            let mut insn = Insn::new();
            Self::default_value(ty, &mut insn);
            (insn.into_body(), true)
        };
        let Some(value) = value else {
            let mut insn = Insn::new();
            Self::default_value(ty, &mut insn);
            return (insn.into_body(), false);
        };
        let ast::Expr::Literal(literal) = value else {
            return default();
        };
        let Some(token) = literal
            .syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| !token.kind().is_trivia())
        else {
            return default();
        };
        let text = token.text();
        let mut insn = Insn::new();
        match (token.kind(), ty) {
            (NULL_KW, ValType::Ref(_)) => {
                insn.ref_null(HeapType::None);
            }
            (TRUE_KW, ValType::I32) => {
                insn.i32_const(1);
            }
            (FALSE_KW, ValType::I32) => {
                insn.i32_const(0);
            }
            (CHAR_LITERAL, ValType::I32) => {
                let Ok(character) = Literal::character(text) else {
                    return default();
                };
                insn.i32_const(character as i32);
            }
            // An `int` literal into a wider field is an assignment conversion, and the *constant* form
            // of one is folding: `static long n = 1` writes `i64.const 1`, not `i32.const` plus an
            // extension no constant expression may hold. Which width to fold *into* is the field's,
            // so the one the fact reads off the suffix is dropped.
            (INT_LITERAL, _) => {
                let Ok((value, _)) = Literal::integer(text) else {
                    return default();
                };
                #[allow(clippy::cast_precision_loss)]
                match ty {
                    ValType::I64 => insn.i64_const(value),
                    ValType::F32 => insn.f32_const(value as f32),
                    ValType::F64 => insn.f64_const(value as f64),
                    _ => match i32::try_from(value) {
                        Ok(value) => insn.i32_const(value),
                        Err(_) => return default(),
                    },
                };
            }
            (FLOAT_LITERAL, ValType::F32 | ValType::F64) => {
                let Ok((value, _)) = Literal::floating(text) else {
                    return default();
                };
                #[allow(clippy::cast_possible_truncation)]
                if ty == ValType::F32 {
                    insn.f32_const(value as f32);
                } else {
                    insn.f64_const(value);
                }
            }
            // A literal whose type is not the field's and not foldable into it — the start function
            // lowers it with the conversion the expression path already knows how to emit.
            _ => return default(),
        }
        (insn.into_body(), false)
    }

    /// A type's default value: what a `static` field with no initialiser holds (§4.12.5).
    fn default_value(ty: ValType, insn: &mut Insn) {
        match ty {
            ValType::I32 => insn.i32_const(0),
            ValType::I64 => insn.i64_const(0),
            ValType::F32 => insn.f32_const(0.0),
            ValType::F64 => insn.f64_const(0.0),
            ValType::Ref(_) => insn.ref_null(HeapType::None),
        };
    }

    /// Declare the array type `ty` needs, and any nested one inside it (`int[][]` is an array of
    /// arrays, so the inner type has to exist first).
    fn declare_array(&mut self, ty: &Ty, module: &mut Module) -> Result<()> {
        let Ty::Array(element) = ty else {
            return Ok(());
        };
        self.declare_array(element, module)?;
        // A type this backend cannot represent is not an error *here*: it only matters if
        // something actually uses it, and that use reports it with the right span.
        let Ok(element) = self.val_type(element) else {
            return Ok(());
        };
        if self.array_type(element).is_some() {
            return Ok(());
        }
        let type_index = module.add_type(SubType::plain(CompType::Array(FieldType {
            storage: StorageType::Val(element),
            mutable: true,
        })));
        self.arrays.push((element, type_index));
        Ok(())
    }

    /// The declared array type whose elements are `element`.
    fn array_type(&self, element: ValType) -> Option<u32> {
        self.arrays
            .iter()
            .find(|(candidate, _)| *candidate == element)
            .map(|(_, index)| *index)
    }

    /// Whether `index` names one of this module's array types.
    ///
    /// The one place a Java type relation has no wasm counterpart: arrays are *covariant* in Java
    /// (`String[]` is an `Object[]`) and **invariant** here, because a wasm array is mutable and
    /// declared subtyping over it would let a write of the wrong element type through. So a
    /// covariant array assignment is refused rather than emitted.
    fn is_array(&self, index: u32) -> bool {
        self.arrays.iter().any(|&(_, candidate)| candidate == index)
    }

    /// A nullable reference to `item`'s struct type — how every Java reference is represented.
    fn class_ref(&self, item: ItemId) -> Result<ValType> {
        // `java.lang.Object` sits where an interface does, and for the same reason: both are
        // satisfied by a value of *any* reference type, and wasm's `anyref` is exactly that.
        if self.interfaces.contains(&item) || self.object == Some(item) {
            return Ok(ValType::Ref(RefType::nullable(HeapType::Any)));
        }
        let index = self
            .structs
            .get(&item)
            .ok_or_else(|| WasmError::NoRepresentation("an undeclared class".to_owned()))?;
        Ok(ValType::Ref(RefType::nullable(HeapType::Concrete(*index))))
    }

    /// The wasm type a Java value of type `ty` has.
    fn val_type(&self, ty: &Ty) -> Result<ValType> {
        Ok(match ty {
            // Every integral type narrower than `long` computes as `i32`, exactly as on the JVM.
            Ty::Primitive(
                Primitive::Boolean
                | Primitive::Byte
                | Primitive::Short
                | Primitive::Char
                | Primitive::Int,
            ) => ValType::I32,
            Ty::Primitive(Primitive::Long) => ValType::I64,
            Ty::Primitive(Primitive::Float) => ValType::F32,
            Ty::Primitive(Primitive::Double) => ValType::F64,
            Ty::Class(_) => {
                let item = ty
                    .project_id()
                    .filter(|id| {
                        self.structs.contains_key(id)
                            || self.interfaces.contains(id)
                            || self.object == Some(*id)
                    })
                    .ok_or_else(|| WasmError::NoRepresentation(ty.to_string()))?;
                self.class_ref(item)?
            }
            Ty::Array(element) => {
                let element = self.val_type(element)?;
                let array = self
                    .array_type(element)
                    .ok_or_else(|| WasmError::NoRepresentation(ty.to_string()))?;
                ValType::Ref(RefType::nullable(HeapType::Concrete(array)))
            }
            // A type variable erases to its bound, and to `Object` with none (JLS §4.6) — so its
            // representation is the top of the reference hierarchy, exactly where an `Object` and an
            // interface-typed value sit. Every use at a concrete type comes back down with a
            // `ref.cast`, which is what erasure costs on this target as it does on the JVM.
            //
            // Not the bound's own struct type even when the bound is a class this module lays out:
            // a field of type `T` is one field whatever a use instantiates it at, and typing it at
            // the bound would make two instantiations two different structs.
            Ty::TypeVar { .. } => ValType::Ref(RefType::nullable(HeapType::Any)),
            other => return Err(WasmError::NoRepresentation(other.to_string())),
        })
    }

    fn field_slot(&self, owner: ItemId, member: MemberId) -> Option<u32> {
        let slot = self
            .fields
            .get(&owner)?
            .iter()
            .position(|slot| matches!(slot, Slot::Declared(id) if *id == member))?;
        u32::try_from(slot).ok()
    }

    /// [`field_slot`](Self::field_slot), with the refusal that names the actual problem.
    ///
    /// A linked library class has no slot map here: its fields were laid out in the library's own
    /// module, and deriving the slots from the replayed struct type would be inventing the
    /// declaration order. The data is reachable — the struct is shared — but only through the
    /// library's own methods, so the report says what is missing rather than pretending a field
    /// of a class that exists in the index is not there.
    fn field_slot_or(
        &self,
        index: &ProjectIndex,
        owner: ItemId,
        member: MemberId,
        name: String,
    ) -> Result<u32> {
        if let Some(slot) = self.field_slot(owner, member) {
            return Ok(slot);
        }
        // The *declaring* class, not the receiver's. A project class that inherits a library field
        // reaches here with an owner this module lays out, and reporting the name as unresolved
        // would send a reader looking for a typo in legal Java; the slot is the library's — its
        // position came with the replayed struct and its member name deliberately did not.
        if self
            .external_classes
            .contains_key(&index.member(member).owner)
        {
            return Err(WasmError::Unsupported(
                "an instance field of a linked library class",
            ));
        }
        Err(WasmError::Unresolved(name))
    }
}

/// Everything that runs in some class's initialisation, before it is grouped by the class it belongs to.
#[derive(Clone, Copy)]
struct StaticState<'a> {
    /// `(field, initialiser)` for every `static` field whose value has to be computed.
    deferred: &'a [(MemberId, ast::Expr)],
    /// `(field, enum, declaration)` for every `enum` constant, which is an allocation rather than a
    /// constant expression.
    constants: &'a [(MemberId, ItemId, SyntaxNode)],
    /// `(input, owner, block)` for every `static { … }`.
    blocks: &'a [(usize, ItemId, ast::Block)],
}

/// One step of a class's static sequence, which JLS §12.4.2 runs in *source* order.
enum StaticStep<'a> {
    /// A `static` field's computed initialiser.
    Field(&'a (MemberId, ast::Expr)),
    /// A `static { … }` block.
    Block(&'a ast::Block),
}

/// One method body being lowered.
struct Body {
    locals: Vec<ValType>,
    code: Vec<Instr>,
}

impl Body {
    fn lower(
        method: &Method,
        input: &TypedFile<'_>,
        index: &ProjectIndex,
        layout: &Layout,
        positions: bool,
    ) -> Result<Self> {
        let mut lowering = Lowering {
            input,
            index,
            layout,
            positions,
            slots: Vec::new(),
            locals: Vec::new(),
            next: 0,
            owner: method.owner,
            loops: Vec::new(),
            pending_label: None,
            cleanups: Vec::new(),
            yields: Vec::new(),
            result: method.result,
        };
        // `this` is parameter 0 of an instance method or a constructor.
        if method.owner.is_some() || method.is_constructor {
            lowering.next += 1;
        }
        // An inner class's constructor takes the enclosing instance next, before any declared parameter.
        if method.encloses.is_some() {
            lowering.next += 1;
        }

        // A lambda's parameters live under its own `LambdaParams`, and its captures are fields rather than
        // parameters — so nothing trails them.
        // The synthesised constructor: `this` is slot 0, so a block initialiser reads its fields the way every
        // other body does.
        if let Some(owner) = method.initialises {
            let mut insn = Insn::new();
            // The implicit `super()` first, as a declared constructor's is: the superclass's
            // initialisers run before this class's (§12.5). Except under an `enum`: a constant's body is
            // a subclass whose *constant site* calls the enum's constructor, that being the one place
            // the constant's arguments exist — calling it here too would run the enum's twice, and the
            // no-argument one at that, which is a different constructor from the one selected.
            let under_enum = index
                .direct_superclass(owner)
                .is_some_and(|parent| index.item(parent).kind == DefKind::Enum);
            if let Some((declaring, function)) = Self::super_constructor(owner, index, layout)
                && !under_enum
            {
                if layout.inner.contains_key(&declaring) {
                    return Err(WasmError::Unsupported(
                        "an implicit `super()` to an inner class's constructor",
                    ));
                }
                insn.local_get(0).call(function);
            }
            lowering.initializers(owner, &method.node, 0, &mut insn)?;
            return Ok(Self {
                locals: lowering.locals,
                code: insn.into_body(),
            });
        }
        if let Some(member) = method.lambda
            && method.node.kind() == METHOD_REF_EXPR
        {
            // A method reference's body is one delegation: pass the interface method's own arguments straight
            // to the method the source named. Its parameters need no bindings, because nothing reads them by
            // name — they are forwarded by position.
            let mut insn = Insn::new();
            // `T::new` allocates rather than delegating: the object *is* what the interface method returns.
            if Facts::constructs(&method.node) {
                let created = Lowering::constructed_item(&method.node, input, index)?;
                let arity = index.member(member).params.len();
                // A linked library's class is built by the library, exactly as in the explicit
                // `new` path: its constructor's in-module shape leads with a `this` no consumer
                // has, and the factory is what stands in for it. Without this arm the body fell
                // through to a bare `struct.new_default` — the object existed, the constructor
                // never ran, and nothing failed, because the module validates.
                if layout.external_classes.contains_key(&created) {
                    // An inner class's factory leads with its enclosing instance, which a
                    // constructor reference's arguments do not carry: `Outer.Inner::new` takes it
                    // as the first parameter of the interface method, a shape this path does not
                    // model. Refusing by name beats emitting a call one argument short.
                    if layout.inner.contains_key(&created) {
                        return Err(WasmError::Unsupported(
                            "a constructor reference to a linked library's inner class",
                        ));
                    }
                    let constructor = index
                        .own_members(created)
                        .iter()
                        .copied()
                        .find(|&id| {
                            index.member(id).kind == DefKind::Constructor
                                && index.member(id).params.len() == arity
                        })
                        .ok_or(WasmError::Unsupported(
                            "a constructor reference with no matching constructor",
                        ))?;
                    let factory = *layout.external_constructors.get(&constructor).ok_or(
                        WasmError::Unsupported("a constructor a linked library does not export"),
                    )?;
                    for position in 0..arity {
                        insn.local_get(
                            u32::try_from(position + 1).map_err(|_| WasmError::TooLarge)?,
                        );
                    }
                    insn.call(factory).return_();
                    return Ok(Self {
                        locals: Vec::new(),
                        code: insn.into_body(),
                    });
                }
                let struct_type = layout.structs[&created];
                insn.struct_new_default(struct_type);
                // Only one with a *body*: the index also holds the default constructor every class
                // without a written one has (JLS §8.8.9), which nothing lowered a function for —
                // and which is the arm below, not a constructor this can call.
                let constructor = layout
                    .constructors(index, created)
                    .find(|&id| index.member(id).params.len() == arity);
                if let Some(constructor) = constructor {
                    let function =
                        *layout
                            .functions
                            .get(&constructor)
                            .ok_or(WasmError::Unsupported(
                                "a constructor reference to a constructor with no body",
                            ))?;
                    let slot = u32::try_from(lowering.locals.len()).unwrap_or(0) + lowering.next;
                    lowering.locals.push(layout.class_ref(created)?);
                    insn.local_set(slot).local_get(slot);
                    for position in 0..arity {
                        insn.local_get(
                            u32::try_from(position + 1).map_err(|_| WasmError::TooLarge)?,
                        );
                    }
                    insn.call(function).local_get(slot);
                } else if arity > 0 {
                    return Err(WasmError::Unsupported(
                        "a constructor reference with no matching constructor",
                    ));
                } else if let Some(initialise) =
                    Self::inherited_initialiser(created, index, layout)?
                {
                    // Declaring no constructor does not mean there is nothing to run: the synthesised one
                    // runs the field initialisers, and it is the same function a plain `new` calls.
                    let slot = u32::try_from(lowering.locals.len()).unwrap_or(0) + lowering.next;
                    lowering.locals.push(layout.class_ref(created)?);
                    insn.local_set(slot).local_get(slot).call(initialise);
                    insn.local_get(slot);
                }
                insn.return_();
                return Ok(Self {
                    locals: lowering.locals,
                    code: insn.into_body(),
                });
            }
            let reference = Facts::of(*input).method_ref(&method.node)?;
            // This backend lowers a plain delegation only: a bound reference captures its receiver
            // and a constructor one needs an allocation, and neither is one.
            let target = reference.target.ok_or(WasmError::Unsupported(
                "a method reference to a constructor",
            ))?;
            let bound = reference.receiver == crate::facts::RefReceiver::Bound;
            let function = *layout.functions.get(&target).ok_or(WasmError::Unsupported(
                "a method reference to a method outside this module",
            ))?;
            let arity = index.member(member).params.len();
            // A *bound* reference's receiver was captured when the object was built, so it comes out of the
            // field rather than off the argument list — and it has to go on first, being the receiver.
            if bound {
                let owner = method
                    .owner
                    .ok_or(WasmError::Unsupported("a bound reference with no owner"))?;
                let field = *layout
                    .capture_slot
                    .get(&owner)
                    .ok_or(WasmError::Unsupported("a bound reference with no capture"))?;
                insn.local_get(0).struct_get(layout.structs[&owner], field);
            }
            // A `static` target takes the arguments alone; an unbound instance one takes the first as its
            // receiver, and forwarding by position already puts it there.
            for position in 0..arity {
                insn.local_get(u32::try_from(position + 1).map_err(|_| WasmError::TooLarge)?);
            }
            insn.call(function).return_();
            return Ok(Self {
                locals: Vec::new(),
                code: insn.into_body(),
            });
        }
        if let Some(member) = method.lambda {
            for param in method
                .node
                .descendants()
                .filter(|node| node.kind() == jals_syntax::SyntaxKind::PARAM)
            {
                let id = lowering
                    .facts()
                    .def_at(&param)
                    .ok_or(WasmError::Unsupported("a lambda parameter with no binding"))?;
                let ty = lowering.layout.val_type(lowering.input.type_of_def(id))?;
                lowering.slots.push((id, lowering.next));
                lowering.next += 1;
                let _ = ty;
            }
            let mut insn = Insn::new();
            let returns = index.resolved_member_ty(member);
            let body = method.node.children().find_map(ast::Block::cast);
            match (
                method
                    .node
                    .children()
                    .filter_map(ast::Expr::cast)
                    .find(|expr| !matches!(expr, ast::Expr::ArrayInit(_))),
                body,
            ) {
                // An expression body *is* the value, or is run for its effect when the interface returns none.
                (Some(value), _) => {
                    if matches!(returns, Ty::Void) {
                        lowering.discard(&value, &mut insn)?;
                    } else {
                        lowering.value_as(&value, &returns, &mut insn)?;
                    }
                    // An expression body leaves the value and writes no `return`, so the instruction is
                    // needed here — unlike a declared method, where a trailing one would be dead code.
                    insn.return_();
                }
                // A block body returns for itself; the trailing trap is the same dead code a declared body's
                // is, and is there so the validator need not infer Java's definite-return rule.
                (None, Some(block)) => {
                    lowering.block(&block, &mut insn)?;
                    if method.result.is_some() {
                        insn.unreachable();
                    }
                }
                (None, None) => return Err(WasmError::Unsupported("a lambda with no body")),
            }
            return Ok(Self {
                locals: lowering.locals,
                code: insn.into_body(),
            });
        }
        if let Some(params) = method.node.children().find_map(ast::ParamList::cast) {
            // A receiver parameter (`void m(Foo this)`) declares no local: JLS §8.4.1 gives it no
            // slot, and the function's own type comes from `resolved_param_tys`, which does not
            // count it either. Declaring one here would put every later parameter one slot past
            // where the signature says it is.
            for param in params.params().filter(|param| !param.is_receiver()) {
                let ty = lowering.declare_param(param.syntax())?;
                let _ = ty;
            }
        }
        // The captures are trailing *parameters*, so their slots follow the declared ones — reserved
        // before any body-local claims them.
        let first_capture = lowering.next;
        lowering.next += u32::try_from(method.captures).unwrap_or(0);

        let mut insn = Insn::new();
        let block = method.node.children().find_map(ast::Block::cast);
        // An instance field initialiser is not a statement anywhere in the source, so a constructor
        // that emitted only its own body left every one of them unrun — a field reading back as its
        // type's default in a module that validates. A `this(…)` delegation is the exception: the
        // constructor it reaches runs them, and running them twice would undo what it did.
        // The synthetic field is written before anything else, so an initialiser or the body can already
        // reach the enclosing instance through it.
        if let (Some(owner), Some(_)) = (method.owner, method.encloses)
            && let Some(&slot) = layout.outer.get(&owner)
        {
            insn.local_get(0)
                .local_get(1)
                .struct_set(layout.structs[&owner], slot);
        }
        // Each capture's parameter goes into its field, before anything else can read it.
        if let Some(owner) = method.owner
            && method.captures > 0
            && let Some(&first_field) = layout.capture_slot.get(&owner)
        {
            for offset in 0..u32::try_from(method.captures).unwrap_or(0) {
                insn.local_get(0)
                    .local_get(first_capture + offset)
                    .struct_set(layout.structs[&owner], first_field + offset);
            }
        }
        if method.is_constructor && !block.as_ref().is_some_and(Self::delegates_to_this) {
            // The implicit `super()`, which the source writes only when it has arguments to pass. It
            // runs the superclass's initialisers, and without it every inherited field read back as its
            // default in a module that validates.
            if let Some(owner) = method.owner
                && !block.as_ref().is_some_and(Self::delegates_to_super)
                && let Some((declaring, function)) = Self::super_constructor(owner, index, layout)
            {
                insn.local_get(0);
                // The superclass may itself be an inner class, whose constructor takes the
                // enclosing instance right after `this`. It is *this* constructor's own — the
                // parameter at slot 1, already written into the synthetic field above — because a
                // subclass of an inner class is enclosed by the same type its superclass is.
                if layout.inner.contains_key(&declaring) {
                    if method.encloses.is_none() {
                        return Err(WasmError::Unsupported(
                            "a subclass of an inner class with no enclosing instance of its own",
                        ));
                    }
                    insn.local_get(1);
                }
                insn.call(function);
            }
            // The constructor's parent *is* the class body, which is where the initialisers are and
            // the reason they need no search: they are this declaration's siblings, in order.
            if let (Some(owner), Some(body)) = (method.owner, method.node.parent()) {
                lowering.initializers(owner, &body, 0, &mut insn)?;
            }
        }
        if let Some(block) = &block {
            lowering.block(block, &mut insn)?;
        }
        // A body that returns on every Java path can still *fall out* of a wasm block: a `br` sitting in
        // unreachable code does not make its target reachable, so the validator sees control reach the
        // end of the function with nothing on the stack. Java's definite-return rule is what makes this
        // dead code; the instruction is here so the validator does not have to infer that.
        if method.result.is_some() {
            insn.unreachable();
        }
        Ok(Self {
            locals: lowering.locals,
            code: insn.into_body(),
        })
    }
}

impl Body {
    /// Whether a constructor body begins with `this(…)` rather than `super(…)` or a statement.
    fn delegates_to_this(block: &ast::Block) -> bool {
        Facts::body_delegates_to(block, jals_syntax::SyntaxKind::THIS_KW)
    }

    /// Whether a constructor body begins with an explicit `super(…)`.
    fn delegates_to_super(block: &ast::Block) -> bool {
        Facts::body_delegates_to(block, jals_syntax::SyntaxKind::SUPER_KW)
    }

    /// The function that runs `owner`'s inherited initialisers when nothing else will: its own
    /// synthesised one, or the nearest ancestor's, which is
    /// [`super_constructor`](Self::super_constructor)'s answer.
    ///
    /// A subclass's construction runs its superclass's field initialisers first (JLS §12.5), and
    /// leaving that out read every inherited field back as its default in a module that validates.
    ///
    /// Every caller has only the receiver to pass, so a constructor that takes an enclosing instance
    /// too is reported rather than called one argument short — and rather than skipped, which would
    /// leave the inherited fields at their defaults in a module that validates.
    fn inherited_initialiser(
        owner: ItemId,
        index: &ProjectIndex,
        layout: &Layout,
    ) -> Result<Option<u32>> {
        if let Some(&function) = layout.default_constructors.get(&owner) {
            return Ok(Some(function));
        }
        match Self::super_constructor(owner, index, layout) {
            Some((declaring, _)) if layout.inner.contains_key(&declaring) => Err(
                WasmError::Unsupported("an inherited initialiser on an inner class"),
            ),
            found => Ok(found.map(|(_, function)| function)),
        }
    }

    /// The function an implicit `super()` calls: the nearest **declared** ancestor with initialisers
    /// to run.
    ///
    /// The walk continues past an ancestor that has no constructor function of its own, because
    /// *its* supertype may still have one — a class with no initialisers is a link in the chain, not
    /// its end. It stops at the first that *declares* one, and answers `None` there when every
    /// declared constructor takes arguments: Java requires an explicit `super(…)` in that case, so
    /// there is nothing implicit to call and the source wrote what to run.
    ///
    /// "Ancestor" is the layout's declared wasm supertype chain, not the index's superclass chain —
    /// see the walk below for why the two are not interchangeable here, and
    /// [`Layout::parent_item`] for what separates them.
    fn super_constructor(
        owner: ItemId,
        index: &ProjectIndex,
        layout: &Layout,
    ) -> Option<(ItemId, u32)> {
        // The *declared wasm* supertype chain ([`Layout::parent_item`]), not the index's superclass
        // chain. The receiver this call passes is `owner`'s struct, so the constructor it reaches
        // has to belong to a type `owner`'s struct is a declared subtype of — and the two chains
        // agree on every well-formed hierarchy and diverge on exactly one input. `class A extends B
        // {}` beside `class B extends A {}` parses and indexes; the layout can declare only one of
        // the two edges, so following the index's chain from the other one found a constructor whose
        // parameter no subtyping relation admits, and the module failed to validate. The layout's
        // chain also needs no cycle guard: each link was reserved strictly earlier than the last.
        //
        // Not a `find_map`. The first ancestor declaring any constructor ends the search **even when
        // it answers `None`** (the doc above says so): a `find_map` would skip that `None` and keep
        // climbing, so `class P { P(int x) {} } class C extends P {}` would call a grandparent's
        // constructor and leave `P`'s fields at their defaults — in a module that validates.
        for item in core::iter::successors(layout.parent_item.get(&owner).copied(), |&item| {
            layout.parent_item.get(&item).copied()
        }) {
            // A linked library's class is where the walk stops, because its own compile already
            // ran its whole chain: each constructor body — declared or the synthesised one that
            // runs the initialisers — was exported under its own key, and the one to call is the
            // same one the library's `new` would have called.
            if layout.external_classes.contains_key(&item) {
                let mut declared = index
                    .own_members(item)
                    .iter()
                    .copied()
                    .filter(|&member| {
                        let info = index.member(member);
                        info.kind == DefKind::Constructor && info.name_range != (0..0)
                    })
                    .peekable();
                if declared.peek().is_some() {
                    return declared
                        .find(|&member| index.member(member).params.is_empty())
                        .and_then(|member| layout.external_initializers.get(&member).copied())
                        .map(|function| (item, function));
                }
                // No declared constructor: the library synthesised one exactly when there was
                // something to run — its own initialisers or an ancestor's — so the answer is that
                // export, and its absence is "nothing to run" rather than "keep climbing". The
                // library's own chain, whatever it held, was compiled into that one function.
                return index
                    .own_members(item)
                    .iter()
                    .copied()
                    .find(|&member| index.member(member).kind == DefKind::Constructor)
                    .and_then(|member| layout.external_initializers.get(&member).copied())
                    .map(|function| (item, function));
            }
            let mut declared = layout.constructors(index, item).peekable();
            if declared.peek().is_some() {
                return declared
                    .find(|&member| index.member(member).params.is_empty())
                    .and_then(|member| layout.functions.get(&member).copied())
                    .map(|function| (item, function));
            }
            if let Some(&function) = layout.default_constructors.get(&item) {
                return Some((item, function));
            }
        }
        None
    }
}

/// The mutable state of lowering one body.
struct Lowering<'a> {
    input: &'a TypedFile<'a>,
    index: &'a ProjectIndex,
    layout: &'a Layout,
    /// Whether this input's statements write their index into the position global.
    ///
    /// True for the project's own inputs, false for the sources a native package publishes — see
    /// [`WasmOptions::positions`], which is the option this field is the per-input half of. The
    /// global has to hold the last statement the *project* entered, or a trap inside a package
    /// would report a line of a file its caller cannot edit.
    positions: bool,
    /// `(definition, local index)` pairs, parameters first.
    slots: Vec<(DefId, u32)>,
    /// Locals beyond the parameters, in declaration order.
    locals: Vec<ValType>,
    /// The next free local index. Unlike the JVM's, a wasm local is one slot whatever its width.
    next: u32,
    owner: Option<ItemId>,
    /// Enclosing `break` / `continue` targets, innermost last.
    loops: Vec<Loop>,
    /// A label read off a `LabeledStmt`, waiting for the loop it labels to claim it.
    pending_label: Option<String>,
    /// Enclosing `finally` blocks, innermost last: a `return` runs every one of them on its way out.
    cleanups: Vec<ast::Block>,
    /// Enclosing `switch` *expressions*, innermost last: where a `yield` branches to, and the type the
    /// value it carries must have.
    yields: Vec<(u32, ValType)>,
    /// The function's declared result, which is what a `return` narrows its value to.
    ///
    /// The same erasure that puts an argument at the top of the reference hierarchy puts a returned
    /// value there: a method declared to return a concrete type may compute one through an
    /// interface, an `Object`, or a type variable, and the signature wants the struct.
    result: Option<ValType>,
}

/// One arm of a lowered `switch`: which keys reach it, in the order the arms are written.
///
/// It carries no entry label, unlike the JVM backend's: an arm's entry is a *position* in the block
/// nesting rather than a name, and the position is the arm's index.
struct Arm {
    /// The `case` keys that reach this arm. Empty for a bare `default`.
    keys: Vec<i32>,
    /// The `case T t` patterns that reach this arm, in the order they are written.
    ///
    /// A pattern is not a constant, so it indexes no `br_table`: a `switch` with one dispatches by
    /// testing each arm's type in source order, which is what §14.11.1 says a pattern `switch` does.
    patterns: Vec<SyntaxNode>,
    /// The arm's `when` clause, which runs after the pattern bound and before the arm is taken.
    guard: Option<ast::Expr>,
    /// Whether one of this arm's labels is `default`.
    is_default: bool,
}

/// One enclosing statement a `break` or a `continue` can name.
///
/// Both depths are `Insn::depth()` values taken just after the structure opened, so a branch is the
/// *difference* against the depth at the branch — the only way to get it right when an `if` may have
/// opened in between.
struct Loop {
    /// The Java label on this statement, if it has one.
    label: Option<String>,
    /// Where a `break` lands: past the whole statement.
    leave: u32,
    /// Where a `continue` lands, or `None` for a labelled statement that is no loop.
    repeat: Option<u32>,
    /// How many `finally` blocks were open when this statement was entered. A jump out of it has to run
    /// every cleanup opened *since* — those are the ones it leaves behind.
    cleanups: usize,
}

impl Lowering<'_> {
    /// A lowering with no receiver, for the start function: it has no `this`, no parameters, and no
    /// enclosing loop, and it carries the locals a previous input's share of the body already used so
    /// two inputs never claim the same index.
    fn for_static<'a>(
        input: &'a TypedFile<'a>,
        index: &'a ProjectIndex,
        layout: &'a Layout,
        locals: Vec<ValType>,
        positions: bool,
    ) -> Lowering<'a> {
        let next = u32::try_from(locals.len()).unwrap_or(u32::MAX);
        Lowering {
            input,
            index,
            layout,
            positions,
            slots: Vec::new(),
            locals,
            next,
            owner: None,
            loops: Vec::new(),
            pending_label: None,
            cleanups: Vec::new(),
            yields: Vec::new(),
            // A class initialiser returns nothing, so there is no `return` value to narrow.
            result: None,
        }
    }

    /// `<global> = <value>` in the start function, with the assignment conversion the constant
    /// expression could not hold.
    fn assign_static(
        &mut self,
        value: &ast::Expr,
        declared: &Ty,
        global: u32,
        insn: &mut Insn,
    ) -> Result<()> {
        self.value_as(value, declared, insn)?;
        insn.global_set(global);
        Ok(())
    }

    /// Build one `enum` constant: allocate it, then run the constructor its arguments select.
    ///
    /// The allocation alone is not the finished object. A constant is the one `new` an `enum` has, and
    /// leaving out the constructor left every field at its default — a module that validates and reads
    /// back zero. Selection is by arity: a constant's argument list is not an expression the index
    /// resolved a call target for, so there is nothing to read a selection out of.
    fn enum_constant(&mut self, owner: ItemId, node: &SyntaxNode, insn: &mut Insn) -> Result<()> {
        let arguments: Vec<ast::Expr> = node
            .children()
            .find_map(ast::ArgList::cast)
            .map(|list| list.args().collect())
            .unwrap_or_default();
        // A constant with a body is an instance of its *own* subclass, which is where its overrides
        // live. Its global still has the enum's type, so nothing else changes.
        let built = if node.children().any(|child| child.kind() == CLASS_BODY) {
            self.index
                .item_by_decl(self.input.file(), usize::from(node.text_range().start()))
                .ok_or(WasmError::Unsupported("an `enum` constant with no item"))?
        } else {
            owner
        };
        let mut matching = self
            .layout
            .constructors(self.index, owner)
            .filter(|&member| self.index.member(member).params.len() == arguments.len());
        let selected = matching.next();
        if selected.is_some() && matching.next().is_some() {
            return Err(WasmError::Unsupported(
                "an `enum` with two constructors of one arity",
            ));
        }
        if selected.is_none() && !arguments.is_empty() {
            return Err(WasmError::Unsupported(
                "an `enum` constant with no matching constructor",
            ));
        }
        let ty = self.layout.class_ref(built)?;
        insn.struct_new_default(self.layout.structs[&built]);
        // The receiver is stored and re-read rather than duplicated, wasm having no `dup`, and the
        // global takes it from there.
        let slot = self.scratch(ty);
        insn.local_set(slot);
        // The enum's own construction: the constructor the arguments selected, or — when the enum
        // declares none — the synthesised one that runs its field initialisers.
        let enum_constructor = match selected {
            Some(constructor) => Some(
                *self
                    .layout
                    .functions
                    .get(&constructor)
                    .ok_or(WasmError::Unsupported("an `enum` constructor with no body"))?,
            ),
            None => self.layout.default_constructors.get(&owner).copied(),
        };
        if let Some(function) = enum_constructor {
            insn.local_get(slot);
            match selected {
                Some(member) => self.push_arguments(member, &arguments, insn)?,
                None => {
                    for argument in &arguments {
                        self.expr(argument, insn)?;
                    }
                }
            }
            insn.call(function);
        }
        // Then the body's *own* initialisers, which belong to a second class and run after the enum's
        // (§12.5). Nothing else reaches them: the enum's constructor knows nothing of the subclass, and
        // running them from the body's synthesised constructor's own `super()` would run the enum's
        // twice — which is why that call is skipped under an `enum`.
        if built != owner
            && let Some(&initialise) = self.layout.default_constructors.get(&built)
        {
            insn.local_get(slot).call(initialise);
        }
        insn.local_get(slot);
        Ok(())
    }

    /// Every instance initialiser the enclosing class declares, in source order.
    ///
    /// Two forms interleave: a field's `= …`, and a bare `{ … }` block. JLS §12.5 runs them in the
    /// order they are *written*, one sequence, before the constructor's own body — which is why they
    /// are emitted from the class body's children here rather than reached through `stmt`. A
    /// `FIELD_DECL` is not a statement, and a `{ … }` in a class body is not the same node as one in a
    /// method.
    fn initializers(
        &mut self,
        owner: ItemId,
        class_body: &SyntaxNode,
        receiver: u32,
        insn: &mut Insn,
    ) -> Result<()> {
        let struct_type =
            *self.layout.structs.get(&owner).ok_or_else(|| {
                WasmError::NoRepresentation(self.index.item(owner).fqn.to_string())
            })?;
        for node in class_body.children() {
            if node.kind() == INITIALIZER {
                // The `static` keyword is inside the `MODIFIERS` child, not on the `INITIALIZER`
                // itself. A `static { … }` runs once at class initialisation rather than per instance,
                // and this backend has no start function to run it in — so it is reported rather than
                // run in every constructor, which would be a different program.
                // A `static { … }` runs once in the module's start function, not per instance.
                if Facts::has_modifier(&node, jals_syntax::SyntaxKind::STATIC_KW) {
                    continue;
                }
                if let Some(block) = node.children().find_map(ast::Block::cast) {
                    self.block(&block, insn)?;
                }
                continue;
            }
            // Each declarator with the value written after its own `=`, which is not the same as
            // pairing names with expressions by index — see `Facts::declarators`.
            for (name, value) in Facts::declarators(&node) {
                let Some(value) = value else {
                    continue;
                };
                let Ok(member) = self.facts().member_at(&name) else {
                    continue;
                };
                if self.index.member(member).modifiers.is_static {
                    continue;
                }
                let Some(slot) = self.layout.field_slot(owner, member) else {
                    continue;
                };
                insn.local_get(receiver);
                let declared = self.index.resolved_member_ty(member);
                self.value_as(&value, &declared, insn)?;
                insn.struct_set(struct_type, slot);
            }
        }
        Ok(())
    }

    fn declare_param(&mut self, node: &SyntaxNode) -> Result<ValType> {
        let id = self
            .facts()
            .def_at(node)
            .ok_or(WasmError::Unsupported("an unresolved parameter"))?;
        let ty = self.layout.val_type(self.input.type_of_def(id))?;
        self.slots.push((id, self.next));
        self.next += 1;
        Ok(ty)
    }

    fn declare_local(&mut self, id: DefId) -> Result<u32> {
        let ty = self.layout.val_type(self.input.type_of_def(id))?;
        let slot = self.next;
        self.slots.push((id, slot));
        self.locals.push(ty);
        self.next += 1;
        Ok(slot)
    }

    /// The local `id` is bound to, searched from the most recent binding: a `catch` arm rebinds its
    /// variable once per declared type, and the copy being lowered must see its *own* local.
    fn slot_of(&self, id: DefId) -> Option<u32> {
        self.slots
            .iter()
            .rev()
            .find(|(entry, _)| *entry == id)
            .map(|(_, slot)| *slot)
    }

    /// The source facts of the file being lowered.
    ///
    /// A projection, not a store: [`Facts`] is a `Copy` handle over the same [`TypedFile`] this
    /// lowering already holds. It is where the span keying, the name binding, and the constant
    /// evaluation live, so neither backend spells them itself.
    const fn facts(&self) -> Facts<'_> {
        Facts::of(*self.input)
    }

    fn ty_of(&self, node: &SyntaxNode) -> Result<ValType> {
        let ty = self
            .input
            .type_of_expr(Facts::span(node))
            .ok_or(WasmError::Unsupported(
                "an expression with no inferred type",
            ))?;
        self.layout.val_type(ty)
    }

    // --- statements ---------------------------------------------------------

    /// The byte range a node was written at, without the trivia the parser attached in front of it.
    ///
    /// [`Facts::span`] is the range the inference memo is keyed by — leading trivia included, which
    /// is what the analysis recorded against — while a report should start at the construct rather
    /// than on the blank line above it. The end is the node's own, so a statement keeps its `;`.
    fn written_range(node: &SyntaxNode) -> Range<usize> {
        let span = Facts::span(node);
        let start = node
            .descendants_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| !token.kind().is_trivia())
            .map_or(span.start, |token| usize::from(token.text_range().start()));
        start..span.end
    }

    fn block(&mut self, block: &ast::Block, insn: &mut Insn) -> Result<()> {
        for statement in block.stmts() {
            self.stmt(&statement, insn)?;
        }
        Ok(())
    }

    /// One statement, with the source it is written at attached to any failure inside it.
    ///
    /// The position is attached at this boundary rather than at each error site: a failure inside
    /// an expression already carries the span the innermost expression boundary gave it, and this
    /// keeps that finer one, while a statement that fails on its own — a `switch` with no selector
    /// — is named by its own. See [`WasmError::at`].
    ///
    /// When the compile asked for [`WasmOptions::positions`], the same boundary is where a
    /// *runtime* position is written: the statement's index goes into the module's position global
    /// before its own code runs, so a trap anywhere inside reports the last statement entered. A
    /// loop re-enters its body, which is what makes the reported statement the iteration's.
    ///
    /// Only the project's own inputs write it — see [`Lowering::positions`] — so a statement of a
    /// native package's Java is lowered without a checkpoint even when the module carries them.
    fn stmt(&mut self, statement: &ast::Stmt, insn: &mut Insn) -> Result<()> {
        let file = self.input.file();
        let range = Self::written_range(statement.syntax());
        if self.positions
            && let Some(build) = &self.layout.positions
        {
            let mut statements = build.statements.borrow_mut();
            let index = i32::try_from(statements.len()).map_err(|_| WasmError::TooLarge)?;
            statements.push(Position {
                file: file.0,
                start: u32::try_from(range.start).map_err(|_| WasmError::TooLarge)?,
                end: u32::try_from(range.end).map_err(|_| WasmError::TooLarge)?,
            });
            insn.i32_const(index).global_set(build.global);
        }
        self.statement(statement, insn)
            .map_err(|error| error.at(file, range))
    }

    /// The statement forms, dispatched one per node kind.
    fn statement(&mut self, statement: &ast::Stmt, insn: &mut Insn) -> Result<()> {
        match statement {
            ast::Stmt::Block(block) => self.block(block, insn),
            // `;` has nothing to emit.
            ast::Stmt::Empty(_) => Ok(()),
            ast::Stmt::Assert(statement) => self.assert(statement, insn),
            ast::Stmt::LocalVar(declaration) => self.local(declaration, insn),
            ast::Stmt::Expr(expression) => {
                let Some(value) = expression.expr() else {
                    return Ok(());
                };
                self.discard(&value, insn)
            }
            ast::Stmt::Return(statement) => {
                // The value is computed *first*, then every enclosing `finally` runs, then the frame
                // leaves — which is the order §14.20.2 gives and the reason a cleanup can observe the
                // value's side effects but not change what is returned.
                if let Some(value) = statement.expr() {
                    match self.result {
                        Some(target) => self.expr_as(&value, target, insn)?,
                        None => {
                            self.expr(&value, insn)?;
                        }
                    }
                }
                // A `return` leaves the frame, so it leaves *every* open cleanup behind.
                self.run_cleanups(0, insn)?;
                insn.return_();
                Ok(())
            }
            ast::Stmt::If(statement) => self.conditional(statement, insn),
            ast::Stmt::While(statement) => self.while_loop(statement, insn),
            ast::Stmt::DoWhile(statement) => self.do_while(statement, insn),
            ast::Stmt::For(statement) => self.for_loop(statement, insn),
            ast::Stmt::ForEach(statement) => self.for_each(statement, insn),
            ast::Stmt::Break(statement) => self.leave(statement.label(), false, insn),
            ast::Stmt::Continue(statement) => self.leave(statement.label(), true, insn),
            ast::Stmt::Labeled(statement) => self.labelled(statement, insn),
            // Each of these names itself rather than going through a catch-all, so a report says which
            // construct is missing. All four wait on the same thing: the exception-handling proposal's
            // `tag` section and `try_table`, which `encode.rs` does not write yet. `synchronized` waits
            // on it too — its body is `finally`-protected, and a monitor this host does not have is the
            // smaller half of the problem.
            ast::Stmt::Throw(statement) => self.throw(statement, insn),
            ast::Stmt::Try(statement) => self.try_catch(statement, insn),
            ast::Stmt::Synchronized(statement) => self.synchronized(statement, insn),
            ast::Stmt::Yield(statement) => {
                let value = statement
                    .expr()
                    .ok_or(WasmError::Unsupported("a `yield` with no value"))?;
                let (leave, ty) = *self.yields.last().ok_or(WasmError::Unsupported(
                    "a `yield` outside a `switch` expression",
                ))?;
                self.arm_value(&value, ty, insn)?;
                insn.br(insn.depth() - leave);
                Ok(())
            }
            ast::Stmt::Switch(statement) => {
                let selector = statement
                    .selector()
                    .ok_or(WasmError::Unsupported("a `switch` with no selector"))?;
                let body = statement
                    .body()
                    .ok_or(WasmError::Unsupported("a `switch` with no body"))?;
                self.switch(&selector, &body, None, insn)
            }
        }
    }

    fn local(&mut self, declaration: &ast::LocalVarDecl, insn: &mut Insn) -> Result<()> {
        // Each declarator with the value written after its own `=`. Pairing names with expressions
        // by index — which this did — gave `int a, b = 2;` its `2` on `a` and left `b` holding the
        // local's zero default, which is silent: unlike the JVM's frame, a wasm local is always
        // readable.
        for (name, value) in Facts::declarators(declaration.syntax()) {
            let id = self
                .facts()
                .def_at_token(&name)
                .ok_or_else(|| WasmError::Unresolved(name.text().into()))?;
            let slot = self.declare_local(id)?;
            if let Some(value) = value {
                let declared = self.input.type_of_def(id).clone();
                self.value_as(&value, &declared, insn)?;
                insn.local_set(slot);
            }
        }
        Ok(())
    }

    /// `if` is wasm's own instruction, so the source's nesting is the output's.
    fn conditional(&mut self, statement: &ast::IfStmt, insn: &mut Insn) -> Result<()> {
        let condition = statement
            .condition()
            .ok_or(WasmError::Unsupported("an `if` with no condition"))?;
        let mut branches = statement.branches();
        let then_branch = branches.next();
        let else_branch = branches.next();

        self.condition(&condition, insn)?;
        insn.if_();
        if let Some(then) = then_branch {
            self.stmt(&then, insn)?;
        }
        if let Some(otherwise) = else_branch {
            insn.else_();
            self.stmt(&otherwise, insn)?;
        }
        insn.end();
        Ok(())
    }

    /// `assert cond;` — nothing at all, or a trap when the condition is false.
    ///
    /// Which one is [`WasmOptions::assertions`], and it is a *compile-time* decision because there
    /// is nowhere else to put it. The JVM backend emits the check behind a `$assertionsDisabled`
    /// read, so one class file serves both `-ea` and not; a wasm host has no flag to read, so a
    /// module either checks or it does not. Off is the default, which is what an unflagged JVM
    /// does — and the condition is still parsed, resolved, and linted either way.
    ///
    /// A failure is `unreachable` and not a `throw`. Java's is an `AssertionError`, which nothing
    /// in this module declares and no `catch` here could name — and the whole point of the error
    /// is that ordinary code does not handle it. A trap is the instruction with that property: it
    /// leaves through every `try` in the module, exactly as an `Error` climbs past every
    /// `catch (Exception e)` on a JVM. Throwing on the module's tag would instead offer it to any
    /// `catch` clause whose `ref.test` happened to accept the payload.
    ///
    /// The detail expression of `assert cond : message` is **dropped**, not evaluated. There is no
    /// `String` to build one with and no error object to attach it to, and Java evaluates it only
    /// on the failing path — so what is lost is a side effect in a message on a run that is about
    /// to trap. Said here rather than reported, because refusing `assert x : y` outright would put
    /// a whole suite outside the subset over a diagnostic string.
    fn assert(&mut self, statement: &ast::AssertStmt, insn: &mut Insn) -> Result<()> {
        if !self.layout.assertions {
            return Ok(());
        }
        let condition = statement
            .syntax()
            .children()
            .find_map(ast::Expr::cast)
            .ok_or(WasmError::Unsupported("an `assert` with no condition"))?;
        self.condition(&condition, insn)?;
        insn.if_();
        insn.else_();
        insn.unreachable();
        insn.end();
        Ok(())
    }

    /// `while` is a `block` around a `loop`: leaving branches out of the block, repeating branches to
    /// the loop. The two labels are why wasm needs both instructions — a `loop` alone can only jump
    /// backwards, and a `block` alone only forwards.
    ///
    /// A `continue` re-tests the condition, so the loop *is* its target here; the other two loop forms
    /// need a third structure because their continuation point is not the top.
    fn while_loop(&mut self, statement: &ast::WhileStmt, insn: &mut Insn) -> Result<()> {
        let condition = statement
            .condition()
            .ok_or(WasmError::Unsupported("a `while` with no condition"))?;
        let label = self.pending_label.take();
        insn.block();
        let leave = insn.depth();
        insn.loop_();
        let repeat = insn.depth();
        self.condition(&condition, insn)?;
        insn.i32_eqz();
        insn.br_if(insn.depth() - leave);
        let cleanups = self.cleanups.len();
        self.loops.push(Loop {
            label,
            leave,
            repeat: Some(repeat),
            cleanups,
        });
        if let Some(body) = statement.body() {
            self.stmt(&body, insn)?;
        }
        self.loops.pop();
        insn.br(insn.depth() - repeat).end().end();
        Ok(())
    }

    /// `do body while (cond)`.
    ///
    /// Three structures, not two: a `continue` reaches the *bottom* test, which is neither the top of
    /// the loop nor past the end of it. The inner block is that point.
    fn do_while(&mut self, statement: &ast::DoWhileStmt, insn: &mut Insn) -> Result<()> {
        let condition = statement
            .condition()
            .ok_or(WasmError::Unsupported("a `do` with no condition"))?;
        let label = self.pending_label.take();
        insn.block();
        let leave = insn.depth();
        insn.loop_();
        let repeat = insn.depth();
        insn.block();
        let next = insn.depth();
        let cleanups = self.cleanups.len();
        self.loops.push(Loop {
            label,
            leave,
            repeat: Some(next),
            cleanups,
        });
        if let Some(body) = statement.body() {
            self.stmt(&body, insn)?;
        }
        self.loops.pop();
        insn.end();
        self.condition(&condition, insn)?;
        insn.br_if(insn.depth() - repeat).end().end();
        Ok(())
    }

    /// `for (init; condition; update) body`.
    ///
    /// A `continue` runs the update before re-testing (JLS §14.14.1.3), so the update sits *between*
    /// the inner block's end and the branch back — which is exactly what makes the inner block the
    /// continue target rather than the loop.
    fn for_loop(&mut self, statement: &ast::ForStmt, insn: &mut Insn) -> Result<()> {
        let label = self.pending_label.take();
        for node in statement.init() {
            self.for_section(&node, insn)?;
        }
        insn.block();
        let leave = insn.depth();
        insn.loop_();
        let repeat = insn.depth();
        // No condition means `for (;;)`, which never leaves by itself.
        if let Some(condition) = statement.condition() {
            self.condition(&condition, insn)?;
            insn.i32_eqz();
            insn.br_if(insn.depth() - leave);
        }
        insn.block();
        let next = insn.depth();
        let cleanups = self.cleanups.len();
        self.loops.push(Loop {
            label,
            leave,
            repeat: Some(next),
            cleanups,
        });
        if let Some(body) = statement.body() {
            self.stmt(&body, insn)?;
        }
        self.loops.pop();
        insn.end();
        for node in statement.update() {
            self.for_section(&node, insn)?;
        }
        insn.br(insn.depth() - repeat).end().end();
        Ok(())
    }

    /// One node of a `for` header's initialiser or update list: a declaration, or an expression run for
    /// its effect.
    fn for_section(&mut self, node: &SyntaxNode, insn: &mut Insn) -> Result<()> {
        if let Some(declaration) = ast::LocalVarDecl::cast(node.clone()) {
            return self.local(&declaration, insn);
        }
        let expression =
            ast::Expr::cast(node.clone()).ok_or(WasmError::Unsupported("this `for` header"))?;
        self.discard(&expression, insn)
    }

    /// `for (T v : iterable) body`: an indexed loop over an array, a protocol over an `Iterable`.
    ///
    /// JLS §14.14.2 defines both, and the array one is that definition: the array and the index live
    /// in scratch locals so neither the iterable expression nor `array.len` is re-evaluated per
    /// step. An `Iterable` runs on its `iterator`, which only a linked `java.lang.Iterable` supplies
    /// — see [`for_each_iterator`](Self::for_each_iterator).
    fn for_each(&mut self, statement: &ast::ForEachStmt, insn: &mut Insn) -> Result<()> {
        let iterable = statement
            .iterable()
            .ok_or(WasmError::Unsupported("a `for`-each over nothing"))?;
        let name: SyntaxToken = statement
            .name_token()
            .ok_or(WasmError::Unsupported("a `for`-each with no variable"))?;
        let Some(ty) = self
            .input
            .type_of_expr(Facts::span(iterable.syntax()))
            .cloned()
        else {
            return Err(WasmError::Unsupported(
                "a `for`-each over a value with no type",
            ));
        };
        let Ty::Array(element) = ty else {
            return self.for_each_iterator(statement, &iterable, &name, insn);
        };
        let element = self.layout.val_type(&element)?;
        let array_type = self
            .layout
            .array_type(element)
            .ok_or_else(|| WasmError::NoRepresentation("an array".to_owned()))?;
        let label = self.pending_label.take();

        let array_ty = self
            .expr(&iterable, insn)?
            .ok_or(WasmError::Unsupported("a `for`-each over no value"))?;
        let array = self.scratch(array_ty);
        insn.local_set(array);
        let index = self.scratch(ValType::I32);
        insn.i32_const(0).local_set(index);

        let id = self
            .facts()
            .def_at_token(&name)
            .ok_or_else(|| WasmError::Unresolved(name.text().into()))?;
        let variable = self.declare_local(id)?;

        insn.block();
        let leave = insn.depth();
        insn.loop_();
        let repeat = insn.depth();
        // `index < array.length`, then leave when it is not.
        insn.local_get(index)
            .local_get(array)
            .array_len()
            .numeric(NumOp::Lt, ValType::I32)
            .ok_or(WasmError::Unsupported("an array length comparison"))?;
        insn.i32_eqz();
        insn.br_if(insn.depth() - leave);
        // The variable is bound before the continue target, so a `continue` cannot skip the binding.
        insn.local_get(array)
            .local_get(index)
            .array_get(array_type)
            .local_set(variable);
        insn.block();
        let next = insn.depth();
        let cleanups = self.cleanups.len();
        self.loops.push(Loop {
            label,
            leave,
            repeat: Some(next),
            cleanups,
        });
        if let Some(body) = statement.body() {
            self.stmt(&body, insn)?;
        }
        self.loops.pop();
        insn.end();
        insn.local_get(index)
            .i32_const(1)
            .numeric(NumOp::Add, ValType::I32)
            .ok_or(WasmError::Unsupported("an index increment"))?;
        insn.local_set(index);
        insn.br(insn.depth() - repeat).end().end();
        Ok(())
    }

    /// `for (T v : iterable) body` over something that is not an array, which JLS §14.14.2 defines
    /// as a loop over `iterable.iterator()`.
    ///
    /// The three calls are named on the *interfaces* that declare them rather than on the receiver's
    /// own type. That resolves for any receiver assignable to `Iterable`, which is exactly the
    /// condition checked before emitting — and it means the override chain is the one the interface's
    /// member carries, so a single pair of functions serves every collection instead of one pair per
    /// static type. `iterator()` and `next()` are interface methods, so both sides hold them at
    /// `anyref`; `next()` erases to `Object`, which is what [`bind_element`](Self::bind_element)
    /// brings down to the type the variable declares.
    fn for_each_iterator(
        &mut self,
        statement: &ast::ForEachStmt,
        iterable: &ast::Expr,
        name: &SyntaxToken,
        insn: &mut Insn,
    ) -> Result<()> {
        const ITERABLE: &str = "java.lang.Iterable";
        const ITERATOR: &str = "java.util.Iterator";
        // Only a type whose members the index holds can be checked for `iterator()`, and a receiver
        // that has none is a program the linter reports rather than one to emit for.
        let iterable_ty = self
            .input
            .type_of_expr(Facts::span(iterable.syntax()))
            .cloned();
        let Some(Ty::Class(ClassTy::Project { id, args, .. })) = iterable_ty else {
            return Err(WasmError::Unsupported("a `for`-each over this type"));
        };
        if self
            .index
            .resolve_member(id, "iterator", Namespace::Method)
            .is_none()
        {
            return Err(WasmError::Unsupported("a `for`-each over a non-`Iterable`"));
        }
        let iterable_iface = self
            .index
            .item_by_fqn(ITERABLE)
            .ok_or_else(|| WasmError::NoRepresentation(ITERABLE.to_owned()))?;
        // The element comes from the `Iterable` instantiation, not from the receiver's own
        // parameters: a non-generic `class Evens implements Iterable<Integer>` has none, and a
        // `Pair<A, B> implements Iterable<B>` would name the wrong one.
        let element = self
            .index
            .substitution_to(id, &args, iterable_iface)
            .and_then(|args| args.into_iter().next());
        let iterator_iface = self
            .index
            .item_by_fqn(ITERATOR)
            .ok_or_else(|| WasmError::NoRepresentation(ITERATOR.to_owned()))?;
        let iterator_method = self
            .method_matching(iterable_iface, "iterator", &[], false)
            .ok_or_else(|| WasmError::Unresolved(alloc::format!("{ITERABLE}.iterator")))?;
        let has_next = self
            .method_matching(iterator_iface, "hasNext", &[], false)
            .ok_or_else(|| WasmError::Unresolved(alloc::format!("{ITERATOR}.hasNext")))?;
        let next = self
            .method_matching(iterator_iface, "next", &[], false)
            .ok_or_else(|| WasmError::Unresolved(alloc::format!("{ITERATOR}.next")))?;
        // An interface erases to `anyref`, so that is what the iterator, and the elements it hands
        // out, are held at.
        let iterator_ty = self.layout.class_ref(iterator_iface)?;

        let label = self.pending_label.take();
        let receiver_ty = self
            .expr(iterable, insn)?
            .ok_or(WasmError::Unsupported("a `for`-each over no value"))?;
        let receiver = self.scratch(receiver_ty);
        insn.local_set(receiver);
        self.dispatch_to(
            receiver,
            receiver_ty,
            iterator_method,
            Some(iterator_ty),
            insn,
        )?;
        let iterator = self.scratch(iterator_ty);
        insn.local_set(iterator);

        insn.block();
        let leave = insn.depth();
        insn.loop_();
        let repeat = insn.depth();
        // `it.hasNext()`, then leave when it is not.
        insn.local_get(iterator);
        self.dispatch_to(iterator, iterator_ty, has_next, Some(ValType::I32), insn)?;
        insn.i32_eqz();
        insn.br_if(insn.depth() - leave);
        // The variable is bound before the body, and `continue` targets the test rather than an
        // update, so it is re-made on the way back rather than skipped.
        let id = self
            .facts()
            .def_at_token(name)
            .ok_or_else(|| WasmError::Unresolved(name.text().into()))?;
        let variable = self.declare_local(id)?;
        let declared = self.input.type_of_def(id).clone();
        insn.local_get(iterator);
        self.dispatch_to(iterator, iterator_ty, next, Some(iterator_ty), insn)?;
        self.bind_element(element.as_ref(), &declared, insn)?;
        insn.local_set(variable);
        let cleanups = self.cleanups.len();
        self.loops.push(Loop {
            label,
            leave,
            repeat: Some(repeat),
            cleanups,
        });
        if let Some(body) = statement.body() {
            self.stmt(&body, insn)?;
        }
        self.loops.pop();
        insn.br(insn.depth() - repeat).end().end();
        Ok(())
    }

    /// Bring the element `iterator.next()` left on the stack down to the type the loop variable
    /// declares.
    ///
    /// `next()` erases to `Object`, so a reference variable needs the same `ref.cast` erasure asks
    /// for anywhere else. A *primitive* variable names its wrapper by the iterable's own type
    /// argument — `for (int n : List<Integer>)` is an `intValue()` — which is why the element type
    /// travels here; the declared type alone could not say which class to unbox.
    fn bind_element(&self, element: Option<&Ty>, declared: &Ty, insn: &mut Insn) -> Result<()> {
        let erased = ValType::Ref(RefType::nullable(HeapType::Any));
        let target = self.layout.val_type(declared)?;
        match Self::numeric(target) {
            Some(target) => {
                let element = element.ok_or(WasmError::Unsupported(
                    "a `for`-each over an element of no type",
                ))?;
                let unboxed = self.unbox_ty(element, erased, insn)?;
                if unboxed == target {
                    return Ok(());
                }
                Self::widen(unboxed, target, insn)
            }
            None => self.narrow(erased, target, insn),
        }
    }

    /// `label: statement`.
    ///
    /// A labelled *loop* takes the label into its own entry, so `continue label` reaches its update
    /// rather than merely leaving it. Anything else gets a block of its own, which only `break label`
    /// can target — the label of a non-loop is a forward jump and nothing else (JLS §14.7).
    fn labelled(&mut self, statement: &ast::LabeledStmt, insn: &mut Insn) -> Result<()> {
        let label = statement
            .label()
            .ok_or(WasmError::Unsupported("a label with no name"))?;
        let inner = statement
            .stmt()
            .ok_or(WasmError::Unsupported("a label with no statement"))?;
        if matches!(
            inner,
            ast::Stmt::While(_) | ast::Stmt::DoWhile(_) | ast::Stmt::For(_) | ast::Stmt::ForEach(_)
        ) {
            self.pending_label = Some(label);
            return self.stmt(&inner, insn);
        }
        insn.block();
        let leave = insn.depth();
        self.loops.push(Loop {
            label: Some(label),
            leave,
            repeat: None,
            cleanups: self.cleanups.len(),
        });
        let lowered = self.stmt(&inner, insn);
        self.loops.pop();
        lowered?;
        insn.end();
        Ok(())
    }

    /// `synchronized (lock) { … }`.
    ///
    /// There is no monitor on this host: a wasm module here is single-threaded, so there is nothing for
    /// a lock to exclude and nothing for a `finally` to release. What remains of the statement is its
    /// two observable effects — the lock expression is evaluated, and a `null` one fails. It *traps*
    /// rather than throwing a `NullPointerException`, which is the same trade this backend already makes
    /// for a failed `ref.cast` on a host with no exception model to throw into.
    fn synchronized(&mut self, statement: &ast::SynchronizedStmt, insn: &mut Insn) -> Result<()> {
        let lock = statement
            .syntax()
            .children()
            .find_map(ast::Expr::cast)
            .ok_or(WasmError::Unsupported("a `synchronized` with no lock"))?;
        let body = statement
            .syntax()
            .children()
            .find_map(ast::Block::cast)
            .ok_or(WasmError::Unsupported("a `synchronized` with no body"))?;
        self.expr(&lock, insn)?
            .ok_or(WasmError::Unsupported("a `synchronized` on no value"))?;
        insn.ref_as_non_null().drop();
        self.block(&body, insn)
    }

    /// `throw e`.
    ///
    /// One tag carries every Java exception, because every one of them is a reference: what a `catch`
    /// tests is the *class* of the payload, not which tag raised it.
    fn throw(&mut self, statement: &ast::ThrowStmt, insn: &mut Insn) -> Result<()> {
        let value = statement
            .expr()
            .ok_or(WasmError::Unsupported("a `throw` with nothing to throw"))?;
        let tag = self
            .layout
            .tag
            .ok_or(WasmError::Unsupported("a `throw` with no tag declared"))?;
        self.expr(&value, insn)?
            .ok_or(WasmError::Unsupported("a `throw` of no value"))?;
        insn.throw(tag);
        Ok(())
    }

    /// `try { … } catch (T v) { … }`, with as many handlers as the source wrote.
    ///
    /// `try_table` delivers the payload to *one* label, so the class tests happen after it rather than
    /// in it: the caught reference is spilled into a local and each handler is a `ref.test` against its
    /// declared type, in source order (§14.20 — the first matching clause wins). A payload no clause
    /// accepts is re-thrown, which is what makes an unhandled exception leave the frame rather than
    /// being swallowed.
    ///
    /// `finally` and try-with-resources are reported: both need their block duplicated onto every exit
    /// path, including the branch out of the `try` and the re-throw, and neither is emitted here yet.
    fn try_catch(&mut self, statement: &ast::TryStmt, insn: &mut Insn) -> Result<()> {
        use jals_syntax::SyntaxKind::{FINALLY_CLAUSE, RESOURCE_LIST};
        let finally = statement
            .syntax()
            .children()
            .find(|child| child.kind() == FINALLY_CLAUSE)
            .and_then(|clause| clause.children().find_map(ast::Block::cast));
        // A resource is declared, used, and closed: the declaration becomes a local here and the close
        // becomes part of the cleanup, so the rest of this function needs to know only that there is one.
        let resources: Vec<ast::Resource> = statement
            .syntax()
            .children()
            .filter(|child| child.kind() == RESOURCE_LIST)
            .flat_map(|list| list.children().filter_map(ast::Resource::cast))
            .collect();
        let clauses: Vec<ast::CatchClause> = statement
            .syntax()
            .children()
            .filter_map(ast::CatchClause::cast)
            .collect();
        // Resources alone are the whole statement. With a `catch` or a `finally` beside them, §14.20.3
        // makes the resource `try` the *body* of an ordinary one — so the outer structure below wraps it
        // rather than duplicating the close sequence into every handler.
        if !resources.is_empty() && clauses.is_empty() && finally.is_none() {
            return self.try_resources(statement, &resources, insn);
        }
        let tag = self
            .layout
            .tag
            .ok_or(WasmError::Unsupported("a `try` with no tag declared"))?;
        let body = statement
            .syntax()
            .children()
            .find_map(ast::Block::cast)
            .ok_or(WasmError::Unsupported("a `try` with no body"))?;
        if clauses.is_empty() && finally.is_none() {
            return Err(WasmError::Unsupported("a `try` with no handler"));
        }
        // A `finally` has to run on *every* way out, and a `return` / `break` / `continue` inside the
        // protected code is a way out this lowering does not intercept — it would branch straight past
        // the block the cleanup sits after. Reported rather than emitted with the cleanup skipped, which
        // would be a silent one.

        if let Some(cleanup) = &finally {
            self.cleanups.push(cleanup.clone());
        }
        insn.block();
        let out = insn.depth();
        insn.block_typed(ValType::Ref(RefType::nullable(HeapType::Any)));
        let handler = insn.depth();
        insn.try_table(&[(tag, insn.depth() - handler)]);
        if resources.is_empty() {
            self.block(&body, insn)?;
        } else {
            self.try_resources(statement, &resources, insn)?;
        }
        insn.end();
        // The body completed, so nothing was caught: leave past every handler.
        insn.br(insn.depth() - out);
        insn.end();

        // The caught reference, which each clause narrows in turn.
        let caught = self.scratch(ValType::Ref(RefType::nullable(HeapType::Any)));
        insn.local_set(caught);
        for clause in &clauses {
            self.catch_clause(clause, caught, out, insn)?;
        }
        // Popped before the cleanup's own copies are emitted: a `return` inside the `finally` must not
        // run the `finally` again.
        if finally.is_some() {
            self.cleanups.pop();
        }
        // Nothing matched: run the cleanup and re-throw, so the exception leaves this frame rather than
        // vanishing. This is the copy of `finally` the *exceptional* path needs; the normal path gets
        // its own below, which is the duplication a structured cleanup costs.
        if let Some(cleanup) = &finally {
            self.block(cleanup, insn)?;
        }
        insn.local_get(caught);
        insn.throw(tag);
        insn.end();
        if let Some(cleanup) = &finally {
            self.block(cleanup, insn)?;
        }
        Ok(())
    }

    /// `try (R r = …) { … }`.
    ///
    /// §14.20.3 closes each resource in reverse declaration order, on both the normal and the exceptional
    /// path, skipping a `null` one. What this does *not* do is record a suppressed exception: a `close()`
    /// that throws while the body is already throwing is swallowed, because `Throwable.addSuppressed`
    /// needs a type with no wasm representation. The *primary* exception is still the body's, which is the
    /// one Java propagates and the one a `catch` sees — so the control flow is right and only the
    /// suppressed list is missing.
    ///
    /// A `catch` or a `finally` beside the resources is not this function's business: §14.20.3 makes the
    /// resource `try` the *body* of an ordinary one, so [`try_catch`](Self::try_catch) wraps this.
    fn try_resources(
        &mut self,
        statement: &ast::TryStmt,
        resources: &[ast::Resource],
        insn: &mut Insn,
    ) -> Result<()> {
        let tag = self
            .layout
            .tag
            .ok_or(WasmError::Unsupported("a `try` with no tag declared"))?;
        let body = statement
            .syntax()
            .children()
            .find_map(ast::Block::cast)
            .ok_or(WasmError::Unsupported("a `try` with no body"))?;

        // Each resource is a local of its declared type, initialised in order.
        let mut slots = Vec::with_capacity(resources.len());
        for resource in resources {
            let name = resource
                .binding()
                .ok_or(WasmError::Unsupported("a resource with no name"))?;
            let value = resource
                .syntax()
                .children()
                .find_map(ast::Expr::cast)
                .ok_or(WasmError::Unsupported("a resource with no initialiser"))?;
            let id = self
                .facts()
                .def_at_token(&name)
                .ok_or_else(|| WasmError::Unresolved(name.text().into()))?;
            let slot = self.declare_local(id)?;
            let declared = self.input.type_of_def(id).clone();
            self.value_as(&value, &declared, insn)?;
            insn.local_set(slot);
            slots.push((slot, declared));
        }

        insn.block();
        let out = insn.depth();
        insn.block_typed(ValType::Ref(RefType::nullable(HeapType::Any)));
        let handler = insn.depth();
        insn.try_table(&[(tag, insn.depth() - handler)]);
        self.block(&body, insn)?;
        insn.end();
        // The body completed: close normally, so a `close()` that throws propagates.
        self.close_resources(&slots, insn)?;
        insn.br(insn.depth() - out);
        insn.end();

        let caught = self.scratch(ValType::Ref(RefType::nullable(HeapType::Any)));
        insn.local_set(caught);
        insn.block();
        let closed = insn.depth();
        insn.block_typed(ValType::Ref(RefType::nullable(HeapType::Any)));
        let threw = insn.depth();
        insn.try_table(&[(tag, insn.depth() - threw)]);
        self.close_resources(&slots, insn)?;
        insn.br(insn.depth() - closed);
        insn.end();
        // Only reachable if `close()` completed without branching, which it cannot.
        insn.unreachable();
        insn.end();
        // The suppressed exception, dropped rather than attached: see this function's own note.
        insn.drop();
        insn.end();
        insn.local_get(caught);
        insn.throw(tag);
        insn.end();
        Ok(())
    }

    /// Call a no-argument `void` method on a receiver already in a local, on its *runtime* type.
    ///
    /// The same `ref.test` chain a call site builds, without the call site: the overrides are a known,
    /// closed set with the whole project in one module, so testing them most-derived first answers what a
    /// vtable would. Used where there is no call expression to read a receiver out of — a resource's
    /// `close`, which §14.20.3 calls and the source never writes, and the `iterator`, `hasNext`, and
    /// `next` a `for`-each runs on. `result` is the type the call leaves, `None` for a `void` one,
    /// because the chain's block has to declare it.
    fn dispatch_to(
        &self,
        receiver: u32,
        receiver_ty: ValType,
        member: MemberId,
        result: Option<ValType>,
        insn: &mut Insn,
    ) -> Result<()> {
        let info = self.index.member(member);
        let owner = info.owner;
        let overriders = self.overriders(member);
        let fallback = self.layout.functions.get(&member).copied();
        // The realm arm, on the same terms a written call takes it: a consumer class that extends one
        // of this library's classes is a subtype of whatever the chain below tests for, so a
        // chain-first call would answer with the library's body and never ask the consumer.
        let dispatched = self.layout.realm.is_some()
            && !info.modifiers.is_static
            && !info.modifiers.is_private
            && info.kind == DefKind::Method
            && !self.layout.imported(member)
            && self.layout.member_types.contains_key(&member);
        let realm = if dispatched {
            Some(self.realm_slot(member)?)
        } else {
            None
        };
        if realm.is_none() && overriders.is_empty() {
            let function = fallback.ok_or(WasmError::Unsupported("a method with no body"))?;
            insn.local_get(receiver);
            self.narrow(receiver_ty, self.layout.class_ref(owner)?, insn)?;
            insn.call(function);
            return Ok(());
        }
        if let Some((field, slot_ty)) = realm {
            let build = self
                .layout
                .realm
                .as_ref()
                .ok_or(WasmError::Unsupported("a dispatch with no realm"))?;
            let (structure, global) = (build.structure, build.global);
            insn.global_get(global).ref_is_null().i32_eqz();
            match result {
                Some(ty) => insn.if_typed(ty),
                None => insn.if_(),
            };
            insn.local_get(receiver);
            // The slot is called at the declared member's type, so the receiver comes up to it from
            // wherever the expression left it.
            let slot_receiver = self.slot_receiver(member);
            self.narrow(receiver_ty, slot_receiver, insn)?;
            insn.global_get(global)
                .struct_get(structure, field)
                .call_ref(slot_ty);
            insn.else_();
        }
        match result {
            Some(ty) => insn.block_typed(ty),
            None => insn.block(),
        };
        let leave = insn.depth();
        for &(item, over) in &overriders {
            let Some(&function) = self.layout.functions.get(&over) else {
                continue;
            };
            let struct_type = self.layout.structs[&item];
            insn.local_get(receiver);
            insn.ref_test(HeapType::Concrete(struct_type), false);
            insn.if_();
            insn.local_get(receiver);
            insn.ref_cast(HeapType::Concrete(struct_type), false);
            insn.call(function);
            insn.br(insn.depth() - leave);
            insn.end();
        }
        // An interface's method is abstract: every class that could satisfy the call is already in the
        // chain, so reaching here means a receiver of a type nothing implemented, which traps.
        match fallback {
            Some(function) => {
                insn.local_get(receiver);
                self.narrow(receiver_ty, self.layout.class_ref(owner)?, insn)?;
                insn.call(function);
            }
            None => {
                insn.unreachable();
            }
        }
        insn.end();
        if realm.is_some() {
            insn.end();
        }
        Ok(())
    }

    /// `if (r != null) r.close();` for each resource, in reverse declaration order.
    fn close_resources(&self, slots: &[(u32, Ty)], insn: &mut Insn) -> Result<()> {
        for (slot, declared) in slots.iter().rev() {
            let item = declared
                .project_id()
                .ok_or(WasmError::Unsupported("a resource of an unindexed type"))?;
            let close = self
                .index
                .members_of(item)
                .into_iter()
                .find(|&id| {
                    let member = self.index.member(id);
                    member.kind == DefKind::Method
                        && member.name == "close"
                        && member.params.is_empty()
                })
                .ok_or(WasmError::Unsupported("a resource with no `close`"))?;
            // `r != null`, which §14.20.3 checks before closing.
            insn.local_get(*slot);
            insn.ref_is_null();
            insn.i32_eqz();
            insn.if_();
            // The runtime type decides which `close` runs, exactly as it does at a call site: the
            // declared type is what named the method, and a subclass may have overridden it.
            self.dispatch_to(*slot, self.layout.val_type(declared)?, close, None, insn)?;
            insn.end();
        }
        Ok(())
    }

    /// One `catch (T v) { … }`: test the payload's class, bind it, run the block, and leave.
    ///
    /// A multi-catch (`catch (A | B v)`) is several tests reaching one block, which is why the types
    /// are a list here rather than one.
    fn catch_clause(
        &mut self,
        clause: &ast::CatchClause,
        caught: u32,
        out: u32,
        insn: &mut Insn,
    ) -> Result<()> {
        let types: Vec<ast::Type> = clause
            .syntax()
            .descendants()
            .filter_map(ast::Type::cast)
            .collect();
        if types.is_empty() {
            return Err(WasmError::Unsupported("a `catch` with no type"));
        }
        let name = clause
            .binding()
            .ok_or(WasmError::Unsupported("a `catch` with no variable"))?;
        let body = clause
            .syntax()
            .children()
            .find_map(ast::Block::cast)
            .ok_or(WasmError::Unsupported("a `catch` with no body"))?;

        // A multi-catch is lowered as one arm *per declared type* rather than one arm testing several.
        // The variable's type is the least upper bound of the declared types, so any member the source
        // can legally reach through it is declared on that bound — and a struct's fields start with its
        // supertype's, so the slot is the same in every one of them. Narrowing to the concrete type per
        // copy is therefore sound, and it is the only way to give the variable a wasm type at all: there
        // is no struct type for a bound this backend does not compute.
        let id = self
            .facts()
            .def_at_token(&name)
            .ok_or_else(|| WasmError::Unresolved(name.text().into()))?;
        for ty in &types {
            let heap = self.named_type(ty)?;
            insn.local_get(caught);
            insn.ref_test(heap, false);
            insn.if_();
            let declared = self.ty_of_type(ty)?;
            let slot = self.scratch(declared);
            insn.local_get(caught);
            insn.ref_cast(heap, true);
            insn.local_set(slot);
            self.slots.push((id, slot));
            let lowered = self.block(&body, insn);
            self.slots.pop();
            lowered?;
            insn.br(insn.depth() - out);
            insn.end();
        }
        Ok(())
    }

    /// The wasm type a written `TYPE` node names, for a binding the analysis records no type for.
    fn ty_of_type(&self, ty: &ast::Type) -> Result<ValType> {
        let HeapType::Concrete(index) = self.named_type(ty)? else {
            return Err(WasmError::Unsupported("a `catch` type with no struct type"));
        };
        Ok(ValType::Ref(RefType::nullable(HeapType::Concrete(index))))
    }

    /// `switch (selector) { … }`, statement or expression.
    ///
    /// The shape is one `block` per arm, nested inside a `block` for the whole `switch`: a `br i` from
    /// the dispatch lands just past arm `i`'s block, which is where arm `i`'s body starts. Falling out
    /// of arm `i`'s block end then runs arm `i+1` — so the colon form's fallthrough is what the nesting
    /// *already does*, with no branch of its own.
    fn switch(
        &mut self,
        selector: &ast::Expr,
        body: &ast::SwitchBlock,
        result: Option<ValType>,
        insn: &mut Insn,
    ) -> Result<()> {
        let rules: Vec<ast::SwitchRule> = body.rules().collect();
        let groups: Vec<ast::SwitchGroup> = body.groups().collect();
        if !rules.is_empty() && !groups.is_empty() {
            // JLS §14.11.1 forbids mixing them, so this is not a program.
            return Err(WasmError::Unsupported("a `switch` mixing both forms"));
        }
        let arms: Vec<Arm> = if rules.is_empty() {
            groups
                .iter()
                .map(|group| self.arm(group.labels()))
                .collect::<Result<_>>()?
        } else {
            rules
                .iter()
                .map(|rule| self.arm(rule.label().into_iter()))
                .collect::<Result<_>>()?
        };
        let count = u32::try_from(arms.len()).map_err(|_| WasmError::TooLarge)?;
        // An unmatched key has to reach *some* label, and for an expression that label cannot be the
        // end of the block: the block owes a value. Exhaustiveness over an `enum` is the other way to
        // satisfy §14.11.2 and is not lowered, so a `default` is required rather than assumed.
        let default = arms.iter().position(|arm| arm.is_default);
        if result.is_some() && default.is_none() {
            return Err(WasmError::Unsupported(
                "a `switch` expression with no `default`",
            ));
        }
        let fallback = default.map_or(count, |index| u32::try_from(index).unwrap_or(count));
        let label = self.pending_label.take();

        match result {
            Some(ty) => insn.block_typed(ty),
            None => insn.block(),
        };
        let leave = insn.depth();
        for _ in 0..count {
            insn.block();
        }
        self.dispatch(selector, &arms, fallback, insn)?;

        let cleanups = self.cleanups.len();
        self.loops.push(Loop {
            label,
            leave,
            repeat: None,
            cleanups,
        });
        if let Some(ty) = result {
            self.yields.push((leave, ty));
        }
        let lowered = if rules.is_empty() {
            self.switch_groups(&groups, result, insn)
        } else {
            self.switch_rules(&rules, result, leave, insn)
        };
        if result.is_some() {
            self.yields.pop();
        }
        self.loops.pop();
        lowered?;
        // A colon-form arm leaves by `yield`, so the last group's end carries no value. Java's own rule
        // is that every arm yields or throws; the instruction is here so the validator does not have to
        // infer that rule, exactly as a value-returning body's trailing one is.
        if result.is_some() {
            insn.unreachable();
        }
        insn.end();
        Ok(())
    }

    /// One arm's `case` keys and patterns. `default` contributes neither.
    fn arm(&self, labels: impl Iterator<Item = ast::SwitchLabel>) -> Result<Arm> {
        let ArmLabels {
            keys,
            patterns,
            guard,
            is_default,
        } = self.facts().switch_arm(labels)?;
        // A `String` key is a fact the source states and a value this lowering does not dispatch
        // on: with the platform linked a `String` *is* representable, but the switch would need a
        // `hashCode`/`equals` pair of calls and a table shape this backend has not written. Refused
        // as the construct it is, rather than as a type it is not.
        let keys = keys
            .into_iter()
            .map(|key| {
                key.as_int()
                    .ok_or(WasmError::Unsupported("a `String` `case` label"))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Arm {
            keys,
            patterns,
            guard,
            is_default,
        })
    }

    /// A pattern `switch`: each arm's type is tested in source order, and the first match wins.
    ///
    /// No `br_table`, because a pattern is not a constant and there is nothing to index on. §14.11.1
    /// gives the first *matching* label, so the tests are emitted in the order they are written and a
    /// `default` is only reached by falling out of all of them — which is what `fallback` already is.
    /// The binding is stored inside the test that matched; a wasm local starts at its type's default,
    /// so the other arms need nothing.
    fn dispatch_patterns(
        &mut self,
        selector: &ast::Expr,
        arms: &[Arm],
        fallback: u32,
        insn: &mut Insn,
    ) -> Result<()> {
        // A constant beside a pattern would need the jump table this does not build.
        if arms.iter().any(|arm| !arm.keys.is_empty()) {
            return Err(WasmError::Unsupported("a `switch` mixing key types"));
        }
        let selector_ty = self
            .expr(selector, insn)?
            .ok_or(WasmError::Unsupported("a `switch` with no selector"))?;
        let scratch = self.scratch(selector_ty);
        insn.local_set(scratch);
        for (index, arm) in arms.iter().enumerate() {
            // A bare `default` matches nothing here: it is where the chain lands when every test failed.
            if arm.patterns.is_empty() && arm.guard.is_none() {
                continue;
            }
            let target = u32::try_from(index).map_err(|_| WasmError::TooLarge)?;
            insn.block();
            let next = insn.depth();
            for pattern in &arm.patterns {
                self.match_pattern(pattern, scratch, next, None, insn)?;
            }
            // The guard runs after the patterns bound, because it is written in terms of the bindings.
            if let Some(guard) = &arm.guard {
                self.expr(guard, insn)?
                    .ok_or(WasmError::Unsupported("a guarded `case`"))?;
                insn.i32_eqz();
                insn.br_if(insn.depth() - next);
            }
            // One block deeper than the depth the arms' blocks were opened at.
            insn.br(target + 1);
            insn.end();
        }
        insn.br(fallback);
        Ok(())
    }

    /// Emit the selector and the jump into the arms.
    fn dispatch(
        &mut self,
        selector: &ast::Expr,
        arms: &[Arm],
        fallback: u32,
        insn: &mut Insn,
    ) -> Result<()> {
        if arms
            .iter()
            .any(|arm| !arm.patterns.is_empty() || arm.guard.is_some())
        {
            return self.dispatch_patterns(selector, arms, fallback, insn);
        }
        // The selector has to *already* be an `i32`. Converting one that is not would narrow it
        // silently: a `long` selector is not a Java program, but an `i32.wrap_i64` would turn it into
        // one that switches on the low 32 bits.
        if !matches!(
            self.input.type_of_expr(Facts::span(selector.syntax())),
            Some(Ty::Primitive(
                Primitive::Byte | Primitive::Short | Primitive::Char | Primitive::Int
            ))
        ) {
            return Err(WasmError::Unsupported("a `switch` on this selector type"));
        }
        let mut cases: Vec<(i32, u32)> = Vec::new();
        for (index, arm) in arms.iter().enumerate() {
            let target = u32::try_from(index).map_err(|_| WasmError::TooLarge)?;
            for &key in &arm.keys {
                cases.push((key, target));
            }
        }
        self.expr(selector, insn)?;
        let Some((&(first, _), rest)) = cases.split_first() else {
            // No `case` at all: the selector is still evaluated, and every key is the default.
            insn.drop().br(fallback);
            return Ok(());
        };
        let (min, max) = rest.iter().fold((first, first), |(low, high), &(key, _)| {
            (low.min(key), high.max(key))
        });
        // A table costs one entry per key in its range, present or not; the comparison chain costs
        // four instructions per key. Past a spread of a few empty slots per key the chain is smaller.
        let span = i64::from(max) - i64::from(min) + 1;
        if span <= 2 * i64::try_from(cases.len()).unwrap_or(i64::MAX) + 8 {
            let mut targets = alloc::vec![fallback; usize::try_from(span).unwrap_or(0)];
            for &(key, target) in &cases {
                let slot = usize::try_from(i64::from(key) - i64::from(min)).unwrap_or(0);
                // The first label wins, which is what a duplicate `case` would mean if it were legal.
                if targets[slot] == fallback {
                    targets[slot] = target;
                }
            }
            // `br_table` reads its index as *unsigned*, so subtracting the lowest key is the whole
            // bounds check: a key below it wraps past 2³¹ and lands on the default with the rest.
            if min != 0 {
                insn.i32_const(min);
                insn.numeric(NumOp::Sub, ValType::I32)
                    .ok_or(WasmError::Unsupported("a `switch` offset"))?;
            }
            insn.br_table(&targets, fallback);
            return Ok(());
        }
        // Sparse: wasm has no `lookupswitch`, so the keys are compared one at a time. The selector
        // lives in a local because each comparison consumes a copy of it.
        let slot = self.scratch(ValType::I32);
        insn.local_set(slot);
        for &(key, target) in &cases {
            insn.local_get(slot).i32_const(key);
            insn.numeric(NumOp::Eq, ValType::I32)
                .ok_or(WasmError::Unsupported("a `switch` comparison"))?;
            insn.br_if(target);
        }
        insn.br(fallback);
        Ok(())
    }

    /// The colon form's arms, which fall through into one another.
    fn switch_groups(
        &mut self,
        groups: &[ast::SwitchGroup],
        result: Option<ValType>,
        insn: &mut Insn,
    ) -> Result<()> {
        // A value switch must leave every arm. The colon form falls through, so it is the *last*
        // group that decides whether the fall-out path exists — and that path has no value to
        // leave on the stack. Emitting it anyway produced a `block` with a declared result type
        // whose fall-out was filled with `unreachable`: a module that loads, validates, and traps.
        if result.is_some() && !groups.last().is_some_and(Facts::arm_leaves) {
            return Err(WasmError::Unsupported(
                "a `switch` expression arm that yields nothing",
            ));
        }
        for group in groups {
            insn.end();
            for statement in group.stmts() {
                self.stmt(&statement, insn)?;
            }
            // No branch: falling into the next group is what the colon form means, and a group that
            // wanted to stop said `break`.
        }
        Ok(())
    }

    /// The arrow form, where each arm stands alone and leaves the `switch` when it finishes.
    fn switch_rules(
        &mut self,
        rules: &[ast::SwitchRule],
        result: Option<ValType>,
        leave: u32,
        insn: &mut Insn,
    ) -> Result<()> {
        for rule in rules {
            insn.end();
            // Three body forms: an expression, a block, or a `throw`. In an expression `switch` the
            // first *is* the arm's value; in a statement one it is evaluated for its effect.
            if let Some(value) = rule.expr() {
                match result {
                    Some(ty) => self.arm_value(&value, ty, insn)?,
                    None => self.discard(&value, insn)?,
                }
                insn.br(insn.depth() - leave);
                continue;
            }
            if let Some(block) = rule.syntax().children().find_map(ast::Block::cast) {
                self.block(&block, insn)?;
            } else if let Some(thrown) = rule.syntax().children().find_map(ast::ThrowStmt::cast) {
                self.stmt(&ast::Stmt::Throw(thrown), insn)?;
            } else {
                return Err(WasmError::Unsupported("a `switch` arm of this form"));
            }
            // A block arm of an expression `switch` leaves by `yield`, which has already branched to the
            // same label carrying the value — so falling off the arm's own end is what Java's "every arm
            // yields or throws" rule says cannot happen, and branching here would branch with no value.
            // The instruction states that rule to the validator, exactly as the colon form's does.
            if result.is_some() {
                insn.unreachable();
            } else {
                insn.br(insn.depth() - leave);
            }
        }
        Ok(())
    }

    /// One arrow arm's value, converted to the type the whole `switch` expression has.
    fn arm_value(&mut self, value: &ast::Expr, ty: ValType, insn: &mut Insn) -> Result<()> {
        if self.num_of(value.syntax()).is_ok()
            && let Ok(target) = Self::num_for(ty)
        {
            return self.operand(value, target, insn);
        }
        self.expr(value, insn)?
            .ok_or(WasmError::Unsupported("a `switch` arm with no value"))?;
        Ok(())
    }

    /// `break` / `break label` / `continue` / `continue label`.
    ///
    /// The branch depth comes from the emitter, not from the source: an `if` between a loop header and
    /// the branch shifts every target, and only the emitter knows how many structures are open.
    fn leave(
        &mut self,
        label: Option<jals_syntax::SyntaxToken>,
        continuing: bool,
        insn: &mut Insn,
    ) -> Result<()> {
        let label = label.map(|token| jals_syntax::decoded_ident(&token).into_owned());
        let target = self
            .loops
            .iter()
            .rev()
            .find(|entry| {
                let named = label
                    .as_ref()
                    .is_none_or(|wanted| entry.label.as_deref() == Some(wanted.as_str()));
                named && (!continuing || entry.repeat.is_some())
            })
            .ok_or(WasmError::Unsupported(
                "a `break` or `continue` with no enclosing target",
            ))?;
        let depth = if continuing {
            target.repeat.ok_or(WasmError::Unsupported(
                "a `continue` naming something that is no loop",
            ))?
        } else {
            target.leave
        };
        // Every `finally` opened *since* the target statement was entered is one this jump leaves
        // behind, so each runs on the way out, innermost first. A cleanup opened outside the target is
        // not left behind and must not run.
        let outer = target.cleanups;
        self.run_cleanups(outer, insn)?;
        insn.br(insn.depth() - depth);
        Ok(())
    }

    /// Emit the cleanups above `outer`, innermost first — the `finally` blocks a jump leaves behind.
    ///
    /// Each is lowered against the cleanups that enclose *it*, not against the whole open set. A
    /// `finally` is not protected by itself: a `return` inside one runs only what is outside it, and
    /// §14.20.2 gives that jump the abrupt completion — the enclosing `try` is already leaving.
    /// Lowering against the unshrunk set instead re-entered the same cleanup for every jump it
    /// contained, which recursed until the compiler's own stack ran out.
    ///
    /// The stack is restored afterwards because this is one *copy* of the cleanup, not the end of
    /// its scope: the exceptional and normal paths still have theirs to emit, and
    /// [`try_catch`](Self::try_catch) is what pops for good.
    fn run_cleanups(&mut self, outer: usize, insn: &mut Insn) -> Result<()> {
        let open = core::mem::take(&mut self.cleanups);
        let mut outcome = Ok(());
        for index in (outer.min(open.len())..open.len()).rev() {
            self.cleanups.clear();
            self.cleanups.extend_from_slice(&open[..index]);
            outcome = self.block(&open[index], insn);
            if outcome.is_err() {
                break;
            }
        }
        self.cleanups = open;
        outcome
    }

    // --- expressions --------------------------------------------------------

    /// Emit `expr`. Returns its type, or `None` when it left nothing on the stack.
    /// One expression, with the source it is written at attached to any failure inside it.
    ///
    /// Every recursive descent in the body below goes through this wrapper, so a failure is named
    /// by the innermost expression whose lowering raised it: a call that fails leaves the call's
    /// own span, and the assignment it is written in keeps it rather than naming the whole
    /// statement. See [`WasmError::at`].
    fn expr(&mut self, expr: &ast::Expr, insn: &mut Insn) -> Result<Option<ValType>> {
        let file = self.input.file();
        let range = Self::written_range(expr.syntax());
        self.expression(expr, insn)
            .map_err(|error| error.at(file, range))
    }

    /// The expression forms, dispatched one per node kind.
    fn expression(&mut self, expr: &ast::Expr, insn: &mut Insn) -> Result<Option<ValType>> {
        match expr {
            ast::Expr::Literal(literal) => self.literal(literal, insn).map(Some),
            ast::Expr::Paren(paren) => {
                let inner = paren
                    .expr()
                    .ok_or(WasmError::Unsupported("an empty parenthesis"))?;
                self.expr(&inner, insn)
            }
            // `this` and `super` both parse as a name reference carrying no identifier token, so
            // nothing resolves either as a name; both are local 0. What `super` changes is which
            // *body* a call reaches and which declaration a field access names, and both of those are
            // settled elsewhere — the host's collector types the value by its struct, and a
            // subclass's struct is a declared subtype of its superclass's.
            ast::Expr::NameRef(_)
                if Facts::is_this(expr.syntax()) || Facts::is_super(expr.syntax()) =>
            {
                let owner = self
                    .owner
                    .ok_or(WasmError::Unsupported("`this` in a `static` method"))?;
                insn.local_get(0);
                self.layout.class_ref(owner).map(Some)
            }
            ast::Expr::NameRef(name) => self.name(name, insn).map(Some),
            ast::Expr::Binary(binary) => self.binary(binary, insn).map(Some),
            ast::Expr::Assignment(assignment) => self.assignment(assignment, true, insn),
            ast::Expr::New(new) => self.new_object(new, insn).map(Some),
            ast::Expr::Call(call) => self.call(call, insn),
            ast::Expr::Index(index) => self.index(index, insn).map(Some),
            ast::Expr::FieldAccess(access) => self.field(access, insn).map(Some),
            ast::Expr::Unary(unary) => self.unary(unary, true, insn),
            ast::Expr::Postfix(postfix) => {
                let (target, delta) = Self::postfix(postfix)?;
                self.update(&target, delta, false, true, insn)
            }
            ast::Expr::Ternary(ternary) => self.ternary(ternary, insn).map(Some),
            ast::Expr::Switch(switch) => {
                let selector = switch
                    .selector()
                    .ok_or(WasmError::Unsupported("a `switch` with no selector"))?;
                let body = switch
                    .body()
                    .ok_or(WasmError::Unsupported("a `switch` with no body"))?;
                let ty = self.ty_of(switch.syntax())?;
                self.switch(&selector, &body, Some(ty), insn)?;
                Ok(Some(ty))
            }
            ast::Expr::Cast(cast) => self.cast(cast, insn).map(Some),
            // Both need a function reference and a target type to make one *of*; the interface that
            // would be that type is not laid out yet either. Each names itself rather than sharing a
            // catch-all, which said only "this expression form".
            // A lambda *is* an instance of a one-method class here, so building one is building that: allocate
            // the struct and write the captures into it, exactly as an anonymous class's `new` does. There is
            // no `invokedynamic` to reach for and no need of one — the dispatch chain already finds the type.
            ast::Expr::MethodRef(reference) => {
                // The same object a lambda builds: the type is what the dispatch chain tests, and a delegating
                // reference captures nothing to write into it.
                let item = self
                    .index
                    .item_by_decl(self.input.file(), Facts::span(reference.syntax()).start)
                    .ok_or(WasmError::Unsupported("a method reference with no item"))?;
                let struct_type = *self
                    .layout
                    .structs
                    .get(&item)
                    .ok_or(WasmError::Unsupported(
                        "a method reference with no struct type",
                    ))?;
                let ty = self.layout.class_ref(item)?;
                insn.struct_new_default(struct_type);
                let captured = self.layout.captures.get(&item).cloned().unwrap_or_default();
                if !captured.is_empty() {
                    let slot = self.scratch(ty);
                    let first = *self
                        .layout
                        .capture_slot
                        .get(&item)
                        .ok_or(WasmError::Unsupported("a capture with no field"))?;
                    insn.local_set(slot);
                    for (offset, (id, _)) in captured.iter().enumerate() {
                        insn.local_get(slot);
                        self.push_capture(*id, insn)?;
                        let field =
                            first + u32::try_from(offset).map_err(|_| WasmError::TooLarge)?;
                        insn.struct_set(struct_type, field);
                    }
                    insn.local_get(slot);
                }
                Ok(Some(ty))
            }
            ast::Expr::Lambda(lambda) => {
                let item = self
                    .index
                    .item_by_decl(self.input.file(), Facts::span(lambda.syntax()).start)
                    .ok_or(WasmError::Unsupported("a lambda with no item"))?;
                let struct_type = *self
                    .layout
                    .structs
                    .get(&item)
                    .ok_or(WasmError::Unsupported("a lambda with no struct type"))?;
                let ty = self.layout.class_ref(item)?;
                insn.struct_new_default(struct_type);
                let captured = self.layout.captures.get(&item).cloned().unwrap_or_default();
                if !captured.is_empty() {
                    let slot = self.scratch(ty);
                    let first = *self
                        .layout
                        .capture_slot
                        .get(&item)
                        .ok_or(WasmError::Unsupported("a capture with no field"))?;
                    insn.local_set(slot);
                    for (offset, (id, _)) in captured.iter().enumerate() {
                        insn.local_get(slot);
                        self.push_capture(*id, insn)?;
                        let field =
                            first + u32::try_from(offset).map_err(|_| WasmError::TooLarge)?;
                        insn.struct_set(struct_type, field);
                    }
                    insn.local_get(slot);
                }
                Ok(Some(ty))
            }
            ast::Expr::ClassLiteral(_) => Err(WasmError::Unsupported("a `.class` literal")),
            // No target here, so the element type is whatever inference read off the elements. That is
            // right when they agree with the declaration and wrong when they do not — which is why a
            // declaration hands its own type down through `value_as` instead of coming through here.
            ast::Expr::ArrayInit(init) => self.array_initializer(init, None, insn).map(Some),
        }
    }

    /// Emit `expr` for its effect, dropping any value it leaves.
    ///
    /// An assignment and an increment are asked *not* to produce one rather than having it dropped
    /// afterwards: with no `dup`, producing it means a second load of a field or an array element, and
    /// an expression statement never wanted it.
    fn discard(&mut self, expr: &ast::Expr, insn: &mut Insn) -> Result<()> {
        match expr {
            ast::Expr::Assignment(assignment) => {
                self.assignment(assignment, false, insn)?;
            }
            ast::Expr::Unary(unary) => {
                if self.unary(unary, false, insn)?.is_some() {
                    insn.drop();
                }
            }
            ast::Expr::Postfix(postfix) => {
                let (target, delta) = Self::postfix(postfix)?;
                self.update(&target, delta, false, false, insn)?;
            }
            other => {
                if self.expr(other, insn)?.is_some() {
                    insn.drop();
                }
            }
        }
        Ok(())
    }

    /// The target and step of a postfix `++` / `--`.
    fn postfix(postfix: &ast::PostfixExpr) -> Result<(ast::Expr, i8)> {
        use jals_syntax::SyntaxKind::{MINUS_MINUS, PLUS_PLUS};
        let target = postfix
            .operand()
            .ok_or(WasmError::Unsupported("an increment of nothing"))?;
        let delta = postfix
            .syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find_map(|token| match token.kind() {
                PLUS_PLUS => Some(1),
                MINUS_MINUS => Some(-1),
                _ => None,
            })
            .ok_or(WasmError::Unsupported("this postfix operator"))?;
        Ok((target, delta))
    }

    /// The member a field access names, and the class that declares it.
    fn field_target(&self, access: &ast::FieldAccess) -> Result<(ItemId, MemberId)> {
        let member = self.facts().field_target(access)?;
        Ok((self.index.member(member).owner, member))
    }

    fn literal(&mut self, literal: &ast::Literal, insn: &mut Insn) -> Result<ValType> {
        use jals_syntax::SyntaxKind::{
            CHAR_LITERAL, FALSE_KW, FLOAT_LITERAL, INT_LITERAL, NULL_KW, STRING_LITERAL, TRUE_KW,
        };
        let token = literal
            .token()
            .ok_or(WasmError::Unsupported("an empty literal"))?;
        // `null` has no type of its own, so it is answered before `ty_of` is asked for one.
        if token.kind() == NULL_KW {
            insn.ref_null(HeapType::None);
            return Ok(ValType::Ref(RefType::nullable(HeapType::None)));
        }
        // A string literal is not a value the target holds: it is a `String` object built at run
        // time from a `char[]` compiled into the module. So it too is answered before `ty_of`,
        // which would refuse the `String` type itself rather than the operation that builds one.
        if token.kind() == STRING_LITERAL {
            return self.string_literal(literal.syntax(), token.text(), insn);
        }
        let ty = self.ty_of(literal.syntax())?;
        let text = token.text();
        match token.kind() {
            TRUE_KW => {
                insn.i32_const(1);
            }
            FALSE_KW => {
                insn.i32_const(0);
            }
            INT_LITERAL => {
                // The shared fact, not the other backend's: `0xFF`, `0b1010`, `017`, and `1_000` all
                // mean what they mean in both, and reading them twice was two chances to disagree
                // about one of them. The width comes from the inferred type below, so the one the
                // fact reads off the suffix is dropped.
                let (value, _) = Literal::integer(text)?;
                match ty {
                    ValType::I64 => insn.i64_const(value),
                    _ => insn
                        .i32_const(i32::try_from(value).map_err(|_| {
                            WasmError::Unsupported("an out-of-range `int` literal")
                        })?),
                };
            }
            FLOAT_LITERAL => {
                let (value, _) = Literal::floating(text)?;
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "the inferred type says `f32`, and that narrowing is what a `float` \
                              constant is"
                )]
                match ty {
                    ValType::F32 => insn.f32_const(value as f32),
                    _ => insn.f64_const(value),
                };
            }
            // A `char` is an unsigned 16-bit integer, so it is an `i32` here like every other integral
            // type narrower than `long`. The escape reading is shared with the JVM backend: `'\n'` and
            // `'\u0041'` mean what they mean in both, and reading them twice would be two chances to
            // disagree about one of them.
            CHAR_LITERAL => {
                let value = Literal::character(text)?;
                match ty {
                    ValType::I64 => insn.i64_const(i64::from(u32::from(value))),
                    _ => insn.i32_const(i32::try_from(u32::from(value)).unwrap_or(0)),
                };
            }
            _ => return Err(WasmError::Unsupported("this literal kind")),
        }
        Ok(ty)
    }

    /// Build a `String` from a string literal: its characters copied out of the module's data
    /// section into a `char[]`, then handed to `String(char[])`.
    ///
    /// The constructor is the one the index resolved for `java.lang.String` — the platform's, when
    /// a library provides it. Nothing here invents a representation for the stub: a `String` whose
    /// package is not linked has no constructor to run, and refusing names the gap instead of
    /// building an object whose methods would be absent.
    ///
    /// The two shapes are the boundary's: a linked library exports its constructor as a *factory*
    /// that allocates and returns the object, while the module's own constructor takes a receiver
    /// first and returns nothing, so the object is allocated here and stored while the array is
    /// pushed underneath it.
    fn string_literal(
        &mut self,
        node: &SyntaxNode,
        source: &str,
        insn: &mut Insn,
    ) -> Result<ValType> {
        // The report names the *type* the expression was inferred as, in the words every other
        // unrepresentable type gets: a literal that cannot be built is a `String` this target has
        // no representation for, not a missing feature of literals.
        let name = self
            .input
            .type_of_expr(Facts::span(node))
            .map_or_else(|| "String".to_owned(), alloc::string::ToString::to_string);
        let missing = || WasmError::NoRepresentation(name.clone());
        let item = self
            .index
            .item_by_fqn("java.lang.String")
            .ok_or_else(missing)?;
        let text = Literal::text(source)?;
        let &(_, data, units) = self
            .layout
            .literals
            .iter()
            .find(|(value, ..)| *value == text)
            .ok_or_else(missing)?;
        let array = self.layout.array_type(ValType::I32).ok_or_else(missing)?;
        let constructor = self
            .index
            .own_members(item)
            .iter()
            .copied()
            .find(|&member| {
                self.index.member(member).kind == DefKind::Constructor
                    && matches!(
                        self.index.resolved_param_tys(member).as_slice(),
                        [Ty::Array(element)] if **element == Ty::Primitive(Primitive::Char)
                    )
            })
            .ok_or_else(missing)?;
        let count = i32::try_from(units).map_err(|_| WasmError::TooLarge)?;
        if let Some(&factory) = self.layout.external_constructors.get(&constructor) {
            insn.i32_const(0)
                .i32_const(count)
                .array_new_data(array, data);
            insn.call(factory);
            return self.layout.class_ref(item);
        }
        let ty = self.layout.class_ref(item)?;
        let structure = self
            .layout
            .structs
            .get(&item)
            .copied()
            .ok_or_else(missing)?;
        let function = self
            .layout
            .functions
            .get(&constructor)
            .copied()
            .ok_or_else(missing)?;
        let slot = self.scratch(ty);
        insn.struct_new_default(structure).local_set(slot);
        insn.local_get(slot)
            .i32_const(0)
            .i32_const(count)
            .array_new_data(array, data);
        insn.call(function).local_get(slot);
        Ok(ty)
    }

    /// Push the receiver an unqualified member access starts from, walking out through enclosing
    /// instances until one is a subtype of `owner`, and report which class it landed on.
    ///
    /// Local 0 is `this`, and for a member of this class or one it inherits that is the whole
    /// answer — which is why pushing local 0 unconditionally was right for as long as a local or
    /// anonymous class never held an enclosing instance. Now that one can, a name written inside it
    /// may name a field or a method of the class the *method* was written in, and reaching that
    /// means reading the synthetic field out, once per level.
    ///
    /// The JVM lowering has walked this chain all along (`Expr::load_unqualified_receiver`,
    /// emitting `getfield this$0`); this is the same walk over this target's struct fields.
    fn load_unqualified_receiver(&self, owner: ItemId, insn: &mut Insn) -> Result<ItemId> {
        let mut item = self.owner.ok_or(WasmError::Unsupported(
            "an unqualified member in a `static` method",
        ))?;
        insn.local_get(0);
        while !self.index.is_subtype(item, owner) {
            // Every enclosing instance in reach has been walked and none of them owns the member.
            let next = *self.layout.inner.get(&item).ok_or(WasmError::Unsupported(
                "an unqualified member of no enclosing instance in scope",
            ))?;
            let slot = *self
                .layout
                .outer
                .get(&item)
                .ok_or(WasmError::Unsupported("an inner class with no outer field"))?;
            insn.struct_get(self.layout.structs[&item], slot);
            item = next;
        }
        Ok(item)
    }

    fn name(&self, name: &ast::NameRef, insn: &mut Insn) -> Result<ValType> {
        let text = name.syntax().text().to_string();
        let unresolved = || WasmError::Unresolved(text.trim().into());
        let member = match self.facts().def_at(name.syntax()) {
            Some(id) => {
                if let Some(slot) = self.slot_of(id) {
                    insn.local_get(slot);
                    return self.layout.val_type(self.input.type_of_def(id));
                }
                // A captured local is not a local *here*: it lives in the field the constructor filled.
                if let Some((field, ty)) = self.capture_field(id) {
                    let owner = self.owner.ok_or_else(unresolved)?;
                    insn.local_get(0)
                        .struct_get(self.layout.structs[&owner], field);
                    return self.layout.val_type(&ty);
                }
                // Not a local: a field of the enclosing class. A `static` one is a global and needs no
                // receiver; an instance one is reached through `this`, which is local 0.
                self.facts().member_of_def(id)
            }
            // Nothing in the file declared it, which an *inherited* field never is.
            None => self.inherited_field(name.syntax()),
        };
        let member = member.ok_or_else(unresolved)?;
        if self.index.member(member).modifiers.is_static {
            let ty = self
                .layout
                .val_type(&self.index.resolved_member_ty(member))?;
            // A linked library's `static` field is reached through its accessor, which also runs
            // the class's initialiser — the same thing `ensure_initialised` would do here.
            if let Some(&(get, _)) = self.layout.external_statics.get(&member) {
                insn.call(get);
                return Ok(ty);
            }
            let global = self.layout.statics.get(&member).ok_or_else(unresolved)?;
            self.ensure_initialised(member, insn);
            insn.global_get(*global);
            return Ok(ty);
        }
        let item = self.load_unqualified_receiver(self.index.member(member).owner, insn)?;
        let slot = self
            .layout
            .field_slot_or(self.index, item, member, text.trim().to_owned())?;
        insn.struct_get(self.layout.structs[&item], slot);
        self.layout.val_type(&self.index.resolved_member_ty(member))
    }

    /// Initialise the class that declares `member` before its `static` field is touched.
    ///
    /// JLS §12.4.1 initialises a class on its first *use*, and a `static` field access is one. The
    /// function guards itself, so all but the first call is a load and a branch — and a class reaching
    /// its own statics mid-initialisation gets the values written so far, which is what §12.4.2 says.
    fn ensure_initialised(&self, member: MemberId, insn: &mut Insn) {
        if let Some(&(function, _)) = self
            .layout
            .class_inits
            .get(&self.index.member(member).owner)
        {
            insn.call(function);
        }
    }

    /// The field an unqualified name reaches when nothing in the file declared it: one of a supertype's.
    ///
    /// Name resolution is file-local, and a superclass's field is not something it can see — it may not
    /// even be in this file. So the name is looked up on the enclosing type and then up the superclass
    /// chain, nearest first, which is the order that makes a shadowing field win. A struct holds its
    /// supertype's fields first, so the slot the inherited member lands in is the enclosing type's own.
    fn inherited_field(&self, node: &SyntaxNode) -> Option<MemberId> {
        let name = Facts::name_token(node)?;
        self.index
            .inherited_field(self.owner?, &jals_syntax::decoded_ident(&name))
    }

    /// `{1, 2, 3}`, whose elements are written rather than defaulted.
    ///
    /// An array initialiser has no type of its own — `{1, 2}` is an array of whatever it is assigned to
    /// — so the element type comes from the type inference recorded for the *declaration*. One
    /// instruction takes the values from the stack, so there is no allocate-then-fill sequence and no
    /// index to keep.
    fn array_initializer(
        &mut self,
        init: &ast::ArrayInit,
        target: Option<&Ty>,
        insn: &mut Insn,
    ) -> Result<ValType> {
        // The *target* decides the element type, not the elements: `long[] c = {1, 2}` is an `i64`
        // array whose elements happen to be written as `int` literals, and reading the type off the
        // elements built an `i32` array instead — a module the validator rejects, and the wrong type if
        // it had not.
        let inferred = self.input.type_of_expr(Facts::span(init.syntax())).cloned();
        let Some(Ty::Array(element)) = target.cloned().or(inferred) else {
            return Err(WasmError::Unsupported(
                "an array initialiser with no target type",
            ));
        };
        let element_ty = self.layout.val_type(&element)?;
        let array_type = self
            .layout
            .array_type(element_ty)
            .ok_or_else(|| WasmError::NoRepresentation("an array".to_owned()))?;
        let elements: Vec<ast::Expr> = init.elements().collect();
        let count = u32::try_from(elements.len()).map_err(|_| WasmError::TooLarge)?;
        for value in &elements {
            // A nested initialiser (`{{1}, {2}}`) reaches this same arm through `expr`, whose recorded
            // type is the inner array's — so nothing here has to know how deep it is.
            self.value_as(value, &element, insn)?;
        }
        insn.array_new_fixed(array_type, count);
        Ok(ValType::Ref(RefType::nullable(HeapType::Concrete(
            array_type,
        ))))
    }

    /// Emit `value` as a value of the declared type `declared`, converting where a numeric assignment
    /// conversion applies and handing a nested array initialiser its own element type.
    fn value_as(&mut self, value: &ast::Expr, declared: &Ty, insn: &mut Insn) -> Result<()> {
        if let ast::Expr::ArrayInit(nested) = value {
            self.array_initializer(nested, Some(declared), insn)?;
            return Ok(());
        }
        let declared_ty = self.layout.val_type(declared)?;
        if self.num_of(value.syntax()).is_ok()
            && let Ok(target) = Self::num_for(declared_ty)
        {
            return self.operand(value, target, insn);
        }
        let produced = self
            .expr(value, insn)?
            .ok_or(WasmError::Unsupported("a value that produced nothing"))?;
        // The declaration is what says which type is wanted, so erasure's top-of-hierarchy value
        // comes back down here — the same `ref.cast` an argument and a `return` get — and a
        // primitive declared *into* a wrapper goes up through its `valueOf` by the same rule.
        self.coerce(value, produced, declared_ty, insn)
    }

    /// `array[index]`.
    fn index(&mut self, expr: &ast::IndexExpr, insn: &mut Insn) -> Result<ValType> {
        let mut parts = expr.parts();
        let array = parts
            .next()
            .ok_or(WasmError::Unsupported("an index with no array"))?;
        let subscript = parts
            .next()
            .ok_or(WasmError::Unsupported("an index with no subscript"))?;
        let element = self.ty_of(expr.syntax())?;
        let array_type = self
            .layout
            .array_type(element)
            .ok_or_else(|| WasmError::NoRepresentation("an array".to_owned()))?;
        self.expr(&array, insn)?;
        // An index is an `int` after the unary numeric promotion of JLS §15.10.3, so a boxed
        // subscript unboxes here and a narrower one widens; the array is a reference already.
        self.operand(&subscript, Numeric::Int, insn)?;
        insn.array_get(array_type);
        Ok(element)
    }

    fn field(&mut self, access: &ast::FieldAccess, insn: &mut Insn) -> Result<ValType> {
        // `array.length` is not a field at all — wasm gives an array its own instruction. The
        // *classification* is shared; the instruction is not.
        if self.facts().is_array_length(access)
            && let Some(receiver) = access.receiver()
        {
            self.expr(&receiver, insn)?;
            insn.array_len();
            return Ok(ValType::I32);
        }
        let (owner, member) = self.field_target(access)?;
        // A `static` field is a global, so the receiver names only the *class*: it is not evaluated,
        // exactly as `getstatic` ignores one on the JVM.
        if self.index.member(member).modifiers.is_static {
            // No global means the field's type has none either, so asking for its `ValType` is what
            // produces the report — the same one a local of that type would get.
            let ty = self
                .layout
                .val_type(&self.index.resolved_member_ty(member))?;
            // A linked library's `static` field is reached through its accessor, which also runs
            // the class's initialiser — the same thing `ensure_initialised` would do here.
            if let Some(&(get, _)) = self.layout.external_statics.get(&member) {
                insn.call(get);
                return Ok(ty);
            }
            let global = self
                .layout
                .statics
                .get(&member)
                .ok_or_else(|| WasmError::Unresolved(access.field().unwrap_or_default()))?;
            self.ensure_initialised(member, insn);
            insn.global_get(*global);
            return Ok(ty);
        }
        let receiver = access
            .receiver()
            .ok_or(WasmError::Unsupported("a field access with no receiver"))?;
        // At the owner's type, for the reason a call's receiver is: `struct.get` names one struct.
        let receiver_ty = self.layout.class_ref(owner)?;
        self.expr_as(&receiver, receiver_ty, insn)?;
        let slot = self.layout.field_slot_or(
            self.index,
            owner,
            member,
            access.field().unwrap_or_default(),
        )?;
        insn.struct_get(self.layout.structs[&owner], slot);
        self.layout.val_type(&self.index.resolved_member_ty(member))
    }

    fn unary(
        &mut self,
        unary: &ast::UnaryExpr,
        keep: bool,
        insn: &mut Insn,
    ) -> Result<Option<ValType>> {
        let operand = unary
            .operand()
            .ok_or(WasmError::Unsupported("a unary with no operand"))?;
        let operator =
            Unary::of(unary.syntax()).ok_or(WasmError::Unsupported("this unary operator"))?;
        // A prefix `++` / `--` is an assignment, not an operator on a value.
        if let Some(step) = operator.step() {
            return self.update(&operand, step, true, keep, insn);
        }
        let ty = match operator {
            // `!b` flips a `boolean`, which is an `i32` that is 0 or 1 — so `i32.eqz` *is* the flip.
            Unary::Not => {
                let produced = self
                    .expr(&operand, insn)?
                    .ok_or(WasmError::Unsupported("a `!` on nothing"))?;
                self.coerce(&operand, produced, ValType::I32, insn)?;
                insn.i32_eqz();
                ValType::I32
            }
            // `+` is not a no-op: unary numeric promotion still applies.
            Unary::Plus => {
                let promoted = Numeric::promote_one(self.num_of(operand.syntax())?);
                self.operand(&operand, promoted, insn)?;
                promoted.val()
            }
            Unary::Minus => {
                let promoted = Numeric::promote_one(self.num_of(operand.syntax())?);
                // wasm has no integer negation at all, so an integral `-x` is `0 - x` — which means
                // the zero goes on the stack *before* the operand.
                match promoted {
                    Numeric::Long => {
                        insn.i64_const(0);
                    }
                    Numeric::Float | Numeric::Double => {}
                    _ => {
                        insn.i32_const(0);
                    }
                }
                self.operand(&operand, promoted, insn)?;
                if matches!(promoted, Numeric::Float | Numeric::Double) {
                    insn.neg(promoted.val())
                        .ok_or(WasmError::Unsupported("this negation"))?;
                } else {
                    insn.numeric(NumOp::Sub, promoted.val())
                        .ok_or(WasmError::Unsupported("this negation"))?;
                }
                promoted.val()
            }
            // `~n` is `n ^ -1`, at the promoted width.
            Unary::BitNot => {
                let promoted = Numeric::promote_one(self.num_of(operand.syntax())?);
                self.operand(&operand, promoted, insn)?;
                match promoted {
                    Numeric::Long => insn.i64_const(-1),
                    _ => insn.i32_const(-1),
                };
                insn.numeric(NumOp::Xor, promoted.val())
                    .ok_or(WasmError::Unsupported("this complement"))?;
                promoted.val()
            }
            // The two assignment forms returned above.
            Unary::Increment | Unary::Decrement => {
                return Err(WasmError::Unsupported("this unary operator"));
            }
        };
        Ok(Some(ty))
    }

    /// `(T) e`.
    ///
    /// A primitive cast is a conversion; a reference cast is `ref.cast`, which *traps* rather than
    /// throwing a `ClassCastException` — the closest a host with no exception model gets, and the
    /// difference is worth knowing rather than hiding.
    fn cast(&mut self, cast: &ast::CastExpr, insn: &mut Insn) -> Result<ValType> {
        let operand = cast
            .expr()
            .ok_or(WasmError::Unsupported("a cast with no operand"))?;
        let ty = cast
            .ty()
            .ok_or(WasmError::Unsupported("a cast with no type"))?;
        if ty.is_primitive_or_var() {
            let target = self.num_of(cast.syntax())?;
            self.operand(&operand, target, insn)?;
            return Ok(target.val());
        }
        // A cast to a **type variable** erases to its bound (JLS §5.5), and the bound is `Object`
        // unless declared — the top of the reference hierarchy, which every reference already is.
        // So it is a cast to nothing: the erasure this backend gives `T` *is* `anyref`, and a
        // `ref.cast` to the top would say nothing the validator does not already know. Resolving the
        // written name instead reported `T` as an unresolved type, which is a report about a type
        // the source declared.
        if matches!(
            self.input.type_of_expr(Facts::span(cast.syntax())),
            Some(Ty::TypeVar { .. })
        ) {
            self.expr(&operand, insn)?
                .ok_or(WasmError::Unsupported("a cast of nothing"))?;
            return Ok(ValType::Ref(RefType::nullable(HeapType::Any)));
        }
        let heap = self.named_type(&ty)?;
        let produced = self
            .expr(&operand, insn)?
            .ok_or(WasmError::Unsupported("a cast of nothing"))?;
        // A cast of a *primitive* to a reference type is a boxing conversion with the cast written
        // on it (JLS §5.5.1): there is nothing to cast, only a `valueOf` to call. Everything else
        // has a value that already is a reference, which is what `ref.cast` takes.
        if Self::numeric(produced).is_some() {
            self.box_value(&operand, insn)?;
        } else {
            insn.ref_cast(heap, true);
        }
        Ok(ValType::Ref(RefType::nullable(heap)))
    }

    fn binary(&mut self, binary: &ast::BinaryExpr, insn: &mut Insn) -> Result<ValType> {
        let operator = Operator::binary(binary.syntax())
            .ok_or(WasmError::Unsupported("this binary operator"))?;

        // Before the operands: an `instanceof` whose right side is a *pattern* has no right operand at
        // all — the pattern is a binding, not an expression, and asking for one reported the wrong thing.
        if operator == Operator::InstanceOf {
            return self.instance_of(binary, insn);
        }
        let left = binary
            .lhs()
            .ok_or(WasmError::Unsupported("a binary with no left operand"))?;
        let right = binary
            .rhs()
            .ok_or(WasmError::Unsupported("a binary with no right operand"))?;

        // `&&` and `||` are not operators over two values: the right operand may not run at all.
        match operator {
            Operator::AndAnd => return self.short_circuit(&left, &right, true, insn),
            Operator::OrOr => return self.short_circuit(&left, &right, false, insn),
            _ => {}
        }
        let op = Self::num_op(operator).ok_or(WasmError::Unsupported("this binary operator"))?;

        // A `+` whose result is a `String` is concatenation, not addition, and it shares this node
        // kind. Asked of the recorded type rather than required of it: `someInteger + 1`'s result is
        // an `int` that inference leaves unknown, and an unknown result is certainly not a `String`.
        if op == NumOp::Add && self.is_string_node(binary.syntax()) {
            return self.concat(&left, &right, insn);
        }

        // A reference `==` / `!=` is identity, not arithmetic, and wasm spells it `ref.eq`.
        if matches!(op, NumOp::Eq | NumOp::Ne) && self.is_reference(left.syntax()) {
            return self.reference_equality(&left, &right, op == NumOp::Ne, insn);
        }

        let left_num = self.num_of(left.syntax())?;
        if op.is_shift() {
            // A shift promotes each side on its own, and wasm wants the *count* at the left operand's
            // own width: `i64.shl` takes two `i64`s where `lshl` takes a `long` and an `int`. So the
            // count is converted to the result's type rather than to `int`.
            let promoted = Numeric::promote_one(left_num);
            self.operand(&left, promoted, insn)?;
            self.operand(&right, promoted, insn)?;
            insn.numeric(op, promoted.val())
                .ok_or(WasmError::Unsupported("this operator on this type"))?;
            return Ok(promoted.val());
        }

        // Both operands share one type, because one opcode names one: `i64.add` over an `i32` is a
        // module the validator rejects. Java's binary numeric promotion says which.
        let promoted = Numeric::promote(left_num, self.num_of(right.syntax())?);
        self.operand(&left, promoted, insn)?;
        self.operand(&right, promoted, insn)?;
        insn.numeric(op, promoted.val())
            .ok_or(WasmError::Unsupported("this operator on this type"))?;
        // A comparison is a `boolean`, which is an `i32`; arithmetic keeps its operand type.
        Ok(match op {
            NumOp::Eq | NumOp::Ne | NumOp::Lt | NumOp::Le | NumOp::Gt | NumOp::Ge => ValType::I32,
            _ => promoted.val(),
        })
    }

    /// Emit `expr` and convert its value to `target`.
    ///
    /// The value's own type is read off the stack rather than from the recorded type, which is what
    /// lets a *wrapper* arrive here: `(int) boxed` unboxes by the accessor the wrapper's class
    /// names, and the conversion that follows is the one the source wrote — widening or narrowing,
    /// as a cast may ask for either. The recorded type still decides every arithmetic promotion; a
    /// cast and a `switch` selector are the callers whose target comes from elsewhere.
    fn operand(&mut self, expr: &ast::Expr, target: Numeric, insn: &mut Insn) -> Result<()> {
        let produced = self
            .expr(expr, insn)?
            .ok_or(WasmError::Unsupported("an operand that produced no value"))?;
        let source = match Self::numeric(produced) {
            Some(source) => source,
            None => self.unbox_value(expr, produced, insn)?,
        };
        if source != target {
            insn.convert(source, target)
                .ok_or(WasmError::Unsupported("this conversion"))?;
        }
        Ok(())
    }

    /// The numeric type `node`'s recorded type is.
    fn num_of(&self, node: &SyntaxNode) -> Result<Numeric> {
        let ty = self
            .input
            .type_of_expr(Facts::span(node))
            .ok_or(WasmError::Unsupported("a value with no inferred type"))?;
        let Ty::Primitive(primitive) = ty else {
            return Err(WasmError::Unsupported("an arithmetic operand of this type"));
        };
        // A `boolean` is not a numeric type (JLS §4.2), so the shared rule refuses it. On *this*
        // target it shares `int`'s representation, and the only operators it reaches are the bitwise
        // ones, where that is exactly right — a statement about wasm, made where wasm decides its
        // own layout rather than folded into the language rule.
        Ok(Numeric::of(*primitive).unwrap_or(Numeric::Int))
    }

    /// Whether `node`'s recorded type is a reference.
    fn is_reference(&self, node: &SyntaxNode) -> bool {
        matches!(
            self.input.type_of_expr(Facts::span(node)),
            Some(Ty::Class(_) | Ty::Array(_) | Ty::Null)
        )
    }

    /// `a == b` / `a != b` over two references, which is identity.
    fn reference_equality(
        &mut self,
        left: &ast::Expr,
        right: &ast::Expr,
        negated: bool,
        insn: &mut Insn,
    ) -> Result<ValType> {
        // `x == null` has no second reference to compare: `ref.null` would need the *other* side's
        // type, and `ref.is_null` asks the question directly.
        let facts = self.facts();
        let (value, other) = if facts.denotes_null(right.syntax()) {
            (left, None)
        } else if facts.denotes_null(left.syntax()) {
            (right, None)
        } else {
            (left, Some(right))
        };
        self.expr(value, insn)?
            .ok_or(WasmError::Unsupported("a comparison operand with no value"))?;
        match other {
            Some(other) => {
                self.expr(other, insn)?
                    .ok_or(WasmError::Unsupported("a comparison operand with no value"))?;
                insn.ref_eq();
            }
            None => {
                insn.ref_is_null();
            }
        }
        if negated {
            insn.i32_eqz();
        }
        Ok(ValType::I32)
    }

    /// `left + right` where the result is a `String`: the builder chain the JVM backend emits,
    /// over whichever `java.lang.StringBuilder` the index resolved.
    ///
    /// A concatenation is not arithmetic, and the builder is why: which rendering an operand gets
    /// is decided by its *static* type — a `long` appends as a `long`, a `char` as a character
    /// rather than its code point — and the class that knows those renderings is `java.base`, not
    /// this backend. The calls below are imports from the linked platform, so a module that links
    /// nothing declaring `java.lang.StringBuilder` is refused with the overload it asked for,
    /// rather than getting a silent wrong rendering.
    ///
    /// The chain is flattened along the left spine, as the JVM lowering flattens it: `a + b + c`
    /// is one builder with three appends, so a loop of concatenations stays linear. Parentheses
    /// group the *tree*, not the chain — `("a" + 1) + 2` is still one builder.
    fn concat(&mut self, left: &ast::Expr, right: &ast::Expr, insn: &mut Insn) -> Result<ValType> {
        let builder = self.string_builder()?;
        let slot = self.builder_start(builder, insn)?;
        self.builder_append(builder, slot, left, insn)?;
        self.builder_append(builder, slot, right, insn)?;
        self.builder_string(builder, slot, insn)
    }

    /// The `java.lang.StringBuilder` the index resolved, or a refusal naming it.
    fn string_builder(&self) -> Result<ItemId> {
        self.index
            .item_by_fqn("java.lang.StringBuilder")
            .ok_or_else(|| WasmError::Unresolved("java.lang.StringBuilder".to_owned()))
    }

    /// `new StringBuilder()`, left in a fresh local; the slot is returned.
    ///
    /// Two shapes again, and the boundary decides which: a consumer calls the factory the ABI
    /// exports, which allocates and runs the constructor, while a module that *owns* the class
    /// allocates here and calls the constructor with the object underneath it.
    fn builder_start(&mut self, builder: ItemId, insn: &mut Insn) -> Result<u32> {
        let missing = || WasmError::Unresolved("java.lang.StringBuilder.<init>()".to_owned());
        let constructor = self
            .index
            .own_members(builder)
            .iter()
            .copied()
            .find(|&id| {
                self.index.member(id).kind == DefKind::Constructor
                    && self.index.resolved_param_tys(id).is_empty()
            })
            .ok_or_else(missing)?;
        // A linked library's factory: the class is replayed into the layout, so `class_ref` has a
        // struct to name. Checked first because with no package the class has none — and the
        // report below is the one that says which class the source needed, where `class_ref`'s
        // generic "an undeclared class" would not.
        if let Some(&factory) = self.layout.external_constructors.get(&constructor) {
            let slot = self.scratch(self.layout.class_ref(builder)?);
            insn.call(factory).local_set(slot);
            return Ok(slot);
        }
        let structure = self
            .layout
            .structs
            .get(&builder)
            .copied()
            .ok_or_else(|| WasmError::NoRepresentation("java.lang.StringBuilder".to_owned()))?;
        let slot = self.scratch(ValType::Ref(RefType::nullable(HeapType::Concrete(
            structure,
        ))));
        let function = self
            .layout
            .functions
            .get(&constructor)
            .copied()
            .ok_or_else(missing)?;
        insn.struct_new_default(structure).local_set(slot);
        insn.local_get(slot).call(function);
        Ok(slot)
    }

    /// Append one operand of a chain, flattening a nested concatenation into the same builder.
    fn builder_append(
        &mut self,
        builder: ItemId,
        slot: u32,
        expr: &ast::Expr,
        insn: &mut Insn,
    ) -> Result<()> {
        // A parenthesised expression groups the tree, not the chain.
        let unwrapped;
        let expr = match expr {
            ast::Expr::Paren(paren) => {
                unwrapped = paren
                    .expr()
                    .ok_or(WasmError::Unsupported("an empty parenthesis"))?;
                &unwrapped
            }
            _ => expr,
        };
        if let ast::Expr::Binary(binary) = expr
            && Operator::binary(binary.syntax()) == Some(Operator::Add)
            && self.is_string_node(binary.syntax())
        {
            let left = binary
                .lhs()
                .ok_or(WasmError::Unsupported("a binary with no left operand"))?;
            let right = binary
                .rhs()
                .ok_or(WasmError::Unsupported("a binary with no right operand"))?;
            self.builder_append(builder, slot, &left, insn)?;
            return self.builder_append(builder, slot, &right, insn);
        }
        let ty = self
            .input
            .type_of_expr(Facts::span(expr.syntax()))
            .cloned()
            .ok_or(WasmError::Unsupported(
                "a concatenation operand with no inferred type",
            ))?;
        insn.local_get(slot);
        self.expr(expr, insn)?.ok_or(WasmError::Unsupported(
            "a concatenation operand with no value",
        ))?;
        self.append_top(builder, &ty, insn)
    }

    /// `builder.toString()`, leaving a `String` on the stack.
    fn builder_string(&self, builder: ItemId, slot: u32, insn: &mut Insn) -> Result<ValType> {
        let missing = || WasmError::Unresolved("java.lang.StringBuilder.toString()".to_owned());
        let to_string = self
            .index
            .own_members(builder)
            .iter()
            .copied()
            .find(|&id| {
                let info = self.index.member(id);
                info.kind == DefKind::Method
                    && !info.modifiers.is_static
                    && info.name == "toString"
                    && self.index.resolved_param_tys(id).is_empty()
            })
            .ok_or_else(missing)?;
        let function = self
            .layout
            .functions
            .get(&to_string)
            .copied()
            .ok_or_else(missing)?;
        insn.local_get(slot).call(function);
        let string = self
            .index
            .item_by_fqn("java.lang.String")
            .ok_or_else(|| WasmError::NoRepresentation("java.lang.String".to_owned()))?;
        self.layout.class_ref(string)
    }

    /// Call the `append` overload the operand's static type names, with the builder and the value
    /// already on the stack.
    fn append_top(&self, builder: ItemId, ty: &Ty, insn: &mut Insn) -> Result<()> {
        let member = self.append_overload(builder, ty)?;
        let function = self.layout.functions.get(&member).copied().ok_or_else(|| {
            let info = self.index.member(member);
            WasmError::Unresolved(alloc::format!(
                "{}.{}",
                self.index.item(info.owner).fqn,
                info.name
            ))
        })?;
        insn.call(function).drop();
        Ok(())
    }

    /// The `append` overload one concatenation operand needs.
    ///
    /// The choice is §15.18.1's: the operand's own type names the overload, and only that overload.
    /// A `byte` or a `short` has none of its own — the JLS widens it to `int` first — and a
    /// reference that is not a `String` goes to `append(Object)`, which is the one that runs
    /// `String.valueOf` and renders a `null` as `"null"` rather than throwing.
    fn append_overload(&self, builder: ItemId, ty: &Ty) -> Result<MemberId> {
        let wanted = self.appended(ty);
        let member = self.index.own_members(builder).iter().copied().find(|&id| {
            let info = self.index.member(id);
            info.kind == DefKind::Method
                && !info.modifiers.is_static
                && info.name == "append"
                && matches!(
                    self.index.resolved_param_tys(id).as_slice(),
                    [only] if self.appended_matches(only, &wanted)
                )
        });
        member.ok_or_else(|| {
            WasmError::Unresolved(alloc::format!(
                "java.lang.StringBuilder.append({})",
                wanted.name()
            ))
        })
    }

    /// The overload shape one operand's static type names.
    fn appended(&self, ty: &Ty) -> Appended {
        match ty {
            Ty::Primitive(Primitive::Byte | Primitive::Short) => {
                Appended::Primitive(Primitive::Int)
            }
            Ty::Primitive(primitive) => Appended::Primitive(*primitive),
            _ if self.is_string_ty(ty) => Appended::Named("java.lang.String"),
            _ => Appended::Named("java.lang.Object"),
        }
    }

    /// Whether a resolved parameter type is the overload shape one operand names.
    fn appended_matches(&self, param: &Ty, wanted: &Appended) -> bool {
        match (param, wanted) {
            (Ty::Primitive(param), Appended::Primitive(wanted)) => param == wanted,
            (Ty::Class(_), Appended::Named(name)) => self.class_name(param) == Some(*name),
            _ => false,
        }
    }

    /// Whether `ty` is `java.lang.String`, however it was named.
    ///
    /// Both spellings count, for the reason the JVM backend's twin gives: a concatenation's own
    /// type comes out of inference as an *external* `String` — the operator synthesises it rather
    /// than reading it off a declaration — while a name written in source resolves to the class the
    /// index holds.
    fn is_string_ty(&self, ty: &Ty) -> bool {
        matches!(self.class_name(ty), Some("String" | "java.lang.String"))
    }

    /// Whether the expression's recorded type is a `String`.
    fn is_string_node(&self, node: &SyntaxNode) -> bool {
        self.input
            .type_of_expr(Facts::span(node))
            .is_some_and(|ty| self.is_string_ty(ty))
    }

    /// The fully-qualified name a class type has, resolved where the index can be.
    fn class_name<'t>(&'t self, ty: &'t Ty) -> Option<&'t str> {
        match ty {
            Ty::Class(ClassTy::Project { id, .. }) => Some(self.index.item(*id).fqn.as_str()),
            Ty::Class(ClassTy::External { name, .. }) => Some(name.as_str()),
            _ => None,
        }
    }

    /// `e instanceof T`.
    ///
    /// `ref.test` answers it, with one difference the lowering has to close: its nullable form is
    /// *true* for a `null`, and Java's `instanceof` is false for one. So the non-nullable form is used,
    /// which is exactly the question Java asks.
    fn instance_of(&mut self, binary: &ast::BinaryExpr, insn: &mut Insn) -> Result<ValType> {
        use jals_syntax::SyntaxKind::{RECORD_PATTERN, TYPE_PATTERN, UNNAMED_PATTERN};
        let operand = binary
            .lhs()
            .ok_or(WasmError::Unsupported("an `instanceof` with no operand"))?;
        let pattern = binary.syntax().children().find(|child| {
            matches!(
                child.kind(),
                TYPE_PATTERN | RECORD_PATTERN | UNNAMED_PATTERN
            )
        });
        // A plain type test binds nothing, so it is the test and nothing else.
        let Some(pattern) = pattern else {
            let ty = binary
                .syntax()
                .children()
                .find_map(ast::Type::cast)
                .ok_or(WasmError::Unsupported("an `instanceof` with no type"))?;
            let target = self.named_type(&ty)?;
            self.expr(&operand, insn)?
                .ok_or(WasmError::Unsupported("an `instanceof` on nothing"))?;
            insn.ref_test(target, false);
            return Ok(ValType::I32);
        };
        let operand_ty = self
            .expr(&operand, insn)?
            .ok_or(WasmError::Unsupported("an `instanceof` on nothing"))?;
        let scratch = self.scratch(operand_ty);
        let answer = self.scratch(ValType::I32);
        insn.local_set(scratch);
        // A wasm local starts at its type's default, so a binding the match did not reach needs nothing
        // arranged for it — unlike the JVM's, where the verifier merges both paths at the join.
        insn.i32_const(0).local_set(answer);
        insn.block();
        let fail = insn.depth();
        self.match_pattern(&pattern, scratch, fail, None, insn)?;
        insn.i32_const(1).local_set(answer);
        insn.end();
        insn.local_get(answer);
        Ok(ValType::I32)
    }

    /// Match `pattern` against the value in `value`, branching out to `fail` when it does not.
    ///
    /// Falls through on a match, with every binding written. A *record* pattern is the recursive case:
    /// it tests the type, then reads each component through its *accessor* — which is what a
    /// deconstruction calls (§14.30.1), a record being free to declare one by hand — and matches the
    /// component pattern against that.
    fn match_pattern(
        &mut self,
        pattern: &SyntaxNode,
        value: u32,
        fail: u32,
        declared: Option<&Ty>,
        insn: &mut Insn,
    ) -> Result<()> {
        use jals_syntax::SyntaxKind::{RECORD_PATTERN, TYPE_PATTERN, UNNAMED_PATTERN};
        match pattern.kind() {
            // `_` matches anything and binds nothing, so there is nothing to emit.
            UNNAMED_PATTERN => Ok(()),
            TYPE_PATTERN => {
                let bound = self
                    .facts()
                    .def_at(pattern)
                    .ok_or(WasmError::Unsupported("a pattern with no binding"))?;
                let bound_ty = self.input.type_of_def(bound).clone();
                let slot = self.declare_local(bound)?;
                // Two cases carry no test. A primitive one because a `ref` instruction over it is not a
                // program. And a component pattern of the component's *own* type because it matches
                // unconditionally (§14.30.2) — including a `null` component, which a `ref.test` would
                // reject and so drop a match Java makes.
                if matches!(bound_ty, Ty::Primitive(_)) || declared == Some(&bound_ty) {
                    insn.local_get(value).local_set(slot);
                    return Ok(());
                }
                let ty = pattern
                    .children()
                    .find_map(ast::Type::cast)
                    .ok_or(WasmError::Unsupported("a pattern with no type"))?;
                let target = self.named_type(&ty)?;
                insn.local_get(value).ref_test(target, false).i32_eqz();
                insn.br_if(insn.depth() - fail);
                insn.local_get(value).ref_cast(target, false);
                insn.local_set(slot);
                Ok(())
            }
            RECORD_PATTERN => {
                let ty = pattern
                    .children()
                    .find_map(ast::Type::cast)
                    .ok_or(WasmError::Unsupported("a `record` pattern with no type"))?;
                let target = self.named_type(&ty)?;
                let item = self
                    .index
                    .resolve_type_name(
                        self.input.file(),
                        &ty.simple_name()
                            .ok_or(WasmError::Unsupported("a type with no name"))?,
                        None,
                    )
                    .project_id()
                    .ok_or(WasmError::Unsupported("a `record` pattern on no record"))?;
                insn.local_get(value).ref_test(target, false).i32_eqz();
                insn.br_if(insn.depth() - fail);
                let narrowed = self.scratch(self.layout.class_ref(item)?);
                insn.local_get(value).ref_cast(target, false);
                insn.local_set(narrowed);
                // The components in header order, which is the order the sub-patterns are written in.
                let components: Vec<MemberId> = self
                    .index
                    .own_members(item)
                    .iter()
                    .copied()
                    .filter(|&member| {
                        let info = self.index.member(member);
                        info.kind == DefKind::Field && !info.modifiers.is_static
                    })
                    .collect();
                let subs: Vec<SyntaxNode> = pattern
                    .children()
                    .filter(|child| {
                        matches!(
                            child.kind(),
                            TYPE_PATTERN | RECORD_PATTERN | UNNAMED_PATTERN
                        )
                    })
                    .collect();
                if subs.len() != components.len() {
                    return Err(WasmError::Unsupported(
                        "a `record` pattern of the wrong arity",
                    ));
                }
                for (component, sub) in components.iter().zip(&subs) {
                    let name = self.index.member(*component).name.clone();
                    let accessor = self
                        .index
                        .own_members(item)
                        .iter()
                        .copied()
                        .find(|&member| {
                            let info = self.index.member(member);
                            info.kind == DefKind::Method
                                && info.name == name
                                && info.params.is_empty()
                        })
                        .ok_or(WasmError::Unsupported(
                            "a record component with no accessor",
                        ))?;
                    let function = *self
                        .layout
                        .functions
                        .get(&accessor)
                        .ok_or(WasmError::Unsupported("a record accessor with no body"))?;
                    let held = self.scratch(
                        self.layout
                            .val_type(&self.index.resolved_member_ty(*component))?,
                    );
                    insn.local_get(narrowed).call(function);
                    insn.local_set(held);
                    let component_ty = self.index.resolved_member_ty(*component);
                    self.match_pattern(sub, held, fail, Some(&component_ty), insn)?;
                }
                Ok(())
            }
            _ => Err(WasmError::Unsupported("an `instanceof` pattern")),
        }
    }

    /// The declared heap type a `TYPE` node names.
    fn named_type(&self, ty: &ast::Type) -> Result<HeapType> {
        let name = ty
            .simple_name()
            .ok_or(WasmError::Unsupported("a type with no name"))?;
        let qualified = ty.is_qualified().then(|| ty.qualified_text()).flatten();
        let item = self
            .index
            .resolve_type_name(self.input.file(), &name, qualified.as_deref())
            .project_id()
            .ok_or_else(|| WasmError::Unresolved(name.clone()))?;
        // An interface is `anyref`, exactly as `Object` is: wasm has no interface types, so a cast
        // to one is a cast to the type every reference already has. The same answer `val_type`
        // gives, and it has to be — the two describe the same value.
        if self.layout.interfaces.contains(&item) || self.layout.object == Some(item) {
            return Ok(HeapType::Any);
        }
        let index = self
            .layout
            .structs
            .get(&item)
            .ok_or(WasmError::NoRepresentation(name))?;
        Ok(HeapType::Concrete(*index))
    }

    /// An assignment, simple or compound. Returns the assigned type when `keep` asked for the value.
    fn assignment(
        &mut self,
        assignment: &ast::AssignmentExpr,
        keep: bool,
        insn: &mut Insn,
    ) -> Result<Option<ValType>> {
        let target = assignment
            .target()
            .ok_or(WasmError::Unsupported("an assignment with no target"))?;
        let value = assignment
            .value()
            .ok_or(WasmError::Unsupported("an assignment with no value"))?;
        let place = self.place(&target, insn)?;

        if assignment.is_simple() {
            place.address(insn);
            let source = self.num_of(value.syntax()).ok();
            let produced = self
                .expr(&value, insn)?
                .ok_or(WasmError::Unsupported("an assignment of no value"))?;
            // Assignment conversion (JLS §5.2): `long n = 1` stores a `long`, and the literal is an
            // `int` until something widens it. A *numeric* target is converted here, at the source's
            // own width, because `byte` / `short` / `char` need the sign extension or the mask that
            // a wasm value type — where all four are `i32` — cannot ask for.
            //
            // A reference target is not "already the right type": that held only while every
            // reference the backend produced was concrete. Erasure puts an `anyref` on the stack
            // wherever a type variable is involved, and a field or an array element declared at a
            // concrete type is a place the validator checks exactly. `b.held = id(c);` was a module
            // `wasm-tools` refuses, emitted with nothing said on this side.
            if let (Some(source), Ok(declared)) = (source, self.num_of(target.syntax())) {
                if source != declared {
                    insn.convert(source, declared)
                        .ok_or(WasmError::Unsupported("this assignment conversion"))?;
                }
            } else {
                self.coerce(&value, produced, place.ty(), insn)?;
            }
            place.store(insn, keep);
        } else {
            let operation = Self::compound_operator(assignment.syntax())?;
            // A `String +=` is a concatenation, not arithmetic: it is `s = s + value`, and the
            // builder does the rendering. Caught here because `compound`'s first act is
            // `num_of(target)`, which has no answer for a `String`.
            if operation == NumOp::Add && self.is_string_node(target.syntax()) {
                place.address(insn);
                let ty = self
                    .input
                    .type_of_expr(Facts::span(target.syntax()))
                    .cloned()
                    .ok_or(WasmError::Unsupported(
                        "a concatenation target with no inferred type",
                    ))?;
                let builder = self.string_builder()?;
                let slot = self.builder_start(builder, insn)?;
                insn.local_get(slot);
                place.read(insn);
                self.append_top(builder, &ty, insn)?;
                self.builder_append(builder, slot, &value, insn)?;
                self.builder_string(builder, slot, insn)?;
                place.store(insn, keep);
            } else {
                self.compound(&place, &target, &value, operation, keep, insn)?;
            }
        }

        if keep { Ok(Some(place.ty())) } else { Ok(None) }
    }

    /// The operator a compound assignment applies. `=` is not one of them.
    fn compound_operator(node: &SyntaxNode) -> Result<NumOp> {
        // The token run is read once, in `facts`; what is left here is the projection onto this
        // target's opcode vocabulary. A compound assignment never spells a comparison, so the
        // arms a comparison would need are unreachable rather than missing.
        Operator::compound(node)
            .and_then(Self::num_op)
            .ok_or(WasmError::Unsupported("this compound assignment operator"))
    }

    /// This target's opcode for a source operator, or `None` for one that is not an operation over
    /// two values.
    ///
    /// wasm fuses arithmetic and comparison — `i32.add` and `i32.lt_s` are the same shape of
    /// instruction — where the JVM splits them, so the two backends project the one vocabulary
    /// differently. That difference is emission and stays here.
    const fn num_op(operator: Operator) -> Option<NumOp> {
        Some(match operator {
            Operator::Add => NumOp::Add,
            Operator::Sub => NumOp::Sub,
            Operator::Mul => NumOp::Mul,
            Operator::Div => NumOp::Div,
            Operator::Rem => NumOp::Rem,
            Operator::And => NumOp::And,
            Operator::Or => NumOp::Or,
            Operator::Xor => NumOp::Xor,
            Operator::Shl => NumOp::Shl,
            Operator::Shr => NumOp::Shr,
            Operator::Ushr => NumOp::Ushr,
            Operator::Eq => NumOp::Eq,
            Operator::Ne => NumOp::Ne,
            Operator::Lt => NumOp::Lt,
            Operator::Le => NumOp::Le,
            Operator::Gt => NumOp::Gt,
            Operator::Ge => NumOp::Ge,
            // Not operations over two values: the short-circuits may not evaluate their right
            // operand at all, and `instanceof`'s right side is a type or a pattern.
            Operator::AndAnd | Operator::OrOr | Operator::InstanceOf => return None,
        })
    }

    /// `E1 op= E2`, which JLS §15.26.2 defines as `E1 = (T)((E1) op (E2))` for `E1`'s type `T`.
    ///
    /// Both conversions carry weight: the operator runs at the *promoted* type, so `int i; i += 1L`
    /// widens `i` to `i64` and wraps the sum back, and `byte b; b += 1` adds as `i32` and sign-extends
    /// the low byte. Dropping either stores a value outside the variable's range in a module that
    /// validates.
    fn compound(
        &mut self,
        place: &Place,
        target: &ast::Expr,
        value: &ast::Expr,
        operation: NumOp,
        keep: bool,
        insn: &mut Insn,
    ) -> Result<()> {
        let declared = self.num_of(target.syntax())?;
        place.address(insn);
        place.read(insn);
        let promoted = if operation.is_shift() {
            Numeric::promote_one(declared)
        } else {
            Numeric::promote(declared, self.num_of(value.syntax())?)
        };
        if declared != promoted {
            insn.convert(declared, promoted)
                .ok_or(WasmError::Unsupported("this conversion"))?;
        }
        self.operand(value, promoted, insn)?;
        insn.numeric(operation, promoted.val())
            .ok_or(WasmError::Unsupported("this operator on this type"))?;
        if promoted != declared {
            insn.convert(promoted, declared)
                .ok_or(WasmError::Unsupported("this narrowing"))?;
        }
        place.store(insn, keep);
        Ok(())
    }

    /// `++e` / `--e` / `e++` / `e--`.
    ///
    /// The postfix and prefix forms differ only in *when* the place is read for the result, and with no
    /// `dup` that difference is a read before the write rather than after it.
    fn update(
        &mut self,
        target: &ast::Expr,
        delta: i8,
        prefix: bool,
        keep: bool,
        insn: &mut Insn,
    ) -> Result<Option<ValType>> {
        let declared = self.num_of(target.syntax())?;
        let place = self.place(target, insn)?;
        // The postfix form yields the value the variable *had*, so it is spilled before the store.
        let previous = if keep && !prefix {
            let slot = self.scratch(place.ty());
            place.read(insn);
            insn.local_set(slot);
            Some(slot)
        } else {
            None
        };

        // `++` is `+= 1` with the same promotion and narrowing (§15.14.2), so a `char c; c++` adds as
        // `i32` and truncates back into a `char`.
        let promoted = Numeric::promote_one(declared);
        place.address(insn);
        place.read(insn);
        if declared != promoted {
            insn.convert(declared, promoted)
                .ok_or(WasmError::Unsupported("this conversion"))?;
        }
        Self::one(delta, promoted, insn);
        insn.numeric(NumOp::Add, promoted.val())
            .ok_or(WasmError::Unsupported("an increment of this type"))?;
        if promoted != declared {
            insn.convert(promoted, declared)
                .ok_or(WasmError::Unsupported("this narrowing"))?;
        }
        // The prefix form's result is the *new* value, which `store` can keep on the way past.
        place.store(insn, keep && prefix);

        match (keep, previous) {
            (false, _) => Ok(None),
            (true, Some(slot)) => {
                insn.local_get(slot);
                Ok(Some(place.ty()))
            }
            (true, None) => Ok(Some(place.ty())),
        }
    }

    /// The `1` (or `-1`) an increment adds, at the promoted type.
    fn one(delta: i8, promoted: Numeric, insn: &mut Insn) {
        match promoted {
            Numeric::Long => insn.i64_const(i64::from(delta)),
            Numeric::Float => insn.f32_const(f32::from(delta)),
            Numeric::Double => insn.f64_const(f64::from(delta)),
            Numeric::Byte | Numeric::Short | Numeric::Char | Numeric::Int => {
                insn.i32_const(i32::from(delta))
            }
        };
    }

    /// Where an assignment's target lives, with its subexpressions already evaluated.
    ///
    /// This is the point of the type: a compound assignment reads its target *and* writes it, and
    /// §15.26.2 evaluates the target's subexpressions exactly once. With no `dup` to duplicate an
    /// address, the receiver of a field access and the array and index of a subscript are spilled into
    /// scratch locals here, and each access reads them back. The scratch locals are fresh per site, so
    /// `a[i++] += b[j++]` nests two of these without either clobbering the other.
    fn place(&mut self, target: &ast::Expr, insn: &mut Insn) -> Result<Place> {
        let ty = self.ty_of(target.syntax())?;
        match target {
            ast::Expr::Paren(paren) => {
                let inner = paren
                    .expr()
                    .ok_or(WasmError::Unsupported("an empty parenthesis"))?;
                self.place(&inner, insn)
            }
            ast::Expr::Index(subscript) => {
                let mut parts = subscript.parts();
                let array = parts
                    .next()
                    .ok_or(WasmError::Unsupported("an index with no array"))?;
                let index = parts
                    .next()
                    .ok_or(WasmError::Unsupported("an index with no subscript"))?;
                let array_type = self
                    .layout
                    .array_type(ty)
                    .ok_or_else(|| WasmError::NoRepresentation("an array".to_owned()))?;
                let array_value = self.expr(&array, insn)?.ok_or(WasmError::Unsupported(
                    "an index into something with no value",
                ))?;
                let array_slot = self.scratch(array_value);
                insn.local_set(array_slot);
                self.operand(&index, Numeric::Int, insn)?;
                let index_slot = self.scratch(ValType::I32);
                insn.local_set(index_slot);
                Ok(Place::Element {
                    array: array_slot,
                    index: index_slot,
                    array_type,
                    ty,
                })
            }
            ast::Expr::FieldAccess(access) => {
                // `a.length` is not a field, so there is no member for the index to have resolved.
                // Reporting the *name* as unresolved named the wrong problem: `length` is final
                // (JLS §10.7), so there is nothing to assign to rather than nothing to find.
                if self.facts().is_array_length(access) {
                    return Err(WasmError::Unsupported("an assignment to an array's length"));
                }
                let (owner, member) = self.field_target(access)?;
                if self.index.member(member).modifiers.is_static {
                    if let Some(&(get, put)) = self.layout.external_statics.get(&member) {
                        return Ok(Place::External { get, put, ty });
                    }
                    let global =
                        *self.layout.statics.get(&member).ok_or_else(|| {
                            WasmError::Unresolved(access.field().unwrap_or_default())
                        })?;
                    self.ensure_initialised(member, insn);
                    return Ok(Place::Global { index: global, ty });
                }
                let slot = self.layout.field_slot_or(
                    self.index,
                    owner,
                    member,
                    access.field().unwrap_or_default(),
                )?;
                let receiver = access.receiver().ok_or(WasmError::Unsupported(
                    "a field assignment with no receiver",
                ))?;
                // At the *owner's* type: a receiver read through an interface, an `Object`, or a
                // type variable is `anyref`, and `struct.set` names one struct in particular.
                let receiver_ty = self.layout.class_ref(owner)?;
                self.expr_as(&receiver, receiver_ty, insn)?;
                let receiver_slot = self.scratch(receiver_ty);
                insn.local_set(receiver_slot);
                Ok(Place::Field {
                    receiver: receiver_slot,
                    struct_type: self.layout.structs[&owner],
                    slot,
                    ty,
                })
            }
            ast::Expr::NameRef(name) => {
                let text = name.syntax().text().to_string();
                let unresolved = || WasmError::Unresolved(text.trim().into());
                let member = match self.facts().def_at(name.syntax()) {
                    Some(id) => {
                        if let Some(slot) = self.slot_of(id) {
                            return Ok(Place::Local { slot, ty });
                        }
                        // A bare name that is no local is a field of the enclosing class. A `static`
                        // one is a global; an instance one needs no spill, local 0 being a stable
                        // receiver already.
                        self.facts().member_of_def(id)
                    }
                    // Nothing in the file declared it, which an *inherited* field never is.
                    None => self.inherited_field(name.syntax()),
                };
                let member = member.ok_or_else(unresolved)?;
                if self.index.member(member).modifiers.is_static {
                    if let Some(&(get, put)) = self.layout.external_statics.get(&member) {
                        return Ok(Place::External { get, put, ty });
                    }
                    let global = *self.layout.statics.get(&member).ok_or_else(unresolved)?;
                    self.ensure_initialised(member, insn);
                    return Ok(Place::Global { index: global, ty });
                }
                let owner = self.owner.ok_or_else(unresolved)?;
                let declared = self.index.member(member).owner;
                // The common case: the field is this class's own or one it inherits, and local 0 is
                // a stable receiver already — no spill, exactly as before.
                if self.index.is_subtype(owner, declared) {
                    let slot = self
                        .layout
                        .field_slot(owner, member)
                        .ok_or_else(unresolved)?;
                    return Ok(Place::Field {
                        receiver: 0,
                        struct_type: self.layout.structs[&owner],
                        slot,
                        ty,
                    });
                }
                // Otherwise the name reaches out through enclosing instances. The walk leaves the
                // receiver on the stack and a place needs it in a local, because the value being
                // assigned is evaluated *after* the place is resolved.
                let item = self.load_unqualified_receiver(declared, insn)?;
                let slot = self
                    .layout
                    .field_slot(item, member)
                    .ok_or_else(unresolved)?;
                let receiver_ty = self.layout.class_ref(item)?;
                let receiver = self.scratch(receiver_ty);
                insn.local_set(receiver);
                Ok(Place::Field {
                    receiver,
                    struct_type: self.layout.structs[&item],
                    slot,
                    ty,
                })
            }
            _ => Err(WasmError::Unsupported("assignment to this target")),
        }
    }

    /// `c ? a : b`.
    ///
    /// A typed `if` rather than `select`, because `select` pops both value operands: both arms would
    /// already have run, and a trapping one would trap whether or not it was taken. §15.25 evaluates
    /// exactly one arm.
    fn ternary(&mut self, expr: &ast::TernaryExpr, insn: &mut Insn) -> Result<ValType> {
        let mut parts = expr.parts();
        let condition = parts
            .next()
            .ok_or(WasmError::Unsupported("a `?:` with no condition"))?;
        let then_arm = parts
            .next()
            .ok_or(WasmError::Unsupported("a `?:` with no then arm"))?;
        let else_arm = parts
            .next()
            .ok_or(WasmError::Unsupported("a `?:` with no else arm"))?;
        let ty = self.ty_of(expr.syntax())?;

        self.condition(&condition, insn)?;
        insn.if_typed(ty);
        self.ternary_arm(&then_arm, ty, insn)?;
        insn.else_();
        self.ternary_arm(&else_arm, ty, insn)?;
        insn.end();
        Ok(ty)
    }

    /// One arm of a `?:`, converted to the type the whole conditional has.
    ///
    /// The conversion is what makes `flag ? 1 : 2L` one `i64` block rather than a module the validator
    /// rejects for arms of different types — and what boxes an `int` arm into the `Integer` the
    /// other arm made the conditional's type.
    fn ternary_arm(&mut self, arm: &ast::Expr, ty: ValType, insn: &mut Insn) -> Result<()> {
        let produced = self
            .expr(arm, insn)?
            .ok_or(WasmError::Unsupported("a `?:` arm with no value"))?;
        self.coerce(arm, produced, ty, insn)
    }

    /// The numeric type a `ValType` is, for converting into a type an expression already has.
    const fn num_for(ty: ValType) -> Result<Numeric> {
        Ok(match ty {
            ValType::I32 => Numeric::Int,
            ValType::I64 => Numeric::Long,
            ValType::F32 => Numeric::Float,
            ValType::F64 => Numeric::Double,
            ValType::Ref(_) => return Err(WasmError::Unsupported("a numeric reference")),
        })
    }

    /// `a && b` / `a || b`, which evaluate `b` only when `a` did not already decide the answer.
    ///
    /// A typed `if` again, and for the same reason: the whole point of the operators is that the right
    /// operand may not run.
    fn short_circuit(
        &mut self,
        left: &ast::Expr,
        right: &ast::Expr,
        and: bool,
        insn: &mut Insn,
    ) -> Result<ValType> {
        self.condition(left, insn)?;
        insn.if_typed(ValType::I32);
        if and {
            self.condition(right, insn)?;
            insn.else_().i32_const(0);
        } else {
            insn.i32_const(1).else_();
            self.condition(right, insn)?;
        }
        insn.end();
        Ok(ValType::I32)
    }

    /// `new C(args)`: allocate with every field at its default, then run the constructor on it.
    ///
    /// The allocation is `struct.new_default`, and from that instruction on the object belongs to
    /// the host's collector. There is no header, no allocation site bookkeeping, and nothing to
    /// free.
    fn new_object(&mut self, new: &ast::NewExpr, insn: &mut Insn) -> Result<ValType> {
        let ty = self.ty_of(new.syntax())?;
        // `new T[n]`: one instruction, and every element starts at its type's default — which is
        // exactly Java's rule for a fresh array.
        if let Some(Ty::Array(element)) =
            self.input.type_of_expr(Facts::span(new.syntax())).cloned()
        {
            // `new T[] { … }` writes its elements out, and the initialiser *is* the array — one
            // `array.new_fixed` with the values on the stack. Reading its node as the length instead
            // built the array twice: `array.new_fixed` and then `array.new_default` consuming it as
            // a count, which is a reference where an `i32` belongs.
            if let Some(init) = new.syntax().children().find_map(ast::ArrayInit::cast) {
                let written = Ty::Array(element);
                return self.array_initializer(&init, Some(&written), insn);
            }
            let element = self.layout.val_type(&element)?;
            let array_type = self
                .layout
                .array_type(element)
                .ok_or_else(|| WasmError::NoRepresentation("an array".to_owned()))?;
            let length = new
                .syntax()
                .children()
                .find_map(ast::Expr::cast)
                .ok_or(WasmError::Unsupported("an array creation with no length"))?;
            self.expr(&length, insn)?;
            insn.array_new_default(array_type);
            return Ok(ty);
        }
        // An anonymous class is its own type, and the `new` builds *that* rather than the type it named.
        let anonymous = Facts::is_anonymous_body(new.syntax());
        let item = if anonymous {
            self.index
                .item_by_decl(self.input.file(), Facts::span(new.syntax()).start)
                .ok_or(WasmError::Unsupported("an anonymous class with no item"))?
        } else {
            self.input
                .type_of_expr(Facts::span(new.syntax()))
                .and_then(Ty::project_id)
                .ok_or(WasmError::Unsupported("a `new` of an unindexed type"))?
        };
        // The expression's *inferred* type is the interface the `new` named, which is held as `anyref` —
        // but the value on the stack is the anonymous struct, and anything that writes its fields needs
        // the concrete type. The subtyping makes the two interchangeable everywhere else, which is why
        // only the capture stores noticed.
        let ty = if anonymous {
            self.layout.class_ref(item)?
        } else {
            ty
        };
        let struct_type =
            *self.layout.structs.get(&item).ok_or_else(|| {
                WasmError::NoRepresentation(self.index.item(item).fqn.to_string())
            })?;

        // An inner class's constructor needs the enclosing instance. Only the unqualified form is
        // lowered: `outer.new Inner()` names a *different* enclosing instance, and taking `this`
        // regardless would build the object against the wrong one — wrong state, silently.
        let encloses = self.layout.inner.get(&item).copied();
        // `outer.new Inner()` names the enclosing instance explicitly; the qualifier is an expression
        // sitting *before* the `new` keyword. Unqualified, it is `this`, which a `static` method has not
        // got.
        let qualifier = encloses.and_then(|_| Facts::new_qualifier(new));
        if encloses.is_some() && qualifier.is_none() && self.owner.is_none() {
            return Err(WasmError::Unsupported(
                "a `new` of an inner class outside an instance method",
            ));
        }
        let arguments: Vec<ast::Expr> = new
            .syntax()
            .children()
            .find_map(ast::ArgList::cast)
            .map(|list| list.args().collect())
            .unwrap_or_default();
        // A linked library's class is built by the library: its constructor's in-module shape
        // leads with a `this` no consumer has, so the export is a factory that allocates, runs
        // the constructor and returns the object. Nothing here lays the struct out.
        if self.layout.external_classes.contains_key(&item) {
            let selected = self
                .input
                .call_target_of(Facts::span(new.syntax()))
                .filter(|&member| self.index.member(member).kind == DefKind::Constructor);
            let member = selected
                .or_else(|| {
                    self.index
                        .own_members(item)
                        .iter()
                        .copied()
                        .find(|&member| self.index.member(member).kind == DefKind::Constructor)
                })
                .ok_or(WasmError::Unsupported(
                    "a linked library class with no constructor",
                ))?;
            let factory = self
                .layout
                .external_constructors
                .get(&member)
                .copied()
                .ok_or(WasmError::Unsupported(
                    "a constructor a linked library does not export",
                ))?;
            // An inner class's factory leads with the enclosing instance — the in-module shape's
            // first parameter after `this` — because the consumer has no `this` to pass. The
            // qualified form is the only one that can name a *library* outer class, and the
            // unqualified one refuses in `enclosing_instance` rather than pushing the wrong value.
            if let Some(&enclosing) = self.layout.inner.get(&item) {
                self.enclosing_instance(qualifier.as_ref(), enclosing, insn)?;
            }
            self.push_arguments(member, &arguments, insn)?;
            insn.call(factory);
            return self.layout.class_ref(item);
        }
        // Which constructor, read from the index rather than re-picked here. Matching on argument
        // *count* alone took the first of any same-arity pair, and a second selection free to
        // disagree with the analysis is the drift `call_target_of` exists to prevent.
        // Only a constructor with a *body* counts on either side. The index also holds the default
        // constructor every class without a written one has (JLS §8.8.9), which nothing lowered a
        // function for — and resolving to that one is the same case as resolving to none, which is
        // the second arm below.
        let constructor = self
            .input
            .call_target_of(Facts::span(new.syntax()))
            .filter(|target| self.layout.functions.contains_key(target));
        let declares_constructor = self.layout.constructors(self.index, item).next().is_some();

        insn.struct_new_default(struct_type);
        match constructor {
            Some(constructor) => {
                let function = *self
                    .layout
                    .functions
                    .get(&constructor)
                    .ok_or(WasmError::Unsupported("a constructor with no body"))?;
                // The receiver has to survive the call, so it is stored and re-read rather than
                // duplicated: wasm has no `dup`. `local.set` and not `local.tee` — a `tee` leaves
                // the value as well, and the copy it left behind outlived the call, so `new`
                // finished one value deep. A trailing `return` discards a surplus, which is why
                // that only surfaced once a `new` sat inside a `block`.
                let slot = self.scratch(ty);
                insn.local_set(slot).local_get(slot);
                // An **anonymous** class declares no constructor, so the one resolved here is its
                // *superclass's* and everything about the call is that class's: its enclosing
                // instance, its parameters, and — since it stores nothing of this class — none of
                // this class's captures. Passing this class's enclosing instance to a constructor
                // that does not take one is a call one argument long, which the validator refuses.
                let declaring = self.index.member(constructor).owner;
                if let Some(&needed) = self.layout.inner.get(&declaring) {
                    self.enclosing_instance(qualifier.as_ref(), needed, insn)?;
                }
                self.push_arguments(constructor, &arguments, insn)?;
                // The captures the *callee* declares, which for an anonymous class's `new` are its
                // superclass's rather than its own: the function being called is that class's, and
                // its trailing parameters are the locals *it* captured.
                self.push_captures(declaring, insn)?;
                insn.call(function);
                insn.local_get(slot);
                // What that constructor did not write because it belongs to another class: this
                // class's own enclosing instance and captures. Its own constructor wrote both from
                // its parameters, so there is nothing left over there.
                if declaring != item {
                    self.fill_synthetic_fields(item, struct_type, ty, qualifier.as_ref(), insn)?;
                }
            }
            // No declared constructor: the implicit default one initialises nothing, so the
            // allocation is already the finished object — except for an inner class, whose synthetic
            // field is written here because there is no constructor function to write it.
            None if !declares_constructor && arguments.is_empty() => {
                // No constructor at all, so nothing else will run the field initialisers: without this a
                // `class Box { int value = 9; }` read back as 0 — a wrong value in a module that validates.
                // Its own synthesised constructor, or — when it has no initialisers of its own — the
                // nearest ancestor's, whose initialisers still have to run.
                if let Some(initialise) =
                    Body::inherited_initialiser(item, self.index, self.layout)?
                {
                    let slot = self.scratch(ty);
                    insn.local_set(slot).local_get(slot).call(initialise);
                    insn.local_get(slot);
                }
                self.fill_synthetic_fields(item, struct_type, ty, qualifier.as_ref(), insn)?;
            }
            None => return Err(WasmError::Unresolved("a matching constructor".into())),
        }
        Ok(ty)
    }

    /// Write the synthetic fields no constructor function wrote, onto the value on top of the stack,
    /// and leave it there.
    ///
    /// Two cases reach here and they are the same case: a class with no constructor at all, and a
    /// class whose `new` ran *another* class's constructor. An anonymous class is the second — it
    /// declares none, so the constructor a `new` of it calls is its superclass's, and that function
    /// stores nothing about the class actually being built.
    fn fill_synthetic_fields(
        &mut self,
        item: ItemId,
        struct_type: u32,
        ty: ValType,
        qualifier: Option<&ast::Expr>,
        insn: &mut Insn,
    ) -> Result<()> {
        let captured = self.layout.captures.get(&item).cloned().unwrap_or_default();
        if !captured.is_empty() {
            let slot = self.scratch(ty);
            let first = *self
                .layout
                .capture_slot
                .get(&item)
                .ok_or(WasmError::Unsupported("a capture with no field"))?;
            insn.local_set(slot);
            for (offset, (id, _)) in captured.iter().enumerate() {
                insn.local_get(slot);
                self.push_capture(*id, insn)?;
                let field = first + u32::try_from(offset).map_err(|_| WasmError::TooLarge)?;
                insn.struct_set(struct_type, field);
            }
            insn.local_get(slot);
        }
        if let Some(&encloses) = self.layout.inner.get(&item) {
            let slot = self.scratch(ty);
            let field = self
                .layout
                .outer
                .get(&item)
                .copied()
                .ok_or(WasmError::Unsupported("an inner class with no outer field"))?;
            insn.local_set(slot);
            insn.local_get(slot);
            self.enclosing_instance(qualifier, encloses, insn)?;
            insn.struct_set(struct_type, field);
            insn.local_get(slot);
        }
        Ok(())
    }

    /// The layout's answer, for a call site this lowering is emitting — see
    /// [`Layout::overriders`], which is the one implementation and the one explanation.
    fn overriders(&self, member: MemberId) -> Vec<(ItemId, MemberId)> {
        self.layout.overriders(self.index, member)
    }

    /// A virtual call: test the receiver's actual type against each override, most-derived first, and
    /// fall through to the statically-selected method when none matches.
    ///
    /// The receiver and every argument are spilled into locals first, because each arm re-pushes them
    /// and Java evaluates them exactly once. There is no vtable and no `call_ref`: with the whole
    /// project in one module the overrides are a known, closed set, so a chain of `ref.test` answers
    /// the same question a vtable would — and needs no element section to declare function references
    /// in.
    fn virtual_call(
        &mut self,
        call: &ast::CallExpr,
        member: MemberId,
        arguments: &[ast::Expr],
        dispatch: Dispatch<'_>,
        insn: &mut Insn,
    ) -> Result<Option<ValType>> {
        let Dispatch {
            overriders,
            ty,
            realm,
        } = dispatch;
        // A bare call in an instance method is an implicit `this`, which is local 0.
        let receiver_ty = if let Some(ast::Expr::FieldAccess(access)) = call.callee() {
            let receiver = access
                .receiver()
                .ok_or(WasmError::Unsupported("a call with no receiver"))?;
            self.expr(&receiver, insn)?
                .ok_or(WasmError::Unsupported("a receiver with no value"))?
        } else {
            // A bare call in an instance method is an implicit `this` — or, from a class that holds
            // an enclosing instance, whichever enclosing instance owns the method.
            let item = self.load_unqualified_receiver(self.index.member(member).owner, insn)?;
            self.layout.class_ref(item)?
        };
        let receiver = self.scratch(receiver_ty);
        insn.local_set(receiver);

        // Lowered untargeted, exactly as the direct-call path does: an argument's own inferred type is
        // what both use, so a virtual call converts no differently from a static one.
        // Each into a local of its own before the dispatch block opens: every branch calls a
        // function declared over the *parameters*, so a value erasure left at the top comes down
        // here rather than once per branch, and a variable-arity call builds its array once.
        let planned = self.plan_arguments(member, arguments)?;
        // The slot *and* what it holds: a slot index counts from the first parameter while the
        // local vector counts from the first declared local, so the type has to travel with it
        // rather than be looked up by index.
        let mut slots: Vec<(u32, ValType)> = Vec::with_capacity(planned.len());
        for arg in &planned {
            let value = self.push_argument(arg, insn)?;
            let slot = self.scratch(value);
            insn.local_set(slot);
            slots.push((slot, value));
        }

        // The realm arm, when a consumer could answer. Realm-first, because a project class that
        // extends one of this library's classes *is* a subtype of the class the chain tests for, so
        // a chain-first call would resolve to the library's body and never ask the consumer. The
        // chain stays below as the `else`: an unlinked library has no realm and runs on it alone.
        let realm = if realm {
            Some(self.realm_slot(member)?)
        } else {
            None
        };
        if let Some((field, slot_ty)) = realm {
            let build = self
                .layout
                .realm
                .as_ref()
                .ok_or(WasmError::Unsupported("a dispatch with no realm"))?;
            let (structure, global) = (build.structure, build.global);
            insn.global_get(global).ref_is_null().i32_eqz();
            match ty {
                Some(ty) => insn.if_typed(ty),
                None => insn.if_(),
            };
            insn.local_get(receiver);
            // The slot is called at the *declared* member's type — the same one
            // `declared_member_type` recorded — so the receiver and every argument come up to it
            // from wherever the expression and the plan left them.
            let slot_receiver = self.slot_receiver(member);
            self.narrow(receiver_ty, slot_receiver, insn)?;
            let declared = self.index.resolved_param_tys(member);
            for (position, &(slot, held)) in slots.iter().enumerate() {
                insn.local_get(slot);
                if let Some(ty) = declared.get(position) {
                    let want = self.layout.val_type(ty)?;
                    self.narrow(held, want, insn)?;
                }
            }
            insn.global_get(global)
                .struct_get(structure, field)
                .call_ref(slot_ty);
            insn.else_();
        }
        match ty {
            Some(ty) => insn.block_typed(ty),
            None => insn.block(),
        };
        let leave = insn.depth();
        for &(item, over) in overriders {
            let Some(&function) = self.layout.functions.get(&over) else {
                continue;
            };
            let struct_type = self.layout.structs[&item];
            insn.local_get(receiver);
            insn.ref_test(HeapType::Concrete(struct_type), false);
            insn.if_();
            insn.local_get(receiver);
            insn.ref_cast(HeapType::Concrete(struct_type), false);
            // The slots hold what the *statically selected* member's parameters wanted, and an
            // override is free to declare narrower ones — `int f(T t)` overridden as `int f(Cell c)`
            // is one function taking `anyref` and one taking a concrete struct. Each arm therefore
            // narrows again, to its own function's parameters, rather than trusting the plan the
            // selection made.
            let over_tys = self.index.resolved_param_tys(over);
            for (position, &(slot, held)) in slots.iter().enumerate() {
                insn.local_get(slot);
                if let Some(ty) = over_tys.get(position) {
                    let want = self.layout.val_type(ty)?;
                    self.narrow(held, want, insn)?;
                }
            }
            insn.call(function);
            insn.br(insn.depth() - leave);
            insn.end();
        }
        // An interface's method is abstract: there is no function to fall back to, and every class that
        // could satisfy the call is already in the chain above. Reaching here means the receiver is
        // `null` or of a type nothing implemented, which traps — the same answer `ref.cast` gives a
        // failed cast, this host having no exception model.
        match self.layout.functions.get(&member) {
            Some(&function) => {
                insn.local_get(receiver);
                // The arms above each `ref.cast` the receiver to the type they tested for; this one
                // tested nothing, so the value is still whatever the expression produced — `anyref`
                // wherever erasure put it there. `<T extends C> int g(T t) { return t.m(); }` is the
                // everyday shape, and the module it produced was refused by the validator with
                // nothing said on this side.
                let owner = self.layout.class_ref(self.index.member(member).owner)?;
                self.narrow(receiver_ty, owner, insn)?;
                for &(slot, _) in &slots {
                    insn.local_get(slot);
                }
                insn.call(function);
            }
            None => {
                insn.unreachable();
            }
        }
        insn.end();
        if realm.is_some() {
            insn.end();
        }
        Ok(ty)
    }

    /// The field index and function type of the realm slot for `member`, registering it on first
    /// use.
    ///
    /// One slot per member, in first-call order: the consumer fills one field with one dispatcher
    /// whatever the number of call sites, which is what keeps the struct small enough to build by
    /// hand. The field index is the position, and the type is the one the declaration recorded —
    /// the same type the consumer replays and calls the field at.
    fn realm_slot(&self, member: MemberId) -> Result<(u32, u32)> {
        let realm = self
            .layout
            .realm
            .as_ref()
            .ok_or(WasmError::Unsupported("a dispatch with no realm"))?;
        let ty = *self
            .layout
            .member_types
            .get(&member)
            .ok_or(WasmError::Unsupported("a dispatch with no declared type"))?;
        let mut slots = realm.slots.borrow_mut();
        let existing = slots.iter().position(|&(seen, _)| seen == member);
        let field = existing.unwrap_or_else(|| {
            slots.push((member, ty));
            slots.len() - 1
        });
        Ok((u32::try_from(field).map_err(|_| WasmError::TooLarge)?, ty))
    }

    /// The receiver type a realm slot for `member` is declared at: the owner's struct, or `anyref`
    /// when the owner is a type this module does not lay out — the same rule
    /// [`declared_member_type`](Self::declared_member_type) records the type by.
    fn slot_receiver(&self, member: MemberId) -> ValType {
        let any = ValType::Ref(RefType::nullable(HeapType::Any));
        self.layout
            .class_ref(self.index.member(member).owner)
            .unwrap_or(any)
    }

    /// Push the enclosing instance an inner class's constructor takes: the qualifier when the source
    /// wrote one, `this` otherwise.
    fn enclosing_instance(
        &mut self,
        qualifier: Option<&ast::Expr>,
        encloses: ItemId,
        insn: &mut Insn,
    ) -> Result<()> {
        match qualifier {
            Some(expr) => {
                self.expr(expr, insn)?
                    .ok_or(WasmError::Unsupported("a qualified `new` with no receiver"))?;
            }
            // Not local 0: the class being compiled need not be the one the target is declared in.
            // A `new Inner()` written inside another class nested in the same outer one passes the
            // *enclosing* instance, which this class reaches through its own synthetic field. The
            // JVM lowering takes the same walk at the same place, and for the same reason.
            None => {
                self.load_unqualified_receiver(encloses, insn)?;
            }
        }
        Ok(())
    }

    /// The type a `T::new` reference constructs.
    fn constructed_item(
        node: &SyntaxNode,
        input: &TypedFile<'_>,
        index: &ProjectIndex,
    ) -> Result<ItemId> {
        let qualifier = node
            .children()
            .find_map(ast::Expr::cast)
            .ok_or(WasmError::Unsupported(
                "a constructor reference with no type",
            ))?;
        let _ = input;
        index
            .item_by_fqn(qualifier.syntax().text().to_string().trim())
            .ok_or(WasmError::Unsupported(
                "a constructor reference to an unindexed type",
            ))
    }

    /// The `(struct field, type)` a captured local is read through, when `id` is one of the enclosing
    /// class's captures.
    fn capture_field(&self, id: DefId) -> Option<(u32, Ty)> {
        let owner = self.owner?;
        let captured = self.layout.captures.get(&owner)?;
        let first = *self.layout.capture_slot.get(&owner)?;
        let position = captured.iter().position(|(seen, _)| *seen == id)?;
        Some((
            first + u32::try_from(position).ok()?,
            captured[position].1.clone(),
        ))
    }

    /// Push the values a local class's constructor takes for its captures, read from wherever they live
    /// here — a local of the enclosing method, or *this* class's own capture field when one local class
    /// creates another.
    fn push_captures(&self, item: ItemId, insn: &mut Insn) -> Result<()> {
        let captured = self.layout.captures.get(&item).cloned().unwrap_or_default();
        for (id, _) in &captured {
            self.push_capture(*id, insn)?;
        }
        Ok(())
    }

    /// Push one captured local's value, from wherever it lives *here*: a local of the enclosing method, or
    /// this class's own capture field when one capturing class creates another.
    fn push_capture(&self, id: DefId, insn: &mut Insn) -> Result<()> {
        if let Some(slot) = self.slot_of(id) {
            insn.local_get(slot);
        } else if let Some((field, _)) = self.capture_field(id) {
            let owner = self
                .owner
                .ok_or(WasmError::Unsupported("a capture with no enclosing class"))?;
            insn.local_get(0)
                .struct_get(self.layout.structs[&owner], field);
        } else {
            return Err(WasmError::Unsupported("a capture with no value here"));
        }
        Ok(())
    }

    /// One value a call site pushes: an argument at its parameter's type, or the array a **varargs**
    /// call builds out of its trailing arguments.
    ///
    /// Planned before anything is emitted because the two call paths push at different moments — the
    /// direct one straight onto the stack, the virtual one into a local apiece before the dispatch
    /// block opens — and both have to pass the same values.
    fn plan_arguments<'e>(
        &self,
        member: MemberId,
        arguments: &'e [ast::Expr],
    ) -> Result<Vec<Arg<'e>>> {
        let params = self.index.resolved_param_tys(member);
        let varargs = self.index.member(member).varargs;
        // The JVM has no variable arity and neither has wasm: `f(int...)` takes an `int[]` and the
        // call site builds it (JLS §15.12.4.2). One argument that is already an array of the right
        // type passes straight through instead — packing it would build an `int[][]`.
        let packs = varargs
            && !params.is_empty()
            && !(arguments.len() == params.len()
                && arguments.last().is_some_and(|last| {
                    matches!(
                        self.input.type_of_expr(Facts::span(last.syntax())),
                        Some(Ty::Array(_))
                    )
                }));
        let fixed = if packs {
            params.len() - 1
        } else {
            params.len()
        };
        let mut out = Vec::with_capacity(arguments.len());
        for (position, argument) in arguments.iter().take(fixed).enumerate() {
            match params.get(position) {
                Some(declared) => out.push(Arg::Value(argument, self.layout.val_type(declared)?)),
                None => out.push(Arg::Untyped(argument)),
            }
        }
        if packs {
            let Some(Ty::Array(element)) = params.last().cloned() else {
                return Err(WasmError::Unsupported(
                    "a variable-arity parameter that is no array",
                ));
            };
            let element_ty = self.layout.val_type(&element)?;
            let array = self
                .layout
                .array_type(element_ty)
                .ok_or_else(|| WasmError::NoRepresentation("an array".to_owned()))?;
            out.push(Arg::Packed {
                values: &arguments[fixed.min(arguments.len())..],
                element: *element,
                array,
            });
        }
        Ok(out)
    }

    /// Push one planned argument, and say what type it left on the stack.
    fn push_argument(&mut self, arg: &Arg<'_>, insn: &mut Insn) -> Result<ValType> {
        match arg {
            Arg::Value(expr, target) => {
                self.expr_as(expr, *target, insn)?;
                Ok(*target)
            }
            Arg::Untyped(expr) => self
                .expr(expr, insn)?
                .ok_or(WasmError::Unsupported("an argument with no value")),
            Arg::Packed {
                values,
                element,
                array,
            } => {
                for value in *values {
                    self.value_as(value, element, insn)?;
                }
                let count = u32::try_from(values.len()).map_err(|_| WasmError::TooLarge)?;
                insn.array_new_fixed(*array, count);
                Ok(ValType::Ref(RefType::nullable(HeapType::Concrete(*array))))
            }
        }
    }

    /// Push every planned argument in order.
    fn push_arguments(
        &mut self,
        member: MemberId,
        arguments: &[ast::Expr],
        insn: &mut Insn,
    ) -> Result<()> {
        for arg in self.plan_arguments(member, arguments)? {
            self.push_argument(&arg, insn)?;
        }
        Ok(())
    }

    /// Push `expr` and make the value the `target` an argument, a field store, or a return wants —
    /// a narrowing when erasure left it at the top of the reference hierarchy, a boxing conversion
    /// when a primitive met a reference.
    ///
    /// An `Object`, an interface, and a type variable are all `anyref` here, and every use at a
    /// *concrete* type — a parameter, a field, a return — wants that struct in particular. The JVM
    /// backend emits a `checkcast` at exactly these places and for exactly this reason; the
    /// validator is stricter than the verifier only in that it will not let one through unchecked.
    ///
    /// Nullable, because Java's `null` is assignable everywhere: a non-null cast would trap on a
    /// value the source is entitled to pass.
    fn expr_as(&mut self, expr: &ast::Expr, target: ValType, insn: &mut Insn) -> Result<()> {
        let produced = self
            .expr(expr, insn)?
            .ok_or(WasmError::Unsupported("an argument with no value"))?;
        self.coerce(expr, produced, target, insn)
    }

    /// Emit `value` — already on the stack, of wasm type `produced` — as the type `target` wants.
    ///
    /// This is the one place Java's conversions between a primitive and its wrapper live. Two
    /// references are a [`narrow`](Self::narrow) — the `ref.cast` erasure makes necessary — and two
    /// primitives are one too, a widening. The two *crossing* pairs are the **boxing** conversion
    /// (JLS §5.1.7) and the **unboxing** one (§5.1.8), and both leave the primitive world through a
    /// wrapper, which is a `java.lang` class a linked package supplies: [`box_value`](Self::box_value)
    /// calls its `valueOf`, [`unbox_value`](Self::unbox_value) its accessor.
    fn coerce(
        &self,
        value: &ast::Expr,
        produced: ValType,
        target: ValType,
        insn: &mut Insn,
    ) -> Result<()> {
        match (Self::numeric(produced), Self::numeric(target)) {
            (Some(_), None) => self.box_value(value, insn),
            (None, Some(target)) => {
                let unboxed = self.unbox_value(value, produced, insn)?;
                if unboxed == target {
                    return Ok(());
                }
                Self::widen(unboxed, target, insn)
            }
            _ => self.narrow(produced, target, insn),
        }
    }

    /// Emit `condition` and leave its `i32` truth on the stack.
    ///
    /// A Java condition is a `boolean` **or** a `Boolean` wherever one is due — an `if`, a loop, a
    /// `?:`, a `!`, a `&&` (JLS §14.9.1, §14.12–§14.14, §15.15.6, §15.23–§15.24, §15.25) — so the
    /// wrapper unboxes here by the same conversion a `boolean` declared from one gets. Every test
    /// the source writes reaches `i32` through this.
    fn condition(&mut self, condition: &ast::Expr, insn: &mut Insn) -> Result<()> {
        let produced = self
            .expr(condition, insn)?
            .ok_or(WasmError::Unsupported("a condition with no value"))?;
        self.coerce(condition, produced, ValType::I32, insn)
    }

    /// Box the primitive on the stack into the wrapper its *own* type names (JLS §5.1.7).
    ///
    /// *Its own* — boxing never converts on the way: `Long l = 1;` is not a Java program precisely
    /// because that would take two conversions, so the wrapper is read off the value's static type
    /// and a widening *reference* conversion to `Object`, to `Number`, costs nothing from there.
    /// Erasure is what makes this common rather than exotic: a type variable is `anyref` here, so
    /// `List<Integer>.add(1)` puts an `i32` where a reference belongs. The `valueOf` that boxes it
    /// is a *library* function, so a wrapper no linked package supplies — `Byte`, `Short`, which
    /// the platform deliberately leaves to the stubs — is refused by the name of the type it needs.
    fn box_value(&self, value: &ast::Expr, insn: &mut Insn) -> Result<()> {
        let primitive = self
            .input
            .type_of_expr(Facts::span(value.syntax()))
            .and_then(|ty| match ty {
                Ty::Primitive(primitive) => Some(*primitive),
                _ => None,
            })
            .ok_or(WasmError::Unsupported(
                "a boxing conversion of a value that is no primitive",
            ))?;
        let wrapper = Self::wrapper_of(primitive);
        let owner = self
            .index
            .item_by_fqn(wrapper)
            .ok_or_else(|| WasmError::Unresolved(wrapper.to_owned()))?;
        let member = self
            .method_matching(owner, "valueOf", &[Ty::Primitive(primitive)], true)
            .ok_or_else(|| WasmError::Unresolved(alloc::format!("{wrapper}.valueOf")))?;
        let function = self
            .layout
            .functions
            .get(&member)
            .copied()
            .ok_or_else(|| WasmError::NoRepresentation(wrapper.to_owned()))?;
        insn.call(function);
        Ok(())
    }

    /// Unbox the value `value` on the stack down to the primitive its own static type names, and
    /// answer which primitive came back.
    fn unbox_value(
        &self,
        value: &ast::Expr,
        produced: ValType,
        insn: &mut Insn,
    ) -> Result<Numeric> {
        let ty = self
            .input
            .type_of_expr(Facts::span(value.syntax()))
            .cloned()
            .ok_or(WasmError::Unsupported(
                "an unboxing conversion of a value with no type",
            ))?;
        self.unbox_ty(&ty, produced, insn)
    }

    /// Unbox the reference on the stack down to the primitive `ty`'s wrapper names (JLS §5.1.8).
    ///
    /// The wrapper's own accessor is a library function and is called directly: the class comes
    /// from the value's static type — or, for the element a `for`-each binds, from the iterable's
    /// type argument, which is the only type that names it when the variable is primitive. A value
    /// that arrived *erased* — a type variable's `anyref`, an element read out of a
    /// `List<Integer>` — is cast down to the wrapper first, the `ref.cast` a written cast would
    /// emit. A reference that is no wrapper at all is not an unboxing conversion the source could
    /// have written, and is refused as the gap in this backend that it is.
    fn unbox_ty(&self, ty: &Ty, produced: ValType, insn: &mut Insn) -> Result<Numeric> {
        let (wrapper, accessor, unboxed) = self
            .class_name(ty)
            .and_then(Self::wrapper_accessor)
            .ok_or(WasmError::Unsupported(
                "an unboxing conversion of a value that is no wrapper",
            ))?;
        let owner = self
            .index
            .item_by_fqn(wrapper)
            .ok_or_else(|| WasmError::Unresolved(wrapper.to_owned()))?;
        let member = self
            .method_matching(owner, accessor, &[], false)
            .ok_or_else(|| WasmError::Unresolved(alloc::format!("{wrapper}.{accessor}")))?;
        let function = self
            .layout
            .functions
            .get(&member)
            .copied()
            .ok_or_else(|| WasmError::NoRepresentation(wrapper.to_owned()))?;
        self.narrow(produced, self.layout.class_ref(owner)?, insn)?;
        insn.call(function);
        Ok(unboxed)
    }

    /// The method `owner` declares under `name` with exactly `params`, when `is_static` matches.
    ///
    /// Called where a protocol names a method the source never wrote: the wrappers' `valueOf` and
    /// their accessors, and the `iterator` / `hasNext` / `next` a `for`-each runs on. The parameter
    /// list is what tells a `valueOf(int)` from the `valueOf(String)` a later platform might add.
    /// Each wrapper member is asked of the *linked* class where one supplies it and of the stub
    /// otherwise, so an unboxing of a wrapper no package provides lands on the library lookup and
    /// not here.
    fn method_matching(
        &self,
        owner: ItemId,
        name: &str,
        params: &[Ty],
        is_static: bool,
    ) -> Option<MemberId> {
        self.index.own_members(owner).iter().copied().find(|&id| {
            let info = self.index.member(id);
            info.kind == DefKind::Method
                && info.modifiers.is_static == is_static
                && info.name == name
                && self.index.resolved_param_tys(id).as_slice() == params
        })
    }

    /// The `java.lang` wrapper class a primitive boxes into (JLS §5.1.7).
    const fn wrapper_of(primitive: Primitive) -> &'static str {
        match primitive {
            Primitive::Boolean => "java.lang.Boolean",
            Primitive::Byte => "java.lang.Byte",
            Primitive::Short => "java.lang.Short",
            Primitive::Char => "java.lang.Character",
            Primitive::Int => "java.lang.Integer",
            Primitive::Long => "java.lang.Long",
            Primitive::Float => "java.lang.Float",
            Primitive::Double => "java.lang.Double",
        }
    }

    /// The wrapper a class name is — under either spelling — the accessor its unboxing conversion
    /// calls (JLS §5.1.8), and the primitive that comes back.
    ///
    /// A name written in source resolves to its fully-qualified form and a type that came out of
    /// inference can carry the simple one, so both answer. Nothing else does: a class called
    /// `Integer` in another package is not the wrapper and has no unboxing conversion.
    fn wrapper_accessor(name: &str) -> Option<(&'static str, &'static str, Numeric)> {
        let simple = name.strip_prefix("java.lang.").unwrap_or(name);
        let (wrapper, accessor, unboxed) = match simple {
            "Boolean" => ("java.lang.Boolean", "booleanValue", Numeric::Int),
            "Byte" => ("java.lang.Byte", "byteValue", Numeric::Int),
            "Short" => ("java.lang.Short", "shortValue", Numeric::Int),
            "Character" => ("java.lang.Character", "charValue", Numeric::Int),
            "Integer" => ("java.lang.Integer", "intValue", Numeric::Int),
            "Long" => ("java.lang.Long", "longValue", Numeric::Long),
            "Float" => ("java.lang.Float", "floatValue", Numeric::Float),
            "Double" => ("java.lang.Double", "doubleValue", Numeric::Double),
            _ => return None,
        };
        Some((wrapper, accessor, unboxed))
    }

    /// Emit the `ref.cast` that takes a top-of-hierarchy value down to `target`, if one is needed —
    /// or, between two primitives, the widening conversion Java performs silently.
    fn narrow(&self, produced: ValType, target: ValType, insn: &mut Insn) -> Result<()> {
        if produced == target {
            return Ok(());
        }
        let (ValType::Ref(from), ValType::Ref(to)) = (produced, target) else {
            // Two primitives that differ are a **widening primitive conversion** (JLS §5.1.2), which
            // the language performs with no cast written and which wasm has no implicit form of.
            // `static long take(long x)` called as `take(1)` put an `i32` where the signature says
            // `i64` and the module was refused by the validator with nothing said on this side —
            // `value_as` routes a numeric target through `operand` and so gets `long a = 1;` right,
            // which is what made the pair disagree about one conversion.
            if let (Some(from), Some(to)) = (Self::numeric(produced), Self::numeric(target)) {
                return Self::widen(from, to, insn);
            }
            // One primitive and one reference is a *boxing* or *unboxing* conversion, and neither
            // goes through here: both leave through `coerce`, which has the value whose type names
            // the wrapper. Arriving at this one means a value was built at one representation and
            // used at the other with no conversion asked for — a bug in this backend, said out loud.
            return Err(WasmError::Unsupported(
                "a conversion between a primitive and a reference",
            ));
        };
        if from.heap == to.heap {
            return Ok(());
        }
        // `HeapType::None` is the *bottom* and already fits everywhere, so only the top needs one.
        if matches!(to.heap, HeapType::Concrete(_)) && from.heap == HeapType::Any {
            insn.ref_cast(to.heap, true);
            return Ok(());
        }
        // Java's arrays are covariant and wasm's are invariant, so `Object[] o = new String[1]` has
        // no representation here at all: the two array types are unrelated, and no cast relates
        // them. Said out loud rather than emitted, because the bytes would be a module no engine
        // loads.
        if let (HeapType::Concrete(from), HeapType::Concrete(to)) = (from.heap, to.heap)
            && (self.layout.is_array(from) || self.layout.is_array(to))
        {
            return Err(WasmError::Unsupported(
                "an array where an array of another type is wanted",
            ));
        }
        Ok(())
    }

    /// The Java primitive a wasm value type stands for, at the granularity wasm keeps.
    ///
    /// `byte`, `short` and `char` are all `i32` here and the conversions between them are no-ops, so
    /// nothing is lost by answering `Int` for all four: what this is used for is the *widening* a
    /// value needs to reach a wider slot, and every such widening is visible at this granularity.
    const fn numeric(ty: ValType) -> Option<Numeric> {
        match ty {
            ValType::I32 => Some(Numeric::Int),
            ValType::I64 => Some(Numeric::Long),
            ValType::F32 => Some(Numeric::Float),
            ValType::F64 => Some(Numeric::Double),
            ValType::Ref(_) => None,
        }
    }

    /// Emit the widening conversion from `from` to `to`, refusing anything that is not one.
    ///
    /// Only widening: JLS §5.3 admits a widening primitive conversion at an invocation and nothing
    /// else, so a *narrowing* here is not a conversion the source omitted but a mismatch this
    /// backend arrived at, and emitting a truncation for it would answer with a different number.
    fn widen(from: Numeric, to: Numeric, insn: &mut Insn) -> Result<()> {
        const fn rank(ty: Numeric) -> u8 {
            match ty {
                Numeric::Byte | Numeric::Short | Numeric::Char | Numeric::Int => 0,
                Numeric::Long => 1,
                Numeric::Float => 2,
                Numeric::Double => 3,
            }
        }
        if rank(from) >= rank(to) {
            return Err(WasmError::Unsupported(
                "a narrowing primitive conversion the source did not write",
            ));
        }
        insn.convert(from, to).ok_or(WasmError::Unsupported(
            "a numeric conversion with no encoding",
        ))?;
        Ok(())
    }

    /// A fresh unnamed local of type `ty`, for values that must outlive the stack.
    fn scratch(&mut self, ty: ValType) -> u32 {
        let slot = self.next;
        self.locals.push(ty);
        self.next += 1;
        slot
    }

    fn call(&mut self, call: &ast::CallExpr, insn: &mut Insn) -> Result<Option<ValType>> {
        let member = self
            .input
            .call_target_of(Facts::span(call.syntax()))
            .ok_or_else(|| WasmError::Unresolved(call.syntax().text().to_string().trim().into()))?;
        let info = self.index.member(member);
        let is_static = info.modifiers.is_static;

        let arguments: Vec<ast::Expr> = call.args().into_iter().flat_map(|l| l.args()).collect();
        // A `super.` qualifier names one body in particular — the superclass's — so the call is not
        // dispatched at all. The whole question sits in the shared layer, not just the `super` leaf:
        // letting the `ref.test` chain select by *runtime* type here is how an override calling
        // `super.f()` would call itself.
        let super_qualified = Facts::is_super_call(call);
        let overriders = if is_static || super_qualified {
            Vec::new()
        } else {
            self.overriders(member)
        };
        // A library's dispatch cannot close over classes it cannot see, so every open call is also
        // routed through its realm: the arm consults the installed dispatcher first and the chain
        // below is what an unlinked library still runs on. `native` is the boundary — its body is
        // the host's, which the library does not export, so a slot for it would have no fallback
        // and an override of a native method in a consumer class is the one dispatch a realm
        // cannot answer for. A member with no declared type is one no arm could be built for
        // either, and stays on the path it was on.
        let dispatched = self.layout.realm.is_some()
            && !super_qualified
            && !is_static
            && !info.modifiers.is_private
            && info.kind == DefKind::Method
            && !self.layout.imported(member)
            && self.layout.member_types.contains_key(&member);
        if !overriders.is_empty() || dispatched {
            let ty = match self.index.resolved_member_ty(member) {
                Ty::Void => None,
                ty => Some(self.layout.val_type(&ty)?),
            };
            return self.virtual_call(
                call,
                member,
                &arguments,
                Dispatch {
                    overriders: &overriders,
                    ty,
                    realm: dispatched,
                },
                insn,
            );
        }
        // Only now: a method with no function index is abstract, and an abstract one is only ever
        // reached through the chain above. Looking it up first reported "outside this module" for every
        // interface call, which named the wrong problem.
        let function = match self.layout.functions.get(&member).copied() {
            Some(function) => function,
            // A `super(…)` whose superclass came from a linked library. The library exports the
            // constructor's *body* beside the factory precisely for this: the factory allocates,
            // and this object already exists. The shape is the same `(this, arguments…) -> ()` an
            // in-module constructor has, so the receiver and argument pushing below is unchanged.
            None if info.kind == DefKind::Constructor
                && self.layout.external_initializers.contains_key(&member) =>
            {
                self.layout.external_initializers[&member]
            }
            // No function, the call is a *dispatch*, and the owner is a class this module lays
            // out: the method has no body anywhere in the module and nothing above overrides it, so
            // no object carrying an implementation can exist here and the call is dynamically
            // unreachable — which `unreachable` says exactly. An abstract class calling its own
            // abstract method is ordinary Java, and refusing it stopped the whole file.
            //
            // Both extra conditions are load-bearing, and each was a module that validated,
            // instantiated, and trapped where the merge base refused by name:
            //
            // - **A `static` call is not a dispatch.** `static native int f()` has no body and no
            //   receiver, so "no object can exist" says nothing about it; the module simply lacks
            //   the body, which is a refusal — unless the declaration said `native`, in which case
            //   it has an import and never reaches this match at all.
            //
            // An *interface* owner stays in, and soundly: a value of interface type comes either
            // from a laid-out struct — which the override scan above covers, lambdas and method
            // references included, since the index gives each of those an item of its own — or from
            // one this backend could not lay out, and that one is refused at the expression that
            // creates it rather than here.
            None if !is_static
                && !super_qualified
                && (self
                    .layout
                    .structs
                    .contains_key(&self.index.member(member).owner)
                    || self
                        .layout
                        .interfaces
                        .contains(&self.index.member(member).owner)) =>
            {
                insn.unreachable();
                return Ok(match self.index.resolved_member_ty(member) {
                    Ty::Void => None,
                    ty => Some(self.layout.val_type(&ty)?),
                });
            }
            // The owner is not a type this module lays out at all, so it is a *library* type — and
            // needing one is what puts a case outside this backend's subset, not a gap in it.
            // `System.out.println` is the everyday shape: nothing in such a file declares a library
            // type, so the case reached this far before naming the one it needs.
            // A project type whose body this module does not hold: an interface method
            // implemented only by a lambda or a method reference this backend does not lay out.
            // Its own report, because the library-type one would send a reader looking for a
            // dependency that is not there. A `native` method no longer reaches here — it has an
            // import and therefore a function index — so what is left is a body that was expected
            // and is missing, rather than one that was never going to be here.
            None if self
                .layout
                .structs
                .contains_key(&self.index.member(member).owner)
                || self
                    .layout
                    .interfaces
                    .contains(&self.index.member(member).owner) =>
            {
                return Err(WasmError::NoImplementation(CompileWasm::member_path(
                    member, self.index,
                )));
            }
            None => {
                return Err(WasmError::NoRepresentation(
                    self.index
                        .item(self.index.member(member).owner)
                        .fqn
                        .to_string(),
                ));
            }
        };
        if !is_static {
            match call.callee() {
                Some(ast::Expr::FieldAccess(access)) => {
                    let receiver = access
                        .receiver()
                        .ok_or(WasmError::Unsupported("a call with no receiver"))?;
                    // At the *owner's* type: a receiver read through an interface, an `Object`, or a
                    // type variable is `anyref`, and the function being called takes the struct.
                    let target = self.layout.class_ref(self.index.member(member).owner)?;
                    self.expr_as(&receiver, target, insn)?;
                }
                // A bare call in an instance method is an implicit `this` — but not necessarily
                // *this* `this`. From a class that holds an enclosing instance the method may be an
                // enclosing class's, and pushing local 0 there hands the callee an object of the
                // wrong type: bytes the emitter produces happily and the validator rejects.
                _ => {
                    self.load_unqualified_receiver(self.index.member(member).owner, insn)?;
                }
            }
        }
        // A constructor of an inner class takes the enclosing instance right after `this`, before
        // every declared argument — which is exactly where a `new` puts it. A `super(…)` or
        // `this(…)` reaching one is a call to that constructor and passes it too; leaving it out is
        // a call one argument short, which the validator refuses.
        if info.kind == DefKind::Constructor
            && let Some(&encloses) = self.layout.inner.get(&info.owner)
        {
            self.enclosing_instance(None, encloses, insn)?;
        }
        self.push_arguments(member, &arguments, insn)?;
        // A local or anonymous class's constructor takes its captures as *trailing* parameters, and
        // a `this(…)` reaching one has to pass them like every other argument. They are read from
        // the synthetic fields, which this constructor's prologue filled before its body ran.
        if info.kind == DefKind::Constructor {
            self.push_captures(info.owner, insn)?;
        }
        insn.call(function);

        // A constructor has no return type at all — `resolved_member_ty` reports `Unknown` for one,
        // which is not a type this backend could represent even in principle. `this(…)` and `super(…)`
        // are calls to one, and they produce no value.
        if self.index.member(member).kind == DefKind::Constructor {
            return Ok(None);
        }
        match self.index.resolved_member_ty(member) {
            Ty::Void => Ok(None),
            ty => Ok(Some(self.layout.val_type(&ty)?)),
        }
    }
}

/// Where an assignable value lives, once its subexpressions have been evaluated.
///
/// The JVM backend's equivalent duplicates an address under a value with `dup_x1`; wasm has no such
/// instruction, so every operand a store needs is held in a local and pushed again for each access.
/// That is the whole difference between the two protocols: here an address is *re-emitted*, not
/// duplicated.
#[derive(Debug, Clone, Copy)]
enum Place {
    Local {
        slot: u32,
        ty: ValType,
    },
    /// A field of an object whose reference is in local `receiver`.
    Field {
        receiver: u32,
        struct_type: u32,
        slot: u32,
        ty: ValType,
    },
    /// A `static` field, which is a module-level global.
    Global {
        index: u32,
        ty: ValType,
    },
    /// A `static` field of a linked library, reached through its accessor imports: a global in
    /// another module is not a name a Java signature can spell.
    External {
        get: u32,
        put: u32,
        ty: ValType,
    },
    /// An element of the array in local `array` at the index in local `index`.
    Element {
        array: u32,
        index: u32,
        array_type: u32,
        ty: ValType,
    },
}

impl Place {
    const fn ty(self) -> ValType {
        match self {
            Self::Local { ty, .. }
            | Self::Global { ty, .. }
            | Self::External { ty, .. }
            | Self::Field { ty, .. }
            | Self::Element { ty, .. } => ty,
        }
    }

    /// Push the operands [`store`](Self::store) needs *below* the value.
    fn address(self, insn: &mut Insn) {
        match self {
            // The value on the stack *is* the argument an accessor call takes.
            Self::Local { .. } | Self::Global { .. } | Self::External { .. } => {}
            Self::Field { receiver, .. } => {
                insn.local_get(receiver);
            }
            Self::Element { array, index, .. } => {
                insn.local_get(array).local_get(index);
            }
        }
    }

    /// Push the value currently held here.
    fn read(self, insn: &mut Insn) {
        match self {
            Self::Local { slot, .. } => {
                insn.local_get(slot);
            }
            Self::Global { index, .. } => {
                insn.global_get(index);
            }
            Self::External { get, .. } => {
                insn.call(get);
            }
            Self::Field {
                receiver,
                struct_type,
                slot,
                ..
            } => {
                insn.local_get(receiver).struct_get(struct_type, slot);
            }
            Self::Element {
                array,
                index,
                array_type,
                ..
            } => {
                insn.local_get(array).local_get(index).array_get(array_type);
            }
        }
    }

    /// Consume the value on top of the stack, storing it here. `keep` leaves it behind.
    ///
    /// wasm has no `dup`, so nothing can duplicate a value under a `struct.set`: keeping one means
    /// `local.tee` for a local, and for a field or an element a second load of what was just written —
    /// which is the same value, this backend having no volatile fields and no threads.
    fn store(self, insn: &mut Insn, keep: bool) {
        match self {
            Self::Local { slot, .. } => {
                if keep {
                    insn.local_tee(slot);
                } else {
                    insn.local_set(slot);
                }
            }
            // No `global.tee`, so keeping the value is a read back — the same value, this backend
            // having neither threads nor volatile fields.
            Self::Global { index, .. } => {
                insn.global_set(index);
                if keep {
                    self.read(insn);
                }
            }
            Self::External { put, .. } => {
                insn.call(put);
                if keep {
                    self.read(insn);
                }
            }
            Self::Field {
                struct_type, slot, ..
            } => {
                insn.struct_set(struct_type, slot);
                if keep {
                    self.read(insn);
                }
            }
            Self::Element { array_type, .. } => {
                insn.array_set(array_type);
                if keep {
                    self.read(insn);
                }
            }
        }
    }
}
