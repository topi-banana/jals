//! The WebAssembly binary format, written by hand.
//!
//! Hand-rolled for the same reason the zip reader and the class-file codec are: the format is a
//! stable, fully-specified byte layout, and writing it directly keeps this crate `no_std` with no
//! new dependency. An encoder crate would pull in `std` and put a third party between jals and a
//! specification it has to match exactly anyway.
//!
//! Scope is the garbage-collected subset (the GC proposal, merged into WebAssembly 3.0): recursive
//! type groups, struct and array types with declared subtyping, and the reference instructions that
//! go with them. That is what lets Java's object model land on the *host's* collector — every
//! reference is a `(ref $T)`, allocation is `struct.new`, and nothing here traces, marks, or frees.

use alloc::string::String;
use alloc::vec::Vec;

use crate::wasm::insn::Instr;

/// A little-endian byte writer with the LEB128 integer encodings WebAssembly uses.
#[derive(Debug, Default)]
pub(crate) struct Bytes {
    out: Vec<u8>,
    /// Set when a length or an index did not fit the `u32` the format spells it with.
    ///
    /// Sticky, and merged whenever one buffer is appended to another, so [`Module::finish`] can
    /// refuse to hand out a module rather than one carrying a length that is simply wrong. A
    /// truncated length is not a smaller module: it is bytes an engine reads as something else.
    overflow: bool,
}

impl Bytes {
    const fn new() -> Self {
        Self {
            out: Vec::new(),
            overflow: false,
        }
    }

    pub(crate) fn byte(&mut self, value: u8) -> &mut Self {
        self.out.push(value);
        self
    }

    pub(crate) fn raw(&mut self, bytes: &[u8]) -> &mut Self {
        self.out.extend_from_slice(bytes);
        self
    }

    /// Unsigned LEB128, the encoding of every index, count, and length in the format.
    pub(crate) fn u32(&mut self, mut value: u32) -> &mut Self {
        loop {
            let byte = u8::try_from(value & 0x7F).unwrap_or(0);
            value >>= 7;
            if value == 0 {
                self.out.push(byte);
                return self;
            }
            self.out.push(byte | 0x80);
        }
    }

    /// Signed LEB128, the encoding of `i32.const` operands and of concrete heap types.
    pub(crate) fn i32(&mut self, value: i32) -> &mut Self {
        self.i64(i64::from(value))
    }

    /// Signed LEB128 over 64 bits.
    pub(crate) fn i64(&mut self, mut value: i64) -> &mut Self {
        loop {
            let byte = u8::try_from(value.cast_unsigned() & 0x7F).unwrap_or(0);
            value >>= 7;
            // The encoding stops when the remaining bits are all copies of the sign bit that the
            // last byte's own high bit already carries.
            let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
            if done {
                self.out.push(byte);
                return self;
            }
            self.out.push(byte | 0x80);
        }
    }

    /// A length-prefixed UTF-8 name.
    fn name(&mut self, text: &str) -> &mut Self {
        self.count(text.len());
        self.raw(text.as_bytes())
    }

    /// The element count that prefixes every vector, and every other length the format spells as a
    /// `u32`. A `usize` that does not fit sets [`overflow`](Self::overflow) instead of wrapping.
    pub(crate) fn count(&mut self, len: usize) -> &mut Self {
        let Ok(len) = u32::try_from(len) else {
            self.overflow = true;
            return self.u32(u32::MAX);
        };
        self.u32(len)
    }

    /// Append `other`'s bytes, carrying its overflow flag with them.
    fn append(&mut self, other: &Self) -> &mut Self {
        self.overflow |= other.overflow;
        self.raw(&other.out)
    }

    const fn len(&self) -> usize {
        self.out.len()
    }

    fn into_vec(self) -> Vec<u8> {
        self.out
    }
}

/// A heap type: what a reference points at.
///
/// Only the concrete form is modelled. The abstract heap types (`any`, `func`, `none`, …) occupy
/// the *negative* range of the same encoding, and this backend has no use for them: every Java
/// reference is a reference to a declared class or array type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeapType {
    /// A declared type, by index.
    Concrete(u32),
    /// The top of the internal reference hierarchy: every struct and array reference is one.
    ///
    /// How an *interface*-typed value is held. An interface has no struct type of its own — wasm's
    /// declared subtyping is single-inheritance, so it cannot be a supertype of two unrelated classes —
    /// so the value is kept at the top of the hierarchy and narrowed with `ref.cast` at each use.
    Any,
    /// The bottom of the reference hierarchy, whose only inhabitant is `null`.
    ///
    /// The one abstract heap type this backend needs: a bare `null` has no type of its own in Java, and
    /// `(ref null none)` is a subtype of *every* nullable reference — so it fits wherever the literal
    /// does without the target type having to be known first.
    None,
    /// The function-reference hierarchy, which is what a linked dispatch slot holds.
    ///
    /// The one heap type here that names no Java type: a library's dispatch slots are function
    /// references the consumer fills in, and the receiver of a call through one is an argument like
    /// any other rather than part of the value. `call_ref` names the function type both sides agreed
    /// on, and the group replay is what makes that agreement canonical.
    Func,
}

impl HeapType {
    pub(crate) fn write_to(self, out: &mut Bytes) {
        match self {
            // A concrete heap type is the type index as a *signed* LEB, which is what keeps it
            // apart from the negatively-encoded abstract ones.
            Self::Concrete(index) => out.i32(index.cast_signed()),
            // The abstract heap types occupy the negative range of the same signed encoding, which
            // is what keeps them apart from an index.
            Self::Any => out.byte(0x6E),
            Self::None => out.byte(0x71),
            Self::Func => out.byte(0x70),
        };
    }
}

/// A reference type: a heap type plus whether `null` inhabits it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefType {
    pub nullable: bool,
    pub heap: HeapType,
}

impl RefType {
    pub const fn nullable(heap: HeapType) -> Self {
        Self {
            nullable: true,
            heap,
        }
    }

    fn write(self, out: &mut Bytes) {
        out.byte(if self.nullable { 0x63 } else { 0x64 });
        self.heap.write_to(out);
    }
}

/// A value type: what a local, a parameter, or a stack slot holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValType {
    I32,
    I64,
    F32,
    F64,
    Ref(RefType),
}

impl ValType {
    pub(crate) fn write(self, out: &mut Bytes) {
        match self {
            Self::I32 => {
                out.byte(0x7F);
            }
            Self::I64 => {
                out.byte(0x7E);
            }
            Self::F32 => {
                out.byte(0x7D);
            }
            Self::F64 => {
                out.byte(0x7C);
            }
            Self::Ref(reference) => reference.write(out),
        }
    }
}

/// A struct field's or array element's type.
///
/// wasm also has packed `i8` / `i16` storage, which is what a `byte[]` should eventually use so it
/// costs a byte per element rather than four; the packed forms need `array.get_s` / `array.get_u`
/// to read back, so they arrive together with those.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageType {
    Val(ValType),
}

impl StorageType {
    fn write(self, out: &mut Bytes) {
        match self {
            Self::Val(value) => value.write(out),
        }
    }
}

/// A field of a struct or the element of an array.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldType {
    pub storage: StorageType,
    pub mutable: bool,
}

impl FieldType {
    fn write(self, out: &mut Bytes) {
        self.storage.write(out);
        out.byte(u8::from(self.mutable));
    }
}

/// A composite type: the three shapes a declared type can take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompType {
    Func {
        params: Vec<ValType>,
        results: Vec<ValType>,
    },
    Struct(Vec<FieldType>),
    Array(FieldType),
}

impl CompType {
    fn write(&self, out: &mut Bytes) {
        match self {
            Self::Func { params, results } => {
                out.byte(0x60).count(params.len());
                for param in params {
                    param.write(out);
                }
                out.count(results.len());
                for result in results {
                    result.write(out);
                }
            }
            Self::Struct(fields) => {
                out.byte(0x5F).count(fields.len());
                for field in fields {
                    field.write(out);
                }
            }
            Self::Array(element) => {
                out.byte(0x5E);
                element.write(out);
            }
        }
    }
}

/// One declared type, with the supertype it extends.
///
/// `final` is the default in the binary format and forbids further subtyping, so every type a Java
/// class hierarchy needs is declared non-final. Subtyping is *declared*, not inferred: this is what
/// makes a `(ref $Sub)` usable where a `(ref $Super)` is expected, which is the whole reason Java
/// inheritance can ride on the host's type system rather than on a hand-built vtable walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubType {
    pub is_final: bool,
    pub supertype: Option<u32>,
    pub comp: CompType,
}

impl SubType {
    pub const fn plain(comp: CompType) -> Self {
        Self {
            is_final: true,
            supertype: None,
            comp,
        }
    }

    fn write(&self, out: &mut Bytes) {
        if self.is_final && self.supertype.is_none() {
            // The bare form is exactly "final, extending nothing", so it needs no prefix.
            self.comp.write(out);
            return;
        }
        out.byte(if self.is_final { 0x4F } else { 0x50 });
        match self.supertype {
            Some(index) => {
                out.count(1).u32(index);
            }
            None => {
                out.count(0);
            }
        }
        self.comp.write(out);
    }
}

/// What an export names.
///
/// A project module's surface is the `public static` methods it compiled, which is why [`Func`]
/// was the only arm for as long as one module was the whole program. A **library** module a project
/// module links against is different: its surface is its `public static` methods, its `static`
/// fields, and the one exception tag every `throw` uses — so all three kinds are exportable, and the
/// index in each is the index in *that kind's* space (see [`Module::global_index`] and
/// [`Module::tag_index`]).
///
/// [`Func`]: Self::Func
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportKind {
    Func,
    /// A module-level variable — a Java `static` field.
    Global,
    /// The exception tag a `throw` names.
    Tag,
}

/// A defined function: its declared locals (beyond the parameters) and its encoded body.
#[derive(Debug, Clone)]
pub struct Func {
    /// Index into the type section.
    pub type_index: u32,
    /// The locals following the parameters, one entry per local (no run-length grouping).
    pub locals: Vec<ValType>,
    /// The body's instructions, *without* the terminating `end`.
    ///
    /// Kept as instructions rather than as encoded bytes so that a finished module can still be
    /// asked what it holds; [`Module::finish`] is where they become bytes.
    pub body: Vec<Instr>,
}

/// What a module imports, and where its type comes from.
///
/// The names are not free-form. `module` is the declaring class's **internal name** for a host
/// function — the pair being derivable from the `native` declaration alone, which is what lets the
/// host that supplies the implementation key its table on exactly the same two strings — and the
/// name a **library module** exports under for the other three. A signature the two halves disagree
/// about is an *unresolved import* rather than a mismatch somebody has to notice.
///
/// # Why a host function's signature is held here rather than as a type index
///
/// An imported host function's type must be **its own type-section entry**, and it must be final
/// with no supertype. That is not a style choice: a host function is canonicalised on its own, so an
/// engine matches it against a declared type only when that type is a recursive group of one — and
/// every type this backend declares otherwise lives in the `rec` groups that let two Java classes
/// reference each other. An import whose signature was allocated in one of those groups links
/// against nothing, with the engine reporting only "incompatible import type".
///
/// So the signature travels with the import and [`Module::finish`] gives it an entry of its own,
/// after the declared groups. Holding a `type_index` instead would put the choice of where the type
/// was allocated at every call site.
///
/// # A library import names a shared type
///
/// The opposite arrangement, for the opposite reason. A module linked against a precompiled library
/// declares the library's types itself — the `rec` group must be *identical* on both sides or the
/// engine canonicalises them to different addresses and the link fails — so an imported function,
/// global, or tag names the type index this module already declared, and the two modules meet at
/// the same canonical type. Nothing is allocated for it here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// The declaring class's internal name (`jals/io/Out`) for a host function, or the name of the
    /// module that exports this value for a library import.
    pub module: String,
    /// The method's name followed by its descriptor (`writeChar(I)V`) for a host function, or the
    /// export name for a library import.
    pub name: String,
    /// Where the imported value's type comes from — see [`ImportKind`].
    pub kind: ImportKind,
}

/// The four things a module may import, and the type arrangement each one needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportKind {
    /// A host function, with the signature [`Module::finish`] gives its own type-section entry.
    Function {
        /// The imported function's parameters.
        params: Vec<ValType>,
        /// Its results — at most one, since a Java method returns at most one value.
        results: Vec<ValType>,
    },
    /// A function another module exports, at a type this module declared identically.
    SharedFunction {
        /// The index of the declared function type both modules share.
        type_index: u32,
    },
    /// A global another module exports — a Java `static` field.
    Global {
        /// The global's value type.
        ty: ValType,
        /// Whether the exporting module declared it mutable. A Java `static` field is assignable,
        /// so a global that is not mutable is a `static final` whose writes were all folded away.
        mutable: bool,
    },
    /// An exception tag another module exports, at a shared function type.
    Tag {
        /// The index of the declared function type that gives the tag its payload.
        type_index: u32,
    },
}

/// A module under construction.
#[derive(Debug, Default)]
pub struct Module {
    /// Declared types, flattened across every recursive group — a type index is an index into this
    /// vector whatever group its entry lives in.
    types: Vec<SubType>,
    /// The type index each recursive group after the first starts at, in ascending order. Empty
    /// means one implicit group over every declared type, which is what every module this crate
    /// produced before a second module existed wanted: every declared type in one `rec` group so
    /// any two may reference each other.
    ///
    /// A second group exists for the one case that cannot share the first: a module linking against
    /// a precompiled library must declare the library's types in a group whose *content* matches
    /// the library module's exactly, because canonicalisation is per group and two groups that
    /// differ by one type canonicalise to different addresses. The project's own types therefore
    /// go after a boundary, where they can reference the library's and not the reverse.
    groups: Vec<usize>,
    /// Host functions and library values this module imports, in the order they occupy their
    /// respective index spaces. A function import's index is *before* every defined function, so
    /// [`func_index`](Self::func_index) offsets by their count; globals and tags have spaces of
    /// their own, offset the same way by [`global_index`](Self::global_index) and
    /// [`tag_index`](Self::tag_index).
    pub imports: Vec<Import>,
    pub funcs: Vec<Func>,
    /// Exception tags, by the index of the function type that gives each one's payload. One tag is
    /// enough for Java: every thrown value is a reference, so the payload type is the same for all of
    /// them and the *class* of the reference is what a `catch` tests.
    pub tags: Vec<u32>,
    pub globals: Vec<Global>,
    pub exports: Vec<(String, ExportKind, u32)>,
    /// The function an engine runs before anything else, if the module has one. This is where a Java
    /// `static` initialiser lives: a global's own initialiser is a constant expression and cannot
    /// compute anything.
    pub start: Option<u32>,
    /// Custom sections, written after the code section.
    ///
    /// A linked library carries its ABI in one — an artifact that travels as one file is an
    /// artifact whose halves cannot drift.
    custom: Vec<(String, Vec<u8>)>,
}

/// A module-level mutable variable, which is what a Java `static` field is.
///
/// Its initialiser is a *constant expression* — the format allows only a handful of instructions
/// there, so anything a `<clinit>` would have to compute cannot live here.
#[derive(Debug)]
pub struct Global {
    pub ty: ValType,
    /// The constant expression, without its terminating `end`.
    pub init: Vec<Instr>,
}

impl Module {
    pub const fn new() -> Self {
        Self {
            types: Vec::new(),
            groups: Vec::new(),
            imports: Vec::new(),
            funcs: Vec::new(),
            tags: Vec::new(),
            globals: Vec::new(),
            exports: Vec::new(),
            start: None,
            custom: Vec::new(),
        }
    }

    /// Append `ty` to the current recursive group and return its index.
    ///
    /// A saturated index would be wrong, but it is also unreachable *and* caught: a type section
    /// with more than `u32::MAX` entries cannot write its own count either, so
    /// [`finish`](Self::finish) refuses the module before an index that large can be read.
    pub fn add_type(&mut self, ty: SubType) -> u32 {
        if self.groups.is_empty() {
            self.groups.push(0);
        }
        self.types.push(ty);
        u32::try_from(self.types.len() - 1).unwrap_or(u32::MAX)
    }

    /// Start a new recursive group: every type added after this call can reference the types before
    /// it, and none of those can reference it.
    ///
    /// The boundary is what a module linking against a precompiled library needs — see the
    /// [`groups`](Self::groups) field. Calling this before any type or twice in a row adds no empty
    /// group: a boundary with nothing after it is not a group, and [`finish`](Self::finish) is what
    /// makes that true of the bytes.
    pub fn begin_group(&mut self) {
        if self.types.is_empty() {
            return;
        }
        if self.groups.is_empty() {
            self.groups.push(0);
        } else if self.groups.last() != Some(&self.types.len()) {
            self.groups.push(self.types.len());
        }
    }

    /// The recursive groups actually emitted, as `(start, end)` spans into [`types`](Self::types).
    ///
    /// A boundary with nothing after it is not a group, and a trailing one is only knowable here —
    /// the caller opens it, and no later `add_type` closes it. The count and the write loop both
    /// come from this list, because a section that declares an entry it does not write is bytes an
    /// engine reads as something else.
    fn emitted_groups(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.groups
            .iter()
            .enumerate()
            .filter_map(move |(position, &start)| {
                let end = self
                    .groups
                    .get(position + 1)
                    .copied()
                    .unwrap_or(self.types.len());
                (start < end).then_some((start, end))
            })
    }

    /// Reserve a type index whose body is filled in by [`set_type`](Self::set_type) later.
    ///
    /// Within a recursive group an index may be *referred to* before its body exists — which is
    /// what lets a class's field mention an array type declared after it and an array's element
    /// mention a class. The placeholder is a fieldless struct: if a caller forgets to fill one, the
    /// module still encodes, and the empty struct is what a reader sees.
    pub(crate) fn reserve_type(&mut self) -> u32 {
        self.add_type(SubType::plain(CompType::Struct(Vec::new())))
    }

    /// Fill in a body reserved by [`reserve_type`](Self::reserve_type).
    pub(crate) fn set_type(&mut self, index: u32, ty: SubType) {
        if let Some(slot) = usize::try_from(index)
            .ok()
            .and_then(|index| self.types.get_mut(index))
        {
            *slot = ty;
        }
    }

    /// Declare a host function import and return the function index it takes.
    ///
    /// Imports occupy the low end of the function index space, so every one of them has to be
    /// declared before the first [`func_index`](Self::func_index) is handed out — otherwise a
    /// defined function is given an index an import later takes. That ordering is the lowering's
    /// to keep, and it keeps it by collecting every `native` declaration in a sweep of its own.
    ///
    /// The signature is taken by value rather than as a type index, because where its type is
    /// *allocated* is load-bearing — see [`ImportKind::Function`].
    pub fn add_import(
        &mut self,
        module: String,
        name: String,
        params: Vec<ValType>,
        results: Vec<ValType>,
    ) -> u32 {
        self.imports.push(Import {
            module,
            name,
            kind: ImportKind::Function { params, results },
        });
        u32::try_from(self.func_import_count().saturating_sub(1)).unwrap_or(u32::MAX)
    }

    /// Import a function another module exports, at a type this module declared identically.
    ///
    /// `type_index` is this module's index for the shared function type — the one both modules
    /// declared in matching recursive groups. Nothing is allocated for it here; the link succeeds
    /// only when the two canonicalise to the same address, which is what makes the shared group a
    /// precondition rather than an optimisation.
    pub fn add_shared_import(&mut self, module: String, name: String, type_index: u32) -> u32 {
        self.imports.push(Import {
            module,
            name,
            kind: ImportKind::SharedFunction { type_index },
        });
        u32::try_from(self.func_import_count().saturating_sub(1)).unwrap_or(u32::MAX)
    }

    /// Import a global another module exports, and return the global index it takes.
    pub fn add_global_import(
        &mut self,
        module: String,
        name: String,
        ty: ValType,
        mutable: bool,
    ) -> u32 {
        self.imports.push(Import {
            module,
            name,
            kind: ImportKind::Global { ty, mutable },
        });
        u32::try_from(self.global_import_count().saturating_sub(1)).unwrap_or(u32::MAX)
    }

    /// Import an exception tag another module exports, at a shared function type, and return the
    /// tag index it takes.
    pub fn add_tag_import(&mut self, module: String, name: String, type_index: u32) -> u32 {
        self.imports.push(Import {
            module,
            name,
            kind: ImportKind::Tag { type_index },
        });
        u32::try_from(self.tag_import_count().saturating_sub(1)).unwrap_or(u32::MAX)
    }

    /// Append a custom section, by name and contents.
    ///
    /// The name is written as the format spells it — a length-prefixed UTF-8 string — so a reader
    /// finds the section by the same bytes the ABI names it with. Custom sections are emitted after
    /// the code section, which is where a linked library's ABI travels.
    pub fn add_custom_section(&mut self, name: String, contents: Vec<u8>) {
        self.custom.push((name, contents));
    }

    /// How many of the imports are functions, which is where the function index space starts.
    fn func_import_count(&self) -> usize {
        self.imports
            .iter()
            .filter(|import| {
                matches!(
                    import.kind,
                    ImportKind::Function { .. } | ImportKind::SharedFunction { .. }
                )
            })
            .count()
    }

    /// How many of the imports are globals.
    fn global_import_count(&self) -> usize {
        self.imports
            .iter()
            .filter(|import| matches!(import.kind, ImportKind::Global { .. }))
            .count()
    }

    /// How many of the imports are tags.
    fn tag_import_count(&self) -> usize {
        self.imports
            .iter()
            .filter(|import| matches!(import.kind, ImportKind::Tag { .. }))
            .count()
    }

    /// How many of the imports take a type-section entry of their own.
    fn host_import_count(&self) -> usize {
        self.imports
            .iter()
            .filter(|import| matches!(import.kind, ImportKind::Function { .. }))
            .count()
    }

    /// The index the `defined`-th defined function has.
    ///
    /// The function index space starts with the function imports, so this is an offset and not an
    /// identity — and the offset counts *function* imports only, since a global or a tag occupies a
    /// space of its own. Written out rather than assumed at the call site for exactly that reason:
    /// a lowering that spelled `defined` directly would be correct for every module with no import
    /// and wrong for every module with one.
    pub fn func_index(&self, defined: usize) -> u32 {
        u32::try_from(self.func_import_count().saturating_add(defined)).unwrap_or(u32::MAX)
    }

    /// The index the `defined`-th defined global has, past any imported globals.
    pub fn global_index(&self, defined: usize) -> u32 {
        u32::try_from(self.global_import_count().saturating_add(defined)).unwrap_or(u32::MAX)
    }

    /// The index the `defined`-th defined tag has, past any imported tags.
    pub fn tag_index(&self, defined: usize) -> u32 {
        u32::try_from(self.tag_import_count().saturating_add(defined)).unwrap_or(u32::MAX)
    }

    /// Every function index a body or global initialiser names with `ref.func`, in first-use order.
    ///
    /// `ref.func` is only valid for a function the module has *declared* a reference to, and a
    /// declarative element segment is where that is spelled. Collected by reading the module's own
    /// instructions back rather than tracked at each `ref.func` site: an emitter that had to
    /// remember would be a second place the set lives, and the encoder is the one place that cannot
    /// go stale.
    fn declared_functions(&self) -> Vec<u32> {
        let mut declared: Vec<u32> = Vec::new();
        let mut record = |instruction: &Instr| {
            if let Instr::RefFunc(index) = instruction
                && !declared.contains(index)
            {
                declared.push(*index);
            }
        };
        for global in &self.globals {
            for instruction in &global.init {
                record(instruction);
            }
        }
        for func in &self.funcs {
            for instruction in &func.body {
                record(instruction);
            }
        }
        declared
    }

    /// Encode the whole module, or `None` when a length did not fit the `u32` the format spells it
    /// with — a module whose own lengths are wrong is not a smaller module, it is bytes an engine
    /// reads as something else.
    pub fn finish(&self) -> Option<Vec<u8>> {
        let mut out = Bytes::new();
        out.raw(b"\0asm").raw(&1u32.to_le_bytes());

        // Every declared recursive group, then one entry of its own per *host* import. The split is
        // what makes a host import linkable at all: a host function is canonicalised alone, so it
        // matches a declared type only when that type is a group of one — see `ImportKind::Function`.
        // A shared, global, or tag import allocates nothing: its type is a declared one the exporting
        // module declared identically. The two counts agree because both read `emitted_groups`.
        let type_entries = self.emitted_groups().count() + self.host_import_count();
        if type_entries > 0 {
            let mut section = Bytes::new();
            section.count(type_entries);
            for (start, end) in self.emitted_groups() {
                section.byte(0x4E).count(end - start);
                for ty in &self.types[start..end] {
                    ty.write(&mut section);
                }
            }
            for import in &self.imports {
                if let ImportKind::Function { params, results } = &import.kind {
                    SubType::plain(CompType::Func {
                        params: params.clone(),
                        results: results.clone(),
                    })
                    .write(&mut section);
                }
            }
            Self::section(&mut out, 1, &section);
        }

        // The import section comes between the types and the functions, which is where the binary
        // format puts it — and it has to, because the function index space it opens is what the
        // function section continues.
        if !self.imports.is_empty() {
            let mut section = Bytes::new();
            section.count(self.imports.len());
            // The host entries are the ones written after the declared groups, so this counts them
            // as the loop reaches them — an import of another kind takes no type-section slot.
            let mut host = 0u32;
            for import in &self.imports {
                section.name(&import.module).name(&import.name);
                match &import.kind {
                    ImportKind::Function { .. } => {
                        // Import descriptor 0x00 is `func`, followed by its type index — which is
                        // its own entry after the declared groups, in the order the host imports
                        // were added.
                        section.byte(0x00);
                        match u32::try_from(self.types.len())
                            .ok()
                            .and_then(|base| base.checked_add(host))
                        {
                            Some(index) => {
                                section.u32(index);
                            }
                            None => {
                                section.count(usize::MAX);
                            }
                        }
                        host = host.saturating_add(1);
                    }
                    ImportKind::SharedFunction { type_index } => {
                        section.byte(0x00).u32(*type_index);
                    }
                    ImportKind::Global { ty, mutable } => {
                        // Import descriptor 0x03 is `global`, followed by its value type and
                        // mutability — the two facts the exporting module's global is checked
                        // against.
                        section.byte(0x03);
                        ty.write(&mut section);
                        section.byte(u8::from(*mutable));
                    }
                    ImportKind::Tag { type_index } => {
                        // Import descriptor 0x04 is `tag`: attribute 0 (`exception`, the only one
                        // there is) then the function type that gives it its payload.
                        section.byte(0x04).byte(0x00).u32(*type_index);
                    }
                }
            }
            Self::section(&mut out, 2, &section);
        }

        if !self.funcs.is_empty() {
            let mut section = Bytes::new();
            section.count(self.funcs.len());
            for func in &self.funcs {
                section.u32(func.type_index);
            }
            Self::section(&mut out, 3, &section);
        }

        // The tag section comes before the global section, which is where the binary format puts it.
        if !self.tags.is_empty() {
            let mut section = Bytes::new();
            section.count(self.tags.len());
            for &ty in &self.tags {
                // Attribute 0 is `exception`, the only one there is.
                section.byte(0x00).u32(ty);
            }
            Self::section(&mut out, 13, &section);
        }

        if !self.globals.is_empty() {
            let mut section = Bytes::new();
            section.count(self.globals.len());
            for global in &self.globals {
                global.ty.write(&mut section);
                // Every Java `static` field is assignable, so every global is mutable.
                section.byte(0x01);
                for instruction in &global.init {
                    instruction.write(&mut section);
                }
                section.byte(0x0B);
            }
            Self::section(&mut out, 6, &section);
        }

        if !self.exports.is_empty() {
            let mut section = Bytes::new();
            section.count(self.exports.len());
            for (name, kind, index) in &self.exports {
                section
                    .name(name)
                    .byte(match kind {
                        ExportKind::Func => 0x00,
                        ExportKind::Global => 0x03,
                        ExportKind::Tag => 0x04,
                    })
                    .u32(*index);
            }
            Self::section(&mut out, 7, &section);
        }

        if let Some(start) = self.start {
            let mut section = Bytes::new();
            section.u32(start);
            Self::section(&mut out, 8, &section);
        }

        // Functions a body takes a reference to have to be declared before any validator accepts
        // the `ref.func` that names them, and a *declarative* element segment is the spelling.
        // One segment listing every such index: the format has no ordering requirement on them,
        // and one segment is one length instead of one per function.
        let declared = self.declared_functions();
        if !declared.is_empty() {
            let mut section = Bytes::new();
            section.count(1);
            // Flags 3 is a declarative segment: elemkind, then the function indices. 0x00 is that
            // elemkind — `funcref`, the only one there is.
            section.byte(0x03).byte(0x00).count(declared.len());
            for index in declared {
                section.u32(index);
            }
            Self::section(&mut out, 9, &section);
        }

        if !self.funcs.is_empty() {
            let mut section = Bytes::new();
            section.count(self.funcs.len());
            for func in &self.funcs {
                let mut body = Bytes::new();
                body.count(func.locals.len());
                for local in &func.locals {
                    body.u32(1);
                    local.write(&mut body);
                }
                for instruction in &func.body {
                    instruction.write(&mut body);
                }
                body.byte(0x0B);
                section.count(body.len());
                section.append(&body);
            }
            Self::section(&mut out, 10, &section);
        }

        for (name, contents) in &self.custom {
            let mut section = Bytes::new();
            section.name(name).raw(contents);
            Self::section(&mut out, 0, &section);
        }

        (!out.overflow).then(|| out.into_vec())
    }

    /// Write one section: its id, its byte length, then its contents.
    fn section(out: &mut Bytes, id: u8, content: &Bytes) {
        out.byte(id).count(content.len());
        out.append(content);
    }
}

#[cfg(test)]
mod tests {
    use super::{Bytes, HeapType, RefType, ValType};

    /// LEB128 is where a hand-written encoder goes wrong first: the signed form has to stop on the
    /// sign bit, not on a zero remainder.
    #[test]
    fn the_integer_encodings_round_trip_through_the_spec() {
        let encode_u32 = |value: u32| {
            let mut bytes = Bytes::new();
            bytes.u32(value);
            bytes.into_vec()
        };
        assert_eq!(encode_u32(0), [0x00]);
        assert_eq!(encode_u32(127), [0x7F]);
        assert_eq!(encode_u32(128), [0x80, 0x01]);
        assert_eq!(encode_u32(624_485), [0xE5, 0x8E, 0x26]);

        let encode_i32 = |value: i32| {
            let mut bytes = Bytes::new();
            bytes.i32(value);
            bytes.into_vec()
        };
        assert_eq!(encode_i32(0), [0x00]);
        assert_eq!(encode_i32(-1), [0x7F]);
        assert_eq!(encode_i32(63), [0x3F]);
        // 64 needs a second byte precisely because 0x40 would read back as -64.
        assert_eq!(encode_i32(64), [0xC0, 0x00]);
        assert_eq!(encode_i32(-64), [0x40]);
        assert_eq!(encode_i32(-123_456), [0xC0, 0xBB, 0x78]);
    }

    /// Unused today, but part of the encoder's surface and cheap to keep honest.
    #[test]
    fn reference_types_encode_their_nullability() {
        let encode = |ty: ValType| {
            let mut bytes = Bytes::new();
            ty.write(&mut bytes);
            bytes.into_vec()
        };
        // `(ref null $3)` and `(ref $3)` differ only in the leading byte.
        assert_eq!(
            encode(ValType::Ref(RefType::nullable(HeapType::Concrete(3)))),
            [0x63, 0x03]
        );
        assert_eq!(
            encode(ValType::Ref(RefType {
                nullable: false,
                heap: HeapType::Concrete(3),
            })),
            [0x64, 0x03]
        );
        // A concrete heap type is a *signed* LEB, so index 64 needs a second byte where 63 does
        // not — the abstract heap types live in the negative range of the same encoding.
        assert_eq!(
            encode(ValType::Ref(RefType::nullable(HeapType::Concrete(64)))),
            [0x63, 0xC0, 0x00]
        );
        // The function-reference hierarchy, whose encoding is an abstract heap type like `any`.
        assert_eq!(
            encode(ValType::Ref(RefType::nullable(HeapType::Func))),
            [0x63, 0x70]
        );
    }
}
