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
    pub(crate) const fn new() -> Self {
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

    pub(crate) fn into_vec(self) -> Vec<u8> {
        self.out
    }
}

/// A heap type: what a reference points at.
///
/// Only the concrete form is modelled. The abstract heap types (`any`, `func`, `none`, …) occupy
/// the *negative* range of the same encoding, and this backend has no use for them: every Java
/// reference is a reference to a declared class or array type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeapType {
    /// A declared type, by index.
    Concrete(u32),
    /// The bottom of the reference hierarchy, whose only inhabitant is `null`.
    ///
    /// The one abstract heap type this backend needs: a bare `null` has no type of its own in Java, and
    /// `(ref null none)` is a subtype of *every* nullable reference — so it fits wherever the literal
    /// does without the target type having to be known first.
    None,
}

impl HeapType {
    pub(crate) fn write_to(self, out: &mut Bytes) {
        match self {
            // A concrete heap type is the type index as a *signed* LEB, which is what keeps it
            // apart from the negatively-encoded abstract ones.
            Self::Concrete(index) => out.i32(index.cast_signed()),
            // The abstract heap types occupy the negative range of the same signed encoding, which
            // is what keeps them apart from an index.
            Self::None => out.byte(0x71),
        };
    }
}

/// A reference type: a heap type plus whether `null` inhabits it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RefType {
    pub(crate) nullable: bool,
    pub(crate) heap: HeapType,
}

impl RefType {
    pub(crate) const fn nullable(heap: HeapType) -> Self {
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
pub(crate) enum ValType {
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
pub(crate) enum StorageType {
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
pub(crate) struct FieldType {
    pub(crate) storage: StorageType,
    pub(crate) mutable: bool,
}

impl FieldType {
    fn write(self, out: &mut Bytes) {
        self.storage.write(out);
        out.byte(u8::from(self.mutable));
    }
}

/// A composite type: the three shapes a declared type can take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CompType {
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
pub(crate) struct SubType {
    pub(crate) is_final: bool,
    pub(crate) supertype: Option<u32>,
    pub(crate) comp: CompType,
}

impl SubType {
    pub(crate) const fn plain(comp: CompType) -> Self {
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

/// What an export names. Only functions are exported today: a module's surface is the `public
/// static` methods it compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExportKind {
    Func,
}

/// A defined function: its declared locals (beyond the parameters) and its encoded body.
#[derive(Debug, Clone)]
pub(crate) struct Func {
    /// Index into the type section.
    pub(crate) type_index: u32,
    /// The locals following the parameters, one entry per local (no run-length grouping).
    pub(crate) locals: Vec<ValType>,
    /// The body's instructions, *without* the terminating `end`.
    pub(crate) body: Vec<u8>,
}

/// A module under construction.
#[derive(Debug, Default)]
pub(crate) struct Module {
    /// Declared types. All of them go in one recursive group so that any two may reference each
    /// other — a class whose method takes its own type, or two mutually-referencing classes, would
    /// otherwise be unorderable.
    types: Vec<SubType>,
    pub(crate) funcs: Vec<Func>,
    pub(crate) exports: Vec<(String, ExportKind, u32)>,
}

impl Module {
    pub(crate) const fn new() -> Self {
        Self {
            types: Vec::new(),
            funcs: Vec::new(),
            exports: Vec::new(),
        }
    }

    /// Append `ty` and return its index.
    ///
    /// A saturated index would be wrong, but it is also unreachable *and* caught: a type section
    /// with more than `u32::MAX` entries cannot write its own count either, so
    /// [`finish`](Self::finish) refuses the module before an index that large can be read.
    pub(crate) fn add_type(&mut self, ty: SubType) -> u32 {
        self.types.push(ty);
        u32::try_from(self.types.len() - 1).unwrap_or(u32::MAX)
    }

    /// The index a defined function will have. Nothing is imported yet, so the function index
    /// space starts at the definitions; a host import would occupy the low indices and shift these,
    /// which is why the mapping is written out rather than assumed at the call site.
    pub(crate) fn func_index(defined: usize) -> u32 {
        u32::try_from(defined).unwrap_or(u32::MAX)
    }

    /// Encode the whole module, or `None` when a length did not fit the `u32` the format spells it
    /// with — a module whose own lengths are wrong is not a smaller module, it is bytes an engine
    /// reads as something else.
    pub(crate) fn finish(&self) -> Option<Vec<u8>> {
        let mut out = Bytes::new();
        out.raw(b"\0asm").raw(&1u32.to_le_bytes());

        if !self.types.is_empty() {
            let mut section = Bytes::new();
            // One vector entry, holding one recursive group of every type.
            section.count(1).byte(0x4E).count(self.types.len());
            for ty in &self.types {
                ty.write(&mut section);
            }
            Self::section(&mut out, 1, &section);
        }

        if !self.funcs.is_empty() {
            let mut section = Bytes::new();
            section.count(self.funcs.len());
            for func in &self.funcs {
                section.u32(func.type_index);
            }
            Self::section(&mut out, 3, &section);
        }

        if !self.exports.is_empty() {
            let mut section = Bytes::new();
            section.count(self.exports.len());
            for (name, kind, index) in &self.exports {
                section
                    .name(name)
                    .byte(match kind {
                        ExportKind::Func => 0x00,
                    })
                    .u32(*index);
            }
            Self::section(&mut out, 7, &section);
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
                body.raw(&func.body).byte(0x0B);
                section.count(body.len());
                section.append(&body);
            }
            Self::section(&mut out, 10, &section);
        }

        (!out.overflow).then(|| out.into_vec())
    }

    /// Write one section: its id, its byte length, then its contents.
    fn section(out: &mut Bytes, id: u8, content: &Bytes) {
        out.byte(id).count(content.len());
        out.append(content);
    }
}
