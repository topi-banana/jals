//! Attributes (JVMS §4.7).
//!
//! An [`Attribute`] keeps its raw `attribute_name_index` plus a decoded [`AttributeBody`]. On read,
//! the body variant is chosen by the attribute's *name* (resolved through the constant pool); a name
//! that is unrecognised — or a body that fails to parse or does not consume exactly its declared
//! length — degrades to [`AttributeBody::Unknown`], which holds the bytes verbatim. So every
//! attribute round-trips byte-for-byte, and `name_index` is always preserved exactly.
//!
//! `StackMapTable` and the instruction stream inside `Code` are modelled in later phases; until then
//! they ride along as `Unknown` / raw `code` bytes.

use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::annotation::{Annotation, ElementValue, TypeAnnotation};
use crate::bytes::{Reader, Writer};
use crate::constant_pool::ConstantPool;
use crate::error::Result;
use crate::instruction::Instruction;
use crate::stackmap::StackMapFrame;

/// A single `attribute_info` (JVMS §4.7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attribute {
    /// `Utf8` constant-pool index of the attribute's name. Always preserved exactly.
    pub name_index: u16,
    /// The decoded body.
    pub body: AttributeBody,
}

/// The decoded body of an [`Attribute`], one variant per JVMS §4.7 attribute (plus a raw fallback).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AttributeBody {
    /// `ConstantValue` (§4.7.2).
    ConstantValue {
        /// Constant-pool index of the field's constant value.
        constantvalue_index: u16,
    },
    /// `Code` (§4.7.3). The instruction stream is raw bytes until the bytecode phase.
    Code(CodeAttribute),
    /// `StackMapTable` (§4.7.4).
    StackMapTable(Vec<StackMapFrame>),
    /// `Exceptions` (§4.7.5): the checked exceptions a method may throw.
    Exceptions {
        /// `Class` indices of the declared exceptions.
        exception_index_table: Vec<u16>,
    },
    /// `InnerClasses` (§4.7.6).
    InnerClasses(Vec<InnerClassEntry>),
    /// `EnclosingMethod` (§4.7.7).
    EnclosingMethod {
        /// `Class` index of the enclosing class.
        class_index: u16,
        /// `NameAndType` index of the enclosing method, or 0.
        method_index: u16,
    },
    /// `Synthetic` (§4.7.8).
    Synthetic,
    /// `Signature` (§4.7.9): the generic-type signature.
    Signature {
        /// `Utf8` index of the signature string.
        signature_index: u16,
    },
    /// `SourceFile` (§4.7.10).
    SourceFile {
        /// `Utf8` index of the source file name.
        sourcefile_index: u16,
    },
    /// `SourceDebugExtension` (§4.7.11): a raw modified-UTF8 blob.
    SourceDebugExtension(Vec<u8>),
    /// `LineNumberTable` (§4.7.12).
    LineNumberTable(Vec<LineNumberEntry>),
    /// `LocalVariableTable` (§4.7.13).
    LocalVariableTable(Vec<LocalVariableEntry>),
    /// `LocalVariableTypeTable` (§4.7.14).
    LocalVariableTypeTable(Vec<LocalVariableTypeEntry>),
    /// `Deprecated` (§4.7.15).
    Deprecated,
    /// `RuntimeVisibleAnnotations` (§4.7.16).
    RuntimeVisibleAnnotations(Vec<Annotation>),
    /// `RuntimeInvisibleAnnotations` (§4.7.17).
    RuntimeInvisibleAnnotations(Vec<Annotation>),
    /// `RuntimeVisibleParameterAnnotations` (§4.7.18): one annotation list per formal parameter.
    RuntimeVisibleParameterAnnotations(Vec<Vec<Annotation>>),
    /// `RuntimeInvisibleParameterAnnotations` (§4.7.19).
    RuntimeInvisibleParameterAnnotations(Vec<Vec<Annotation>>),
    /// `RuntimeVisibleTypeAnnotations` (§4.7.20).
    RuntimeVisibleTypeAnnotations(Vec<TypeAnnotation>),
    /// `RuntimeInvisibleTypeAnnotations` (§4.7.21).
    RuntimeInvisibleTypeAnnotations(Vec<TypeAnnotation>),
    /// `AnnotationDefault` (§4.7.22).
    AnnotationDefault(ElementValue),
    /// `BootstrapMethods` (§4.7.23).
    BootstrapMethods(Vec<BootstrapMethod>),
    /// `MethodParameters` (§4.7.24).
    MethodParameters(Vec<MethodParameterEntry>),
    /// `Module` (§4.7.25).
    Module(ModuleAttribute),
    /// `ModulePackages` (§4.7.26).
    ModulePackages {
        /// `Package` indices.
        package_index: Vec<u16>,
    },
    /// `ModuleMainClass` (§4.7.27).
    ModuleMainClass {
        /// `Class` index of the main class.
        main_class_index: u16,
    },
    /// `NestHost` (§4.7.28).
    NestHost {
        /// `Class` index of the nest host.
        host_class_index: u16,
    },
    /// `NestMembers` (§4.7.29).
    NestMembers {
        /// `Class` indices of the nest members.
        classes: Vec<u16>,
    },
    /// `Record` (§4.7.30).
    Record(Vec<RecordComponentInfo>),
    /// `PermittedSubclasses` (§4.7.31).
    PermittedSubclasses {
        /// `Class` indices of the permitted direct subclasses.
        classes: Vec<u16>,
    },
    /// An attribute whose name is not modelled (e.g. `StackMapTable` until the bytecode phase), or
    /// one that failed to parse cleanly: kept verbatim so it round-trips byte-for-byte.
    Unknown(Vec<u8>),
}

/// The body of a `Code` attribute (JVMS §4.7.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeAttribute {
    /// Maximum operand-stack depth.
    pub max_stack: u16,
    /// Number of local-variable slots.
    pub max_locals: u16,
    /// The decoded bytecode instructions.
    pub code: Vec<Instruction>,
    /// The exception handlers covering this code.
    pub exception_table: Vec<ExceptionTableEntry>,
    /// Nested attributes (`LineNumberTable`, `LocalVariableTable`, `StackMapTable`, …).
    pub attributes: Vec<Attribute>,
}

/// One entry of a `Code` attribute's exception table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExceptionTableEntry {
    /// Inclusive start of the covered range (bytecode offset).
    pub start_pc: u16,
    /// Exclusive end of the covered range.
    pub end_pc: u16,
    /// Offset of the handler.
    pub handler_pc: u16,
    /// `Class` index of the caught type, or 0 for a `finally` (`any`) handler.
    pub catch_type: u16,
}

/// One entry of an `InnerClasses` attribute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InnerClassEntry {
    /// `Class` index of the inner class.
    pub inner_class_info_index: u16,
    /// `Class` index of the enclosing class, or 0.
    pub outer_class_info_index: u16,
    /// `Utf8` index of the simple name, or 0 for an anonymous class.
    pub inner_name_index: u16,
    /// The inner class's access flags.
    pub inner_class_access_flags: u16,
}

/// One entry of a `LineNumberTable`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineNumberEntry {
    /// Bytecode offset where the line begins.
    pub start_pc: u16,
    /// The source line number.
    pub line_number: u16,
}

/// One entry of a `LocalVariableTable`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalVariableEntry {
    /// Start of the variable's live range.
    pub start_pc: u16,
    /// Length of the live range.
    pub length: u16,
    /// `Utf8` index of the variable's name.
    pub name_index: u16,
    /// `Utf8` index of the variable's field descriptor.
    pub descriptor_index: u16,
    /// Local-variable slot index.
    pub index: u16,
}

/// One entry of a `LocalVariableTypeTable` (like [`LocalVariableEntry`] but with a generic signature).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalVariableTypeEntry {
    /// Start of the variable's live range.
    pub start_pc: u16,
    /// Length of the live range.
    pub length: u16,
    /// `Utf8` index of the variable's name.
    pub name_index: u16,
    /// `Utf8` index of the variable's generic signature.
    pub signature_index: u16,
    /// Local-variable slot index.
    pub index: u16,
}

/// One entry of a `BootstrapMethods` attribute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapMethod {
    /// `MethodHandle` index of the bootstrap method.
    pub bootstrap_method_ref: u16,
    /// Constant-pool indices of the static bootstrap arguments.
    pub bootstrap_arguments: Vec<u16>,
}

/// One entry of a `MethodParameters` attribute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MethodParameterEntry {
    /// `Utf8` index of the parameter name, or 0 if unnamed.
    pub name_index: u16,
    /// The parameter's access flags (`final`, `synthetic`, `mandated`).
    pub access_flags: u16,
}

/// One component of a `Record` attribute.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordComponentInfo {
    /// `Utf8` index of the component's name.
    pub name_index: u16,
    /// `Utf8` index of the component's field descriptor.
    pub descriptor_index: u16,
    /// The component's attributes (`Signature`, annotations, …).
    pub attributes: Vec<Attribute>,
}

/// The body of a `Module` attribute (JVMS §4.7.25).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleAttribute {
    /// `Module` index of this module's name.
    pub module_name_index: u16,
    /// The module's flags.
    pub module_flags: u16,
    /// `Utf8` index of the module version, or 0.
    pub module_version_index: u16,
    /// `requires` directives.
    pub requires: Vec<ModuleRequire>,
    /// `exports` directives.
    pub exports: Vec<ModuleExport>,
    /// `opens` directives.
    pub opens: Vec<ModuleOpen>,
    /// `Class` indices of the services this module uses.
    pub uses_index: Vec<u16>,
    /// `provides` directives.
    pub provides: Vec<ModuleProvide>,
}

/// A `requires` directive of a [`ModuleAttribute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleRequire {
    /// `Module` index of the required module.
    pub requires_index: u16,
    /// The directive's flags.
    pub requires_flags: u16,
    /// `Utf8` index of the required version, or 0.
    pub requires_version_index: u16,
}

/// An `exports` directive of a [`ModuleAttribute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleExport {
    /// `Package` index of the exported package.
    pub exports_index: u16,
    /// The directive's flags.
    pub exports_flags: u16,
    /// `Module` indices the package is exported to (empty = exported to all).
    pub exports_to_index: Vec<u16>,
}

/// An `opens` directive of a [`ModuleAttribute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleOpen {
    /// `Package` index of the opened package.
    pub opens_index: u16,
    /// The directive's flags.
    pub opens_flags: u16,
    /// `Module` indices the package is opened to (empty = opened to all).
    pub opens_to_index: Vec<u16>,
}

/// A `provides` directive of a [`ModuleAttribute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleProvide {
    /// `Class` index of the provided service interface.
    pub provides_index: u16,
    /// `Class` indices of the implementations.
    pub provides_with_index: Vec<u16>,
}

impl Attribute {
    fn read(r: &mut Reader<'_>, pool: &ConstantPool) -> Result<Self> {
        let name_index = r.u16()?;
        let length = r.u32()? as usize;
        let body_bytes = r.bytes(length)?;
        let body = pool
            .utf8(name_index)
            .and_then(|name| AttributeBody::parse(&name, body_bytes, pool))
            .unwrap_or_else(|| AttributeBody::Unknown(body_bytes.to_vec()));
        Ok(Self { name_index, body })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.name_index);
        let patch = w.reserve_u32_len();
        self.body.write(w);
        w.patch_u32_len(patch);
    }

    /// Read an `attributes_count`-prefixed run of attributes, dispatching each by name.
    pub(crate) fn read_all(r: &mut Reader<'_>, pool: &ConstantPool) -> Result<Vec<Self>> {
        r.list(|r| Self::read(r, pool))
    }

    /// Write an `attributes_count`-prefixed run of attributes; the count is derived from the slice.
    pub(crate) fn write_all(attrs: &[Self], w: &mut Writer) {
        w.list(attrs, Self::write);
    }
}

impl AttributeBody {
    /// Decode a recognised attribute body, or `None` to fall back to [`AttributeBody::Unknown`].
    /// Returns `None` for an unknown name, a parse error, or a body that does not consume exactly
    /// `bytes`.
    fn parse(name: &str, bytes: &[u8], pool: &ConstantPool) -> Option<Self> {
        /// Read the per-parameter annotation lists of a `Runtime*ParameterAnnotations` attribute (a
        /// `u8` parameter count, then a `u16`-counted annotation list per parameter).
        fn read_parameter_annotations(r: &mut Reader<'_>) -> Result<Vec<Vec<Annotation>>> {
            let count = r.u8()?;
            let mut params = Vec::with_capacity(count as usize);
            for _ in 0..count {
                params.push(r.list(Annotation::read)?);
            }
            Ok(params)
        }

        let mut r = Reader::new(bytes);
        let body = match name {
            "ConstantValue" => Self::ConstantValue {
                constantvalue_index: r.u16().ok()?,
            },
            "Code" => Self::Code(CodeAttribute::read(&mut r, pool).ok()?),
            "StackMapTable" => Self::StackMapTable(r.list(StackMapFrame::read).ok()?),
            "Exceptions" => Self::Exceptions {
                exception_index_table: r.u16_list().ok()?,
            },
            "InnerClasses" => Self::InnerClasses(r.list(InnerClassEntry::read).ok()?),
            "EnclosingMethod" => Self::EnclosingMethod {
                class_index: r.u16().ok()?,
                method_index: r.u16().ok()?,
            },
            "Synthetic" => Self::Synthetic,
            "Signature" => Self::Signature {
                signature_index: r.u16().ok()?,
            },
            "SourceFile" => Self::SourceFile {
                sourcefile_index: r.u16().ok()?,
            },
            "SourceDebugExtension" => {
                let n = r.remaining();
                Self::SourceDebugExtension(r.bytes(n).ok()?.to_vec())
            }
            "LineNumberTable" => Self::LineNumberTable(r.list(LineNumberEntry::read).ok()?),
            "LocalVariableTable" => {
                Self::LocalVariableTable(r.list(LocalVariableEntry::read).ok()?)
            }
            "LocalVariableTypeTable" => {
                Self::LocalVariableTypeTable(r.list(LocalVariableTypeEntry::read).ok()?)
            }
            "Deprecated" => Self::Deprecated,
            "RuntimeVisibleAnnotations" => {
                Self::RuntimeVisibleAnnotations(r.list(Annotation::read).ok()?)
            }
            "RuntimeInvisibleAnnotations" => {
                Self::RuntimeInvisibleAnnotations(r.list(Annotation::read).ok()?)
            }
            "RuntimeVisibleParameterAnnotations" => {
                Self::RuntimeVisibleParameterAnnotations(read_parameter_annotations(&mut r).ok()?)
            }
            "RuntimeInvisibleParameterAnnotations" => {
                Self::RuntimeInvisibleParameterAnnotations(read_parameter_annotations(&mut r).ok()?)
            }
            "RuntimeVisibleTypeAnnotations" => {
                Self::RuntimeVisibleTypeAnnotations(r.list(TypeAnnotation::read).ok()?)
            }
            "RuntimeInvisibleTypeAnnotations" => {
                Self::RuntimeInvisibleTypeAnnotations(r.list(TypeAnnotation::read).ok()?)
            }
            "AnnotationDefault" => Self::AnnotationDefault(ElementValue::read(&mut r).ok()?),
            "BootstrapMethods" => Self::BootstrapMethods(r.list(BootstrapMethod::read).ok()?),
            "MethodParameters" => {
                let count = r.u8().ok()?;
                let mut v = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    v.push(MethodParameterEntry::read(&mut r).ok()?);
                }
                Self::MethodParameters(v)
            }
            "Module" => Self::Module(ModuleAttribute::read(&mut r).ok()?),
            "ModulePackages" => Self::ModulePackages {
                package_index: r.u16_list().ok()?,
            },
            "ModuleMainClass" => Self::ModuleMainClass {
                main_class_index: r.u16().ok()?,
            },
            "NestHost" => Self::NestHost {
                host_class_index: r.u16().ok()?,
            },
            "NestMembers" => Self::NestMembers {
                classes: r.u16_list().ok()?,
            },
            "Record" => Self::Record(r.list(|r| RecordComponentInfo::read(r, pool)).ok()?),
            "PermittedSubclasses" => Self::PermittedSubclasses {
                classes: r.u16_list().ok()?,
            },
            _ => return None,
        };
        (r.remaining() == 0).then_some(body)
    }

    fn write(&self, w: &mut Writer) {
        /// Write the per-parameter annotation lists of a `Runtime*ParameterAnnotations` attribute (a
        /// `u8` parameter count, then a `u16`-counted annotation list per parameter).
        fn write_parameter_annotations(params: &[Vec<Annotation>], w: &mut Writer) {
            w.u8(params.len() as u8);
            for annotations in params {
                w.list(annotations, Annotation::write);
            }
        }

        match self {
            Self::ConstantValue {
                constantvalue_index,
            } => w.u16(*constantvalue_index),
            Self::Code(c) => c.write(w),
            Self::StackMapTable(frames) => w.list(frames, StackMapFrame::write),
            Self::Exceptions {
                exception_index_table,
            } => w.u16_list(exception_index_table),
            Self::InnerClasses(entries) => w.list(entries, InnerClassEntry::write),
            Self::EnclosingMethod {
                class_index,
                method_index,
            } => {
                w.u16(*class_index);
                w.u16(*method_index);
            }
            Self::Synthetic | Self::Deprecated => {}
            Self::Signature { signature_index } => w.u16(*signature_index),
            Self::SourceFile { sourcefile_index } => w.u16(*sourcefile_index),
            Self::SourceDebugExtension(b) | Self::Unknown(b) => w.bytes(b),
            Self::LineNumberTable(entries) => w.list(entries, LineNumberEntry::write),
            Self::LocalVariableTable(entries) => {
                w.list(entries, LocalVariableEntry::write);
            }
            Self::LocalVariableTypeTable(entries) => {
                w.list(entries, LocalVariableTypeEntry::write);
            }
            Self::RuntimeVisibleAnnotations(a) | Self::RuntimeInvisibleAnnotations(a) => {
                w.list(a, Annotation::write);
            }
            Self::RuntimeVisibleParameterAnnotations(p)
            | Self::RuntimeInvisibleParameterAnnotations(p) => {
                write_parameter_annotations(p, w);
            }
            Self::RuntimeVisibleTypeAnnotations(a) | Self::RuntimeInvisibleTypeAnnotations(a) => {
                w.list(a, TypeAnnotation::write);
            }
            Self::AnnotationDefault(v) => v.write(w),
            Self::BootstrapMethods(m) => w.list(m, BootstrapMethod::write),
            Self::MethodParameters(p) => {
                w.u8(p.len() as u8);
                for e in p {
                    e.write(w);
                }
            }
            Self::Module(m) => m.write(w),
            Self::ModulePackages { package_index } => w.u16_list(package_index),
            Self::ModuleMainClass { main_class_index } => w.u16(*main_class_index),
            Self::NestHost { host_class_index } => w.u16(*host_class_index),
            Self::NestMembers { classes } | Self::PermittedSubclasses { classes } => {
                w.u16_list(classes);
            }
            Self::Record(components) => w.list(components, RecordComponentInfo::write),
        }
    }
}

impl CodeAttribute {
    fn read(r: &mut Reader<'_>, pool: &ConstantPool) -> Result<Self> {
        let max_stack = r.u16()?;
        let max_locals = r.u16()?;
        let code_length = r.u32()? as usize;
        let code = Instruction::decode_code(r.bytes(code_length)?)?;
        let exception_table = r.list(ExceptionTableEntry::read)?;
        let attributes = Attribute::read_all(r, pool)?;
        Ok(Self {
            max_stack,
            max_locals,
            code,
            exception_table,
            attributes,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.max_stack);
        w.u16(self.max_locals);
        let code = Instruction::encode_code(&self.code);
        w.u32(code.len() as u32);
        w.bytes(&code);
        w.list(&self.exception_table, ExceptionTableEntry::write);
        Attribute::write_all(&self.attributes, w);
    }
}

impl ExceptionTableEntry {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            start_pc: r.u16()?,
            end_pc: r.u16()?,
            handler_pc: r.u16()?,
            catch_type: r.u16()?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.start_pc);
        w.u16(self.end_pc);
        w.u16(self.handler_pc);
        w.u16(self.catch_type);
    }
}

impl InnerClassEntry {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            inner_class_info_index: r.u16()?,
            outer_class_info_index: r.u16()?,
            inner_name_index: r.u16()?,
            inner_class_access_flags: r.u16()?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.inner_class_info_index);
        w.u16(self.outer_class_info_index);
        w.u16(self.inner_name_index);
        w.u16(self.inner_class_access_flags);
    }
}

impl LineNumberEntry {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            start_pc: r.u16()?,
            line_number: r.u16()?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.start_pc);
        w.u16(self.line_number);
    }
}

impl LocalVariableEntry {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            start_pc: r.u16()?,
            length: r.u16()?,
            name_index: r.u16()?,
            descriptor_index: r.u16()?,
            index: r.u16()?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.start_pc);
        w.u16(self.length);
        w.u16(self.name_index);
        w.u16(self.descriptor_index);
        w.u16(self.index);
    }
}

impl LocalVariableTypeEntry {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            start_pc: r.u16()?,
            length: r.u16()?,
            name_index: r.u16()?,
            signature_index: r.u16()?,
            index: r.u16()?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.start_pc);
        w.u16(self.length);
        w.u16(self.name_index);
        w.u16(self.signature_index);
        w.u16(self.index);
    }
}

impl BootstrapMethod {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            bootstrap_method_ref: r.u16()?,
            bootstrap_arguments: r.u16_list()?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.bootstrap_method_ref);
        w.u16_list(&self.bootstrap_arguments);
    }
}

impl MethodParameterEntry {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            name_index: r.u16()?,
            access_flags: r.u16()?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.name_index);
        w.u16(self.access_flags);
    }
}

impl RecordComponentInfo {
    fn read(r: &mut Reader<'_>, pool: &ConstantPool) -> Result<Self> {
        Ok(Self {
            name_index: r.u16()?,
            descriptor_index: r.u16()?,
            attributes: Attribute::read_all(r, pool)?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.name_index);
        w.u16(self.descriptor_index);
        Attribute::write_all(&self.attributes, w);
    }
}

impl ModuleAttribute {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        let module_name_index = r.u16()?;
        let module_flags = r.u16()?;
        let module_version_index = r.u16()?;
        let requires = r.list(ModuleRequire::read)?;
        let exports = r.list(ModuleExport::read)?;
        let opens = r.list(ModuleOpen::read)?;
        let uses_index = r.u16_list()?;
        let provides = r.list(ModuleProvide::read)?;
        Ok(Self {
            module_name_index,
            module_flags,
            module_version_index,
            requires,
            exports,
            opens,
            uses_index,
            provides,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.module_name_index);
        w.u16(self.module_flags);
        w.u16(self.module_version_index);
        w.list(&self.requires, ModuleRequire::write);
        w.list(&self.exports, ModuleExport::write);
        w.list(&self.opens, ModuleOpen::write);
        w.u16_list(&self.uses_index);
        w.list(&self.provides, ModuleProvide::write);
    }
}

impl ModuleRequire {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            requires_index: r.u16()?,
            requires_flags: r.u16()?,
            requires_version_index: r.u16()?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.requires_index);
        w.u16(self.requires_flags);
        w.u16(self.requires_version_index);
    }
}

impl ModuleExport {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            exports_index: r.u16()?,
            exports_flags: r.u16()?,
            exports_to_index: r.u16_list()?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.exports_index);
        w.u16(self.exports_flags);
        w.u16_list(&self.exports_to_index);
    }
}

impl ModuleOpen {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            opens_index: r.u16()?,
            opens_flags: r.u16()?,
            opens_to_index: r.u16_list()?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.opens_index);
        w.u16(self.opens_flags);
        w.u16_list(&self.opens_to_index);
    }
}

impl ModuleProvide {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            provides_index: r.u16()?,
            provides_with_index: r.u16_list()?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.provides_index);
        w.u16_list(&self.provides_with_index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::Annotation;

    /// A pool with only the sentinel — enough for every body that does not resolve a name (all the
    /// ones exercised here parse straight from bytes).
    fn empty_pool() -> ConstantPool {
        ConstantPool::read(&mut Reader::new(&[0x00, 0x01])).expect("empty pool")
    }

    /// `write` a body, then `parse` it back under its attribute name: the name must select the right
    /// variant (so a dropped match arm is caught) and the bytes must survive the round-trip (so a
    /// dropped `write` is caught).
    fn roundtrip_body(name: &str, body: &AttributeBody) {
        let pool = empty_pool();
        let mut w = Writer::new();
        body.write(&mut w);
        let bytes = w.into_vec();
        assert_eq!(
            AttributeBody::parse(name, &bytes, &pool).as_ref(),
            Some(body),
            "{name} did not round-trip"
        );
    }

    #[test]
    fn each_attribute_name_selects_and_round_trips_its_body() {
        roundtrip_body(
            "Exceptions",
            &AttributeBody::Exceptions {
                exception_index_table: vec![3, 5],
            },
        );
        roundtrip_body(
            "EnclosingMethod",
            &AttributeBody::EnclosingMethod {
                class_index: 5,
                method_index: 7,
            },
        );
        roundtrip_body("Synthetic", &AttributeBody::Synthetic);
        roundtrip_body(
            "SourceDebugExtension",
            &AttributeBody::SourceDebugExtension(vec![1, 2, 3]),
        );
        roundtrip_body(
            "LocalVariableTable",
            &AttributeBody::LocalVariableTable(vec![LocalVariableEntry {
                start_pc: 1,
                length: 2,
                name_index: 3,
                descriptor_index: 4,
                index: 5,
            }]),
        );
        roundtrip_body(
            "LocalVariableTypeTable",
            &AttributeBody::LocalVariableTypeTable(vec![LocalVariableTypeEntry {
                start_pc: 1,
                length: 2,
                name_index: 3,
                signature_index: 4,
                index: 5,
            }]),
        );
        roundtrip_body(
            "RuntimeInvisibleAnnotations",
            &AttributeBody::RuntimeInvisibleAnnotations(vec![Annotation {
                type_index: 9,
                element_value_pairs: Vec::new(),
            }]),
        );
        roundtrip_body(
            "RuntimeVisibleParameterAnnotations",
            &AttributeBody::RuntimeVisibleParameterAnnotations(vec![
                vec![Annotation {
                    type_index: 9,
                    element_value_pairs: Vec::new(),
                }],
                Vec::new(),
            ]),
        );
        roundtrip_body(
            "RuntimeInvisibleParameterAnnotations",
            &AttributeBody::RuntimeInvisibleParameterAnnotations(vec![vec![Annotation {
                type_index: 9,
                element_value_pairs: Vec::new(),
            }]]),
        );
        roundtrip_body(
            "AnnotationDefault",
            &AttributeBody::AnnotationDefault(ElementValue::Const {
                tag: b'I',
                const_value_index: 1,
            }),
        );
        roundtrip_body(
            "ModulePackages",
            &AttributeBody::ModulePackages {
                package_index: vec![2, 4],
            },
        );
        roundtrip_body(
            "ModuleMainClass",
            &AttributeBody::ModuleMainClass {
                main_class_index: 3,
            },
        );
        roundtrip_body(
            "PermittedSubclasses",
            &AttributeBody::PermittedSubclasses {
                classes: vec![4, 6],
            },
        );
    }

    #[test]
    fn an_unmodelled_attribute_name_is_not_parsed() {
        assert_eq!(
            AttributeBody::parse("NotAnAttribute", &[], &empty_pool()),
            None
        );
    }

    #[test]
    fn exception_table_entry_round_trips() {
        let entry = ExceptionTableEntry {
            start_pc: 1,
            end_pc: 2,
            handler_pc: 3,
            catch_type: 4,
        };
        let mut w = Writer::new();
        entry.write(&mut w);
        let bytes = w.into_vec();
        let mut r = Reader::new(&bytes);
        assert_eq!(ExceptionTableEntry::read(&mut r).unwrap(), entry);
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn module_open_and_provide_round_trip() {
        let open = ModuleOpen {
            opens_index: 1,
            opens_flags: 2,
            opens_to_index: vec![3, 4],
        };
        let mut w = Writer::new();
        open.write(&mut w);
        let bytes = w.into_vec();
        let mut r = Reader::new(&bytes);
        assert_eq!(ModuleOpen::read(&mut r).unwrap(), open);
        assert_eq!(r.remaining(), 0);

        let provide = ModuleProvide {
            provides_index: 5,
            provides_with_index: vec![6, 7],
        };
        let mut w = Writer::new();
        provide.write(&mut w);
        let bytes = w.into_vec();
        let mut r = Reader::new(&bytes);
        assert_eq!(ModuleProvide::read(&mut r).unwrap(), provide);
        assert_eq!(r.remaining(), 0);
    }
}
