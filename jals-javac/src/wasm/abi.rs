//! The ABI a linked library carries in its `jals.library` custom section.
//!
//! One artifact, not two: the code a project links against and the description of what that code
//! exports travel in the same file, so they cannot drift. What a consumer needs to state for
//! itself is exactly two things — the type groups it must replay verbatim, because
//! canonicalisation is per group, and the Java API it reads to resolve names — and both are here.
//!
//! # Why the types are data rather than decoded from the module
//!
//! A consumer could in principle re-read the library's type section and replay it. It cannot:
//! [`Module`](super::Module) is an encoder, not a decoder, and the crate is deliberately
//! `no_std + alloc` with no wasm parser in it. So the library writes its own type list — the same
//! values it just encoded — into this section, and the consumer replays those. A section that
//! disagreed with the code would be a bug in one compile, not a drift between two artifacts.
//!
//! # The format
//!
//! Little-endian, LEB128 lengths (the same spelling wasm uses), versioned by a magic and a
//! [`VERSION`]. Every shape writes a tag byte first, so a reader refuses an unknown one rather
//! than misreading it.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use super::encode::{Bytes, CompType, FieldType, HeapType, RefType, StorageType, SubType, ValType};

/// The custom section's name, as it is written into the module.
pub const CUSTOM_SECTION: &str = "jals.library";

/// The format this reader and writer speak.
///
/// Bumped when a shape changes meaning. A consumer that reads a version it does not know refuses
/// the library rather than guessing, which is the only answer a linker can give about an ABI.
pub const VERSION: u32 = 1;

/// The magic that opens the section, so a mangled file is refused before it is parsed.
const MAGIC: &[u8; 8] = b"JALSLIB\0";

/// One Java source a library publishes for the index and the linter.
///
/// The API a consumer resolves against is the library's *own* text — the same text the compile
/// that produced the module read — so an editor or a lint sees the library the code actually
/// links, not a hand-written copy of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The path inside the package, e.g. `demo/Counter.java`.
    pub path: String,
    /// The file's text.
    pub text: String,
}

/// One class the library declared and the type index its struct occupies.
///
/// Indexed by the class's internal name (`demo/Counter`). A consumer needs it to represent a value
/// of the type at all: a call through an imported method takes a receiver of the concrete replayed
/// type, and a cast or a `field` of that type is a concrete heap type too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassType {
    /// The class's internal name, `/`-separated.
    pub name: String,
    /// The class's struct type, as a local index into [`LibraryAbi::types`].
    pub index: u32,
}

/// One exported function and the type index its signature occupies.
///
/// A consumer cannot derive the index: the function's type lives inside a replayed group, and an
/// import that declared a structurally-equal type of its own would canonicalise somewhere else and
/// link against nothing. So the library states which of its types is which export's, and the
/// consumer imports at that index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportType {
    /// The export name — a member key, or an ABI name like `$jals$tag`.
    pub(crate) name: String,
    /// The function type's local index into [`LibraryAbi::types`].
    pub(crate) type_index: u32,
}

/// What a linked library states about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryAbi {
    /// The package's name (`java.base`, `demo`), which is also its link identity.
    pub package: String,
    /// The package's version, for a cache key and a diagnostic.
    pub version: u32,
    /// The Java the package publishes, for the index.
    pub sources: Vec<Source>,
    /// Every class the module declared and where its struct sits.
    pub classes: Vec<ClassType>,
    /// Every function the module exports and the type its signature occupies.
    pub(crate) functions: Vec<ExportType>,
    /// The type index each declared recursive group starts at — the boundaries a consumer must
    /// reproduce *exactly*.
    pub(crate) groups: Vec<usize>,
    /// Every declared type, flattened across the groups.
    pub types: Vec<SubType>,
}

impl LibraryAbi {
    /// The *local* type index the function exported under `name` occupies.
    pub(crate) fn export_type(&self, name: &str) -> Option<u32> {
        self.functions
            .iter()
            .find(|export| export.name == name)
            .map(|export| export.type_index)
    }
}

/// Why a `jals.library` section was not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiError {
    /// The module carries no section under [`CUSTOM_SECTION`].
    Absent,
    /// The bytes ended in the middle of a value.
    Truncated,
    /// A tag byte named a shape this version does not know.
    Unknown(u8),
    /// A LEB128's final byte carried bits the value has no room for.
    IntegerTooLarge,
    /// A string was not valid UTF-8.
    Text,
    /// The section states a version this reader does not know.
    Version(u32),
    /// The bytes are not a module at all.
    NotAModule,
}

impl fmt::Display for AbiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absent => write!(f, "the module carries no `{CUSTOM_SECTION}` section"),
            Self::Truncated => f.write_str("the library ABI section ended early"),
            Self::Unknown(tag) => {
                write!(f, "the library ABI section states an unknown shape ({tag})")
            }
            Self::IntegerTooLarge => f.write_str(
                "the library ABI section holds a LEB128 whose final byte has bits the value has no room for",
            ),
            Self::Text => f.write_str("the library ABI section holds text that is not UTF-8"),
            Self::Version(version) => write!(
                f,
                "the library ABI section is version {version}, and this build reads {VERSION}"
            ),
            Self::NotAModule => f.write_str("the bytes are not a WebAssembly module"),
        }
    }
}

impl LibraryAbi {
    /// Encode the section's payload.
    pub fn write(&self) -> Vec<u8> {
        let mut out = Bytes::new();
        out.raw(MAGIC).u32(VERSION);
        out.name(&self.package).u32(self.version);
        out.count(self.sources.len());
        for source in &self.sources {
            out.name(&source.path).name(&source.text);
        }
        out.count(self.classes.len());
        for class in &self.classes {
            out.name(&class.name).u32(class.index);
        }
        out.count(self.functions.len());
        for export in &self.functions {
            out.name(&export.name).u32(export.type_index);
        }
        out.count(self.groups.len());
        for &start in &self.groups {
            out.u32(u32::try_from(start).unwrap_or(u32::MAX));
        }
        out.count(self.types.len());
        for ty in &self.types {
            Self::write_subtype(&mut out, ty);
        }
        out.into_vec()
    }

    /// Read a section payload written by [`write`](Self::write).
    pub fn read(bytes: &[u8]) -> Result<Self, AbiError> {
        let mut reader = Reader::new(bytes);
        if reader.take(MAGIC.len())? != MAGIC {
            return Err(AbiError::NotAModule);
        }
        let version = reader.u32()?;
        if version != VERSION {
            return Err(AbiError::Version(version));
        }
        let package = reader.string()?;
        let version = reader.u32()?;
        let mut sources = Vec::new();
        for _ in 0..reader.u32()? {
            sources.push(Source {
                path: reader.string()?,
                text: reader.string()?,
            });
        }
        let mut classes = Vec::new();
        for _ in 0..reader.u32()? {
            classes.push(ClassType {
                name: reader.string()?,
                index: reader.u32()?,
            });
        }
        let mut functions = Vec::new();
        for _ in 0..reader.u32()? {
            functions.push(ExportType {
                name: reader.string()?,
                type_index: reader.u32()?,
            });
        }
        let mut groups = Vec::new();
        for _ in 0..reader.u32()? {
            groups.push(usize::try_from(reader.u32()?).unwrap_or(usize::MAX));
        }
        let mut types = Vec::new();
        for _ in 0..reader.u32()? {
            types.push(Self::read_subtype(&mut reader)?);
        }
        Ok(Self {
            package,
            version,
            sources,
            classes,
            functions,
            groups,
            types,
        })
    }

    /// The `jals.library` section of an encoded module, or [`AbiError::Absent`].
    ///
    /// A minimal section walk, which is all reading one needs: the code section is skipped by its
    /// length, not decoded, so this stays a reader of the *container* the ABI travels in.
    pub fn of_module(module: &[u8]) -> Result<Self, AbiError> {
        if module.len() < 8 || &module[..8] != b"\0asm\x01\0\0\0" {
            return Err(AbiError::NotAModule);
        }
        let mut reader = Reader::new(&module[8..]);
        while !reader.is_empty() {
            let id = reader.byte()?;
            let size = usize::try_from(reader.u32()?).map_err(|_| AbiError::Truncated)?;
            let payload = reader.take(size)?;
            if id != 0 {
                continue;
            }
            // A custom section's payload starts with its name; the rest is the contents.
            let mut payload = Reader::new(payload);
            let name = payload.string()?;
            if name == CUSTOM_SECTION {
                return Self::read(payload.rest());
            }
        }
        Err(AbiError::Absent)
    }

    /// One declared type, tag-first.
    fn write_subtype(out: &mut Bytes, ty: &SubType) {
        out.byte(u8::from(ty.is_final));
        match ty.supertype {
            Some(supertype) => {
                out.byte(1).u32(supertype);
            }
            None => {
                out.byte(0);
            }
        }
        match &ty.comp {
            CompType::Func { params, results } => {
                out.byte(0).count(params.len());
                for param in params {
                    Self::write_val(out, *param);
                }
                out.count(results.len());
                for result in results {
                    Self::write_val(out, *result);
                }
            }
            CompType::Struct(fields) => {
                out.byte(1).count(fields.len());
                for field in fields {
                    Self::write_field(out, field);
                }
            }
            CompType::Array(element) => {
                out.byte(2);
                Self::write_field(out, element);
            }
        }
    }

    fn read_subtype(reader: &mut Reader<'_>) -> Result<SubType, AbiError> {
        let is_final = reader.byte()? != 0;
        let supertype = match reader.byte()? {
            0 => None,
            1 => Some(reader.u32()?),
            other => return Err(AbiError::Unknown(other)),
        };
        let comp = match reader.byte()? {
            0 => {
                let mut params = Vec::new();
                for _ in 0..reader.u32()? {
                    params.push(Self::read_val(reader)?);
                }
                let mut results = Vec::new();
                for _ in 0..reader.u32()? {
                    results.push(Self::read_val(reader)?);
                }
                CompType::Func { params, results }
            }
            1 => {
                let mut fields = Vec::new();
                for _ in 0..reader.u32()? {
                    fields.push(Self::read_field(reader)?);
                }
                CompType::Struct(fields)
            }
            2 => CompType::Array(Self::read_field(reader)?),
            other => return Err(AbiError::Unknown(other)),
        };
        Ok(SubType {
            is_final,
            supertype,
            comp,
        })
    }

    fn write_field(out: &mut Bytes, field: &FieldType) {
        let StorageType::Val(ty) = field.storage;
        out.byte(0);
        Self::write_val(out, ty);
        out.byte(u8::from(field.mutable));
    }

    fn read_field(reader: &mut Reader<'_>) -> Result<FieldType, AbiError> {
        match reader.byte()? {
            0 => {}
            other => return Err(AbiError::Unknown(other)),
        }
        let ty = Self::read_val(reader)?;
        let mutable = reader.byte()? != 0;
        Ok(FieldType {
            storage: StorageType::Val(ty),
            mutable,
        })
    }

    fn write_val(out: &mut Bytes, ty: ValType) {
        match ty {
            ValType::I32 => {
                out.byte(0);
            }
            ValType::I64 => {
                out.byte(1);
            }
            ValType::F32 => {
                out.byte(2);
            }
            ValType::F64 => {
                out.byte(3);
            }
            ValType::Ref(reference) => {
                out.byte(4).byte(u8::from(reference.nullable));
                match reference.heap {
                    HeapType::Concrete(index) => {
                        out.byte(0).u32(index);
                    }
                    HeapType::Any => {
                        out.byte(1);
                    }
                    HeapType::None => {
                        out.byte(2);
                    }
                    HeapType::Func => {
                        out.byte(3);
                    }
                }
            }
        }
    }

    fn read_val(reader: &mut Reader<'_>) -> Result<ValType, AbiError> {
        Ok(match reader.byte()? {
            0 => ValType::I32,
            1 => ValType::I64,
            2 => ValType::F32,
            3 => ValType::F64,
            4 => {
                let nullable = reader.byte()? != 0;
                let heap = match reader.byte()? {
                    0 => HeapType::Concrete(reader.u32()?),
                    1 => HeapType::Any,
                    2 => HeapType::None,
                    3 => HeapType::Func,
                    other => return Err(AbiError::Unknown(other)),
                };
                ValType::Ref(RefType { nullable, heap })
            }
            other => return Err(AbiError::Unknown(other)),
        })
    }
}

/// A cursor over section bytes, refusing to read past the end rather than returning zeroes.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    const fn is_empty(&self) -> bool {
        self.at >= self.bytes.len()
    }

    /// What is left, for handing the contents of a nested structure on.
    fn rest(&self) -> &'a [u8] {
        &self.bytes[self.at..]
    }

    fn byte(&mut self) -> Result<u8, AbiError> {
        let byte = *self.bytes.get(self.at).ok_or(AbiError::Truncated)?;
        self.at += 1;
        Ok(byte)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], AbiError> {
        let end = self.at.checked_add(len).ok_or(AbiError::Truncated)?;
        let slice = self.bytes.get(self.at..end).ok_or(AbiError::Truncated)?;
        self.at = end;
        Ok(slice)
    }

    /// LEB128, as wasm spells every length.
    fn u32(&mut self) -> Result<u32, AbiError> {
        let mut value = 0u32;
        for shift in 0..5 {
            let byte = self.byte()?;
            let payload = u32::from(byte & 0x7F);
            // Five bytes carry 35 bits for a 32-bit value, so the last byte's top three data bits
            // (0x70) have no room in the value. Accepting them would silently discard them — the
            // reader would read `FF FF FF FF 7F` as `u32::MAX` — and a reader that guesses at
            // corrupt bytes is the failure this section's format contract exists to refuse.
            if shift == 4 && payload & 0x70 != 0 {
                return Err(AbiError::IntegerTooLarge);
            }
            value |= payload << (shift * 7);
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(AbiError::Truncated)
    }

    fn string(&mut self) -> Result<String, AbiError> {
        let len = usize::try_from(self.u32()?).map_err(|_| AbiError::Truncated)?;
        let bytes = self.take(len)?;
        core::str::from_utf8(bytes)
            .map(String::from)
            .map_err(|_| AbiError::Text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shapes a library's type list actually holds: declared subtyping, a concrete reference
    /// inside a field, an array, and the abstract heap types.
    fn abi() -> LibraryAbi {
        LibraryAbi {
            package: "demo".to_owned(),
            version: 3,
            sources: alloc::vec![Source {
                path: "demo/Counter.java".to_owned(),
                text: "class Counter { int n; }".to_owned(),
            }],
            classes: alloc::vec![ClassType {
                name: "demo/Counter".to_owned(),
                index: 1,
            }],
            functions: alloc::vec![ExportType {
                name: "demo/Counter#twice(I)I".to_owned(),
                type_index: 0,
            }],
            groups: alloc::vec![0, 3],
            types: alloc::vec![
                SubType::plain(CompType::Func {
                    params: alloc::vec![ValType::I32],
                    results: alloc::vec![ValType::Ref(RefType::nullable(HeapType::Concrete(1)))],
                }),
                SubType {
                    is_final: false,
                    supertype: None,
                    comp: CompType::Struct(alloc::vec![FieldType {
                        storage: StorageType::Val(ValType::Ref(RefType {
                            nullable: false,
                            heap: HeapType::Func,
                        })),
                        mutable: true,
                    }]),
                },
                SubType::plain(CompType::Array(FieldType {
                    storage: StorageType::Val(ValType::Ref(RefType::nullable(HeapType::Any))),
                    mutable: false,
                })),
            ],
        }
    }

    #[test]
    fn an_abi_round_trips_through_its_bytes() {
        let abi = abi();
        let bytes = abi.write();
        assert_eq!(LibraryAbi::read(&bytes).expect("reads back"), abi);
    }

    #[test]
    fn a_module_is_read_back_at_its_section() {
        let mut module = super::super::Module::new();
        let abi = abi();
        module.add_custom_section(CUSTOM_SECTION.to_owned(), abi.write());
        let bytes = module.finish().expect("encodes");
        assert_eq!(LibraryAbi::of_module(&bytes).expect("found"), abi);
    }

    #[test]
    fn a_module_without_the_section_is_absent_not_guessed() {
        let bytes = super::super::Module::new().finish().expect("encodes");
        assert_eq!(LibraryAbi::of_module(&bytes), Err(AbiError::Absent));
    }

    /// `FF FF FF FF 7F` is five bytes of payload for a `u32` that has room for four, so the last
    /// byte's top three bits have nowhere to go. Reading it as `u32::MAX` is what the reader used
    /// to do — `checked_shl(28)` can never fail — and silently discarding payload is the failure
    /// this format contract exists to refuse.
    #[test]
    fn a_leb128_with_bits_the_value_cannot_hold_is_refused() {
        assert_eq!(
            Reader::new(&[0xFF, 0xFF, 0xFF, 0xFF, 0x7F]).u32(),
            Err(AbiError::IntegerTooLarge)
        );
        // The same five bytes with the extra bits clear *is* `u32::MAX`, so the check rejects the
        // unused bits and not a five-byte encoding as such.
        assert_eq!(
            Reader::new(&[0xFF, 0xFF, 0xFF, 0xFF, 0x0F]).u32(),
            Ok(u32::MAX)
        );
    }

    /// The same refusal through the public door: a section whose version field is over-long is an
    /// error, not a version someone might match.
    #[test]
    fn an_over_long_leb128_in_a_section_is_an_error_at_read() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0x7F]);
        assert_eq!(LibraryAbi::read(&bytes), Err(AbiError::IntegerTooLarge));
    }
}
