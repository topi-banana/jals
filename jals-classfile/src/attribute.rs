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
use crate::instruction::{self, Instruction};
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LineNumberEntry {
    /// Bytecode offset where the line begins.
    pub start_pc: u16,
    /// The source line number.
    pub line_number: u16,
}

/// One entry of a `LocalVariableTable`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BootstrapMethod {
    /// `MethodHandle` index of the bootstrap method.
    pub bootstrap_method_ref: u16,
    /// Constant-pool indices of the static bootstrap arguments.
    pub bootstrap_arguments: Vec<u16>,
}

/// One entry of a `MethodParameters` attribute.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleRequire {
    /// `Module` index of the required module.
    pub requires_index: u16,
    /// The directive's flags.
    pub requires_flags: u16,
    /// `Utf8` index of the required version, or 0.
    pub requires_version_index: u16,
}

/// An `exports` directive of a [`ModuleAttribute`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleExport {
    /// `Package` index of the exported package.
    pub exports_index: u16,
    /// The directive's flags.
    pub exports_flags: u16,
    /// `Module` indices the package is exported to (empty = exported to all).
    pub exports_to_index: Vec<u16>,
}

/// An `opens` directive of a [`ModuleAttribute`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleOpen {
    /// `Package` index of the opened package.
    pub opens_index: u16,
    /// The directive's flags.
    pub opens_flags: u16,
    /// `Module` indices the package is opened to (empty = opened to all).
    pub opens_to_index: Vec<u16>,
}

/// A `provides` directive of a [`ModuleAttribute`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleProvide {
    /// `Class` index of the provided service interface.
    pub provides_index: u16,
    /// `Class` indices of the implementations.
    pub provides_with_index: Vec<u16>,
}

impl Attribute {
    fn read(r: &mut Reader<'_>, pool: &ConstantPool) -> Result<Attribute> {
        let name_index = r.u16()?;
        let length = r.u32()? as usize;
        let body_bytes = r.bytes(length)?;
        let body = pool
            .utf8(name_index)
            .and_then(|name| parse_body(&name, body_bytes, pool))
            .unwrap_or_else(|| AttributeBody::Unknown(body_bytes.to_vec()));
        Ok(Attribute { name_index, body })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.name_index);
        let patch = w.reserve_u32_len();
        self.body.write(w);
        w.patch_u32_len(patch);
    }
}

/// Decode a recognised attribute body, or `None` to fall back to [`AttributeBody::Unknown`]. Returns
/// `None` for an unknown name, a parse error, or a body that does not consume exactly `bytes`.
fn parse_body(name: &str, bytes: &[u8], pool: &ConstantPool) -> Option<AttributeBody> {
    let mut r = Reader::new(bytes);
    let body = match name {
        "ConstantValue" => AttributeBody::ConstantValue {
            constantvalue_index: r.u16().ok()?,
        },
        "Code" => AttributeBody::Code(CodeAttribute::read(&mut r, pool).ok()?),
        "StackMapTable" => {
            AttributeBody::StackMapTable(read_list(&mut r, StackMapFrame::read).ok()?)
        }
        "Exceptions" => AttributeBody::Exceptions {
            exception_index_table: read_u16_list(&mut r).ok()?,
        },
        "InnerClasses" => {
            AttributeBody::InnerClasses(read_list(&mut r, InnerClassEntry::read).ok()?)
        }
        "EnclosingMethod" => AttributeBody::EnclosingMethod {
            class_index: r.u16().ok()?,
            method_index: r.u16().ok()?,
        },
        "Synthetic" => AttributeBody::Synthetic,
        "Signature" => AttributeBody::Signature {
            signature_index: r.u16().ok()?,
        },
        "SourceFile" => AttributeBody::SourceFile {
            sourcefile_index: r.u16().ok()?,
        },
        "SourceDebugExtension" => {
            let n = r.remaining();
            AttributeBody::SourceDebugExtension(r.bytes(n).ok()?.to_vec())
        }
        "LineNumberTable" => {
            AttributeBody::LineNumberTable(read_list(&mut r, LineNumberEntry::read).ok()?)
        }
        "LocalVariableTable" => {
            AttributeBody::LocalVariableTable(read_list(&mut r, LocalVariableEntry::read).ok()?)
        }
        "LocalVariableTypeTable" => AttributeBody::LocalVariableTypeTable(
            read_list(&mut r, LocalVariableTypeEntry::read).ok()?,
        ),
        "Deprecated" => AttributeBody::Deprecated,
        "RuntimeVisibleAnnotations" => {
            AttributeBody::RuntimeVisibleAnnotations(read_list(&mut r, Annotation::read).ok()?)
        }
        "RuntimeInvisibleAnnotations" => {
            AttributeBody::RuntimeInvisibleAnnotations(read_list(&mut r, Annotation::read).ok()?)
        }
        "RuntimeVisibleParameterAnnotations" => AttributeBody::RuntimeVisibleParameterAnnotations(
            read_parameter_annotations(&mut r).ok()?,
        ),
        "RuntimeInvisibleParameterAnnotations" => {
            AttributeBody::RuntimeInvisibleParameterAnnotations(
                read_parameter_annotations(&mut r).ok()?,
            )
        }
        "RuntimeVisibleTypeAnnotations" => AttributeBody::RuntimeVisibleTypeAnnotations(
            read_list(&mut r, TypeAnnotation::read).ok()?,
        ),
        "RuntimeInvisibleTypeAnnotations" => AttributeBody::RuntimeInvisibleTypeAnnotations(
            read_list(&mut r, TypeAnnotation::read).ok()?,
        ),
        "AnnotationDefault" => AttributeBody::AnnotationDefault(ElementValue::read(&mut r).ok()?),
        "BootstrapMethods" => {
            AttributeBody::BootstrapMethods(read_list(&mut r, BootstrapMethod::read).ok()?)
        }
        "MethodParameters" => {
            let count = r.u8().ok()?;
            let mut v = Vec::with_capacity(count as usize);
            for _ in 0..count {
                v.push(MethodParameterEntry::read(&mut r).ok()?);
            }
            AttributeBody::MethodParameters(v)
        }
        "Module" => AttributeBody::Module(ModuleAttribute::read(&mut r).ok()?),
        "ModulePackages" => AttributeBody::ModulePackages {
            package_index: read_u16_list(&mut r).ok()?,
        },
        "ModuleMainClass" => AttributeBody::ModuleMainClass {
            main_class_index: r.u16().ok()?,
        },
        "NestHost" => AttributeBody::NestHost {
            host_class_index: r.u16().ok()?,
        },
        "NestMembers" => AttributeBody::NestMembers {
            classes: read_u16_list(&mut r).ok()?,
        },
        "Record" => {
            AttributeBody::Record(read_list(&mut r, |r| RecordComponentInfo::read(r, pool)).ok()?)
        }
        "PermittedSubclasses" => AttributeBody::PermittedSubclasses {
            classes: read_u16_list(&mut r).ok()?,
        },
        _ => return None,
    };
    (r.remaining() == 0).then_some(body)
}

impl AttributeBody {
    fn write(&self, w: &mut Writer) {
        match self {
            AttributeBody::ConstantValue {
                constantvalue_index,
            } => w.u16(*constantvalue_index),
            AttributeBody::Code(c) => c.write(w),
            AttributeBody::StackMapTable(frames) => write_list(frames, w, StackMapFrame::write),
            AttributeBody::Exceptions {
                exception_index_table,
            } => write_u16_list(exception_index_table, w),
            AttributeBody::InnerClasses(entries) => write_list(entries, w, InnerClassEntry::write),
            AttributeBody::EnclosingMethod {
                class_index,
                method_index,
            } => {
                w.u16(*class_index);
                w.u16(*method_index);
            }
            AttributeBody::Synthetic | AttributeBody::Deprecated => {}
            AttributeBody::Signature { signature_index } => w.u16(*signature_index),
            AttributeBody::SourceFile { sourcefile_index } => w.u16(*sourcefile_index),
            AttributeBody::SourceDebugExtension(b) => w.bytes(b),
            AttributeBody::LineNumberTable(entries) => {
                write_list(entries, w, LineNumberEntry::write)
            }
            AttributeBody::LocalVariableTable(entries) => {
                write_list(entries, w, LocalVariableEntry::write);
            }
            AttributeBody::LocalVariableTypeTable(entries) => {
                write_list(entries, w, LocalVariableTypeEntry::write);
            }
            AttributeBody::RuntimeVisibleAnnotations(a)
            | AttributeBody::RuntimeInvisibleAnnotations(a) => write_list(a, w, Annotation::write),
            AttributeBody::RuntimeVisibleParameterAnnotations(p)
            | AttributeBody::RuntimeInvisibleParameterAnnotations(p) => {
                write_parameter_annotations(p, w);
            }
            AttributeBody::RuntimeVisibleTypeAnnotations(a)
            | AttributeBody::RuntimeInvisibleTypeAnnotations(a) => {
                write_list(a, w, TypeAnnotation::write);
            }
            AttributeBody::AnnotationDefault(v) => v.write(w),
            AttributeBody::BootstrapMethods(m) => write_list(m, w, BootstrapMethod::write),
            AttributeBody::MethodParameters(p) => {
                w.u8(p.len() as u8);
                for e in p {
                    e.write(w);
                }
            }
            AttributeBody::Module(m) => m.write(w),
            AttributeBody::ModulePackages { package_index } => write_u16_list(package_index, w),
            AttributeBody::ModuleMainClass { main_class_index } => w.u16(*main_class_index),
            AttributeBody::NestHost { host_class_index } => w.u16(*host_class_index),
            AttributeBody::NestMembers { classes }
            | AttributeBody::PermittedSubclasses { classes } => write_u16_list(classes, w),
            AttributeBody::Record(components) => {
                write_list(components, w, RecordComponentInfo::write)
            }
            AttributeBody::Unknown(b) => w.bytes(b),
        }
    }
}

impl CodeAttribute {
    fn read(r: &mut Reader<'_>, pool: &ConstantPool) -> Result<CodeAttribute> {
        let max_stack = r.u16()?;
        let max_locals = r.u16()?;
        let code_length = r.u32()? as usize;
        let code = instruction::decode_code(r.bytes(code_length)?)?;
        let exception_table = read_list(r, ExceptionTableEntry::read)?;
        let attributes = read_attributes(r, pool)?;
        Ok(CodeAttribute {
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
        let code = instruction::encode_code(&self.code);
        w.u32(code.len() as u32);
        w.bytes(&code);
        write_list(&self.exception_table, w, ExceptionTableEntry::write);
        write_attributes(&self.attributes, w);
    }
}

impl ExceptionTableEntry {
    fn read(r: &mut Reader<'_>) -> Result<ExceptionTableEntry> {
        Ok(ExceptionTableEntry {
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
    fn read(r: &mut Reader<'_>) -> Result<InnerClassEntry> {
        Ok(InnerClassEntry {
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
    fn read(r: &mut Reader<'_>) -> Result<LineNumberEntry> {
        Ok(LineNumberEntry {
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
    fn read(r: &mut Reader<'_>) -> Result<LocalVariableEntry> {
        Ok(LocalVariableEntry {
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
    fn read(r: &mut Reader<'_>) -> Result<LocalVariableTypeEntry> {
        Ok(LocalVariableTypeEntry {
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
    fn read(r: &mut Reader<'_>) -> Result<BootstrapMethod> {
        Ok(BootstrapMethod {
            bootstrap_method_ref: r.u16()?,
            bootstrap_arguments: read_u16_list(r)?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.bootstrap_method_ref);
        write_u16_list(&self.bootstrap_arguments, w);
    }
}

impl MethodParameterEntry {
    fn read(r: &mut Reader<'_>) -> Result<MethodParameterEntry> {
        Ok(MethodParameterEntry {
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
    fn read(r: &mut Reader<'_>, pool: &ConstantPool) -> Result<RecordComponentInfo> {
        Ok(RecordComponentInfo {
            name_index: r.u16()?,
            descriptor_index: r.u16()?,
            attributes: read_attributes(r, pool)?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.name_index);
        w.u16(self.descriptor_index);
        write_attributes(&self.attributes, w);
    }
}

impl ModuleAttribute {
    fn read(r: &mut Reader<'_>) -> Result<ModuleAttribute> {
        let module_name_index = r.u16()?;
        let module_flags = r.u16()?;
        let module_version_index = r.u16()?;
        let requires = read_list(r, ModuleRequire::read)?;
        let exports = read_list(r, ModuleExport::read)?;
        let opens = read_list(r, ModuleOpen::read)?;
        let uses_index = read_u16_list(r)?;
        let provides = read_list(r, ModuleProvide::read)?;
        Ok(ModuleAttribute {
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
        write_list(&self.requires, w, ModuleRequire::write);
        write_list(&self.exports, w, ModuleExport::write);
        write_list(&self.opens, w, ModuleOpen::write);
        write_u16_list(&self.uses_index, w);
        write_list(&self.provides, w, ModuleProvide::write);
    }
}

impl ModuleRequire {
    fn read(r: &mut Reader<'_>) -> Result<ModuleRequire> {
        Ok(ModuleRequire {
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
    fn read(r: &mut Reader<'_>) -> Result<ModuleExport> {
        Ok(ModuleExport {
            exports_index: r.u16()?,
            exports_flags: r.u16()?,
            exports_to_index: read_u16_list(r)?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.exports_index);
        w.u16(self.exports_flags);
        write_u16_list(&self.exports_to_index, w);
    }
}

impl ModuleOpen {
    fn read(r: &mut Reader<'_>) -> Result<ModuleOpen> {
        Ok(ModuleOpen {
            opens_index: r.u16()?,
            opens_flags: r.u16()?,
            opens_to_index: read_u16_list(r)?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.opens_index);
        w.u16(self.opens_flags);
        write_u16_list(&self.opens_to_index, w);
    }
}

impl ModuleProvide {
    fn read(r: &mut Reader<'_>) -> Result<ModuleProvide> {
        Ok(ModuleProvide {
            provides_index: r.u16()?,
            provides_with_index: read_u16_list(r)?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.provides_index);
        write_u16_list(&self.provides_with_index, w);
    }
}

/// Read an `attributes_count`-prefixed run of attributes, dispatching each by name.
pub(crate) fn read_attributes(r: &mut Reader<'_>, pool: &ConstantPool) -> Result<Vec<Attribute>> {
    let count = r.u16()?;
    let mut v = Vec::with_capacity(count as usize);
    for _ in 0..count {
        v.push(Attribute::read(r, pool)?);
    }
    Ok(v)
}

/// Write an `attributes_count`-prefixed run of attributes; the count is derived from the slice.
pub(crate) fn write_attributes(attrs: &[Attribute], w: &mut Writer) {
    w.u16(attrs.len() as u16);
    for a in attrs {
        a.write(w);
    }
}

/// Read a `u16`-counted run of items parsed by `read_one`.
fn read_list<T>(
    r: &mut Reader<'_>,
    read_one: impl Fn(&mut Reader<'_>) -> Result<T>,
) -> Result<Vec<T>> {
    let count = r.u16()?;
    let mut v = Vec::with_capacity(count as usize);
    for _ in 0..count {
        v.push(read_one(r)?);
    }
    Ok(v)
}

/// Write a `u16`-counted run of items via `write_one`.
fn write_list<T>(items: &[T], w: &mut Writer, write_one: impl Fn(&T, &mut Writer)) {
    w.u16(items.len() as u16);
    for item in items {
        write_one(item, w);
    }
}

/// Read a `u16`-counted run of raw `u16` indices.
fn read_u16_list(r: &mut Reader<'_>) -> Result<Vec<u16>> {
    let count = r.u16()?;
    let mut v = Vec::with_capacity(count as usize);
    for _ in 0..count {
        v.push(r.u16()?);
    }
    Ok(v)
}

/// Write a `u16`-counted run of raw `u16` indices.
fn write_u16_list(items: &[u16], w: &mut Writer) {
    w.u16(items.len() as u16);
    for &i in items {
        w.u16(i);
    }
}

/// Read the per-parameter annotation lists of a `Runtime*ParameterAnnotations` attribute (a `u8`
/// parameter count, then a `u16`-counted annotation list per parameter).
fn read_parameter_annotations(r: &mut Reader<'_>) -> Result<Vec<Vec<Annotation>>> {
    let count = r.u8()?;
    let mut params = Vec::with_capacity(count as usize);
    for _ in 0..count {
        params.push(read_list(r, Annotation::read)?);
    }
    Ok(params)
}

fn write_parameter_annotations(params: &[Vec<Annotation>], w: &mut Writer) {
    w.u8(params.len() as u8);
    for annotations in params {
        write_list(annotations, w, Annotation::write);
    }
}
