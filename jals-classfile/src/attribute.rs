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

use alloc::boxed::Box;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::annotation::{Annotation, ElementValue, TypeAnnotation};
use crate::bytes::{Input, Reader, Writer};
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
    async fn read<R: Input>(r: &mut Reader<R>, pool: &ConstantPool) -> Result<Self> {
        let name_index = r.u16().await?;
        let length = r.u32().await? as usize;
        let body_bytes = r.bytes(length).await?;
        let body = match pool.utf8(name_index) {
            Some(name) => AttributeBody::parse(&name, &body_bytes, pool).await,
            None => None,
        }
        .unwrap_or(AttributeBody::Unknown(body_bytes));
        Ok(Self { name_index, body })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.name_index);
        let patch = w.reserve_u32_len();
        self.body.write(w);
        w.patch_u32_len(patch);
    }

    /// Read an `attributes_count`-prefixed run of attributes, dispatching each by name.
    pub(crate) async fn read_all<R: Input>(
        r: &mut Reader<R>,
        pool: &ConstantPool,
    ) -> Result<Vec<Self>> {
        r.list(async |r| Self::read(r, pool).await).await
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
    ///
    /// The `Code` and `Record` arms recurse back into [`Attribute::read_all`]
    /// (attribute-in-attribute), so those calls are pinned with `Box::pin` to keep this future finite.
    async fn parse(name: &str, bytes: &[u8], pool: &ConstantPool) -> Option<Self> {
        /// Read the per-parameter annotation lists of a `Runtime*ParameterAnnotations` attribute (a
        /// `u8` parameter count, then a `u16`-counted annotation list per parameter).
        async fn read_parameter_annotations<R: Input>(
            r: &mut Reader<R>,
        ) -> Result<Vec<Vec<Annotation>>> {
            let count = r.u8().await?;
            let mut params = Vec::with_capacity(count as usize);
            for _ in 0..count {
                params.push(r.list(Annotation::read).await?);
            }
            Ok(params)
        }

        let mut r = Reader::new(bytes);
        let body = match name {
            "ConstantValue" => Self::ConstantValue {
                constantvalue_index: r.u16().await.ok()?,
            },
            "Code" => Self::Code(Box::pin(CodeAttribute::read(&mut r, pool)).await.ok()?),
            "StackMapTable" => Self::StackMapTable(r.list(StackMapFrame::read).await.ok()?),
            "Exceptions" => Self::Exceptions {
                exception_index_table: r.u16_list().await.ok()?,
            },
            "InnerClasses" => Self::InnerClasses(r.list(InnerClassEntry::read).await.ok()?),
            "EnclosingMethod" => Self::EnclosingMethod {
                class_index: r.u16().await.ok()?,
                method_index: r.u16().await.ok()?,
            },
            "Synthetic" => Self::Synthetic,
            "Signature" => Self::Signature {
                signature_index: r.u16().await.ok()?,
            },
            "SourceFile" => Self::SourceFile {
                sourcefile_index: r.u16().await.ok()?,
            },
            "SourceDebugExtension" => {
                let n = r.remaining();
                Self::SourceDebugExtension(r.bytes(n).await.ok()?)
            }
            "LineNumberTable" => Self::LineNumberTable(r.list(LineNumberEntry::read).await.ok()?),
            "LocalVariableTable" => {
                Self::LocalVariableTable(r.list(LocalVariableEntry::read).await.ok()?)
            }
            "LocalVariableTypeTable" => {
                Self::LocalVariableTypeTable(r.list(LocalVariableTypeEntry::read).await.ok()?)
            }
            "Deprecated" => Self::Deprecated,
            "RuntimeVisibleAnnotations" => {
                Self::RuntimeVisibleAnnotations(r.list(Annotation::read).await.ok()?)
            }
            "RuntimeInvisibleAnnotations" => {
                Self::RuntimeInvisibleAnnotations(r.list(Annotation::read).await.ok()?)
            }
            "RuntimeVisibleParameterAnnotations" => Self::RuntimeVisibleParameterAnnotations(
                read_parameter_annotations(&mut r).await.ok()?,
            ),
            "RuntimeInvisibleParameterAnnotations" => Self::RuntimeInvisibleParameterAnnotations(
                read_parameter_annotations(&mut r).await.ok()?,
            ),
            "RuntimeVisibleTypeAnnotations" => {
                Self::RuntimeVisibleTypeAnnotations(r.list(TypeAnnotation::read).await.ok()?)
            }
            "RuntimeInvisibleTypeAnnotations" => {
                Self::RuntimeInvisibleTypeAnnotations(r.list(TypeAnnotation::read).await.ok()?)
            }
            "AnnotationDefault" => Self::AnnotationDefault(ElementValue::read(&mut r).await.ok()?),
            "BootstrapMethods" => Self::BootstrapMethods(r.list(BootstrapMethod::read).await.ok()?),
            "MethodParameters" => {
                let count = r.u8().await.ok()?;
                let mut v = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    v.push(MethodParameterEntry::read(&mut r).await.ok()?);
                }
                Self::MethodParameters(v)
            }
            "Module" => Self::Module(ModuleAttribute::read(&mut r).await.ok()?),
            "ModulePackages" => Self::ModulePackages {
                package_index: r.u16_list().await.ok()?,
            },
            "ModuleMainClass" => Self::ModuleMainClass {
                main_class_index: r.u16().await.ok()?,
            },
            "NestHost" => Self::NestHost {
                host_class_index: r.u16().await.ok()?,
            },
            "NestMembers" => Self::NestMembers {
                classes: r.u16_list().await.ok()?,
            },
            "Record" => Self::Record(
                r.list(async |r| Box::pin(RecordComponentInfo::read(r, pool)).await)
                    .await
                    .ok()?,
            ),
            "PermittedSubclasses" => Self::PermittedSubclasses {
                classes: r.u16_list().await.ok()?,
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
    async fn read<R: Input>(r: &mut Reader<R>, pool: &ConstantPool) -> Result<Self> {
        let max_stack = r.u16().await?;
        let max_locals = r.u16().await?;
        let code_length = r.u32().await? as usize;
        let code = Instruction::decode_code(&r.bytes(code_length).await?).await?;
        let exception_table = r.list(ExceptionTableEntry::read).await?;
        let attributes = Attribute::read_all(r, pool).await?;
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
    async fn read<R: Input>(r: &mut Reader<R>) -> Result<Self> {
        Ok(Self {
            start_pc: r.u16().await?,
            end_pc: r.u16().await?,
            handler_pc: r.u16().await?,
            catch_type: r.u16().await?,
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
    async fn read<R: Input>(r: &mut Reader<R>) -> Result<Self> {
        Ok(Self {
            inner_class_info_index: r.u16().await?,
            outer_class_info_index: r.u16().await?,
            inner_name_index: r.u16().await?,
            inner_class_access_flags: r.u16().await?,
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
    async fn read<R: Input>(r: &mut Reader<R>) -> Result<Self> {
        Ok(Self {
            start_pc: r.u16().await?,
            line_number: r.u16().await?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.start_pc);
        w.u16(self.line_number);
    }
}

impl LocalVariableEntry {
    async fn read<R: Input>(r: &mut Reader<R>) -> Result<Self> {
        Ok(Self {
            start_pc: r.u16().await?,
            length: r.u16().await?,
            name_index: r.u16().await?,
            descriptor_index: r.u16().await?,
            index: r.u16().await?,
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
    async fn read<R: Input>(r: &mut Reader<R>) -> Result<Self> {
        Ok(Self {
            start_pc: r.u16().await?,
            length: r.u16().await?,
            name_index: r.u16().await?,
            signature_index: r.u16().await?,
            index: r.u16().await?,
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
    async fn read<R: Input>(r: &mut Reader<R>) -> Result<Self> {
        Ok(Self {
            bootstrap_method_ref: r.u16().await?,
            bootstrap_arguments: r.u16_list().await?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.bootstrap_method_ref);
        w.u16_list(&self.bootstrap_arguments);
    }
}

impl MethodParameterEntry {
    async fn read<R: Input>(r: &mut Reader<R>) -> Result<Self> {
        Ok(Self {
            name_index: r.u16().await?,
            access_flags: r.u16().await?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.name_index);
        w.u16(self.access_flags);
    }
}

impl RecordComponentInfo {
    async fn read<R: Input>(r: &mut Reader<R>, pool: &ConstantPool) -> Result<Self> {
        Ok(Self {
            name_index: r.u16().await?,
            descriptor_index: r.u16().await?,
            attributes: Attribute::read_all(r, pool).await?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.name_index);
        w.u16(self.descriptor_index);
        Attribute::write_all(&self.attributes, w);
    }
}

impl ModuleAttribute {
    async fn read<R: Input>(r: &mut Reader<R>) -> Result<Self> {
        let module_name_index = r.u16().await?;
        let module_flags = r.u16().await?;
        let module_version_index = r.u16().await?;
        let requires = r.list(ModuleRequire::read).await?;
        let exports = r.list(ModuleExport::read).await?;
        let opens = r.list(ModuleOpen::read).await?;
        let uses_index = r.u16_list().await?;
        let provides = r.list(ModuleProvide::read).await?;
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
    async fn read<R: Input>(r: &mut Reader<R>) -> Result<Self> {
        Ok(Self {
            requires_index: r.u16().await?,
            requires_flags: r.u16().await?,
            requires_version_index: r.u16().await?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.requires_index);
        w.u16(self.requires_flags);
        w.u16(self.requires_version_index);
    }
}

impl ModuleExport {
    async fn read<R: Input>(r: &mut Reader<R>) -> Result<Self> {
        Ok(Self {
            exports_index: r.u16().await?,
            exports_flags: r.u16().await?,
            exports_to_index: r.u16_list().await?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.exports_index);
        w.u16(self.exports_flags);
        w.u16_list(&self.exports_to_index);
    }
}

impl ModuleOpen {
    async fn read<R: Input>(r: &mut Reader<R>) -> Result<Self> {
        Ok(Self {
            opens_index: r.u16().await?,
            opens_flags: r.u16().await?,
            opens_to_index: r.u16_list().await?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.opens_index);
        w.u16(self.opens_flags);
        w.u16_list(&self.opens_to_index);
    }
}

impl ModuleProvide {
    async fn read<R: Input>(r: &mut Reader<R>) -> Result<Self> {
        Ok(Self {
            provides_index: r.u16().await?,
            provides_with_index: r.u16_list().await?,
        })
    }

    fn write(&self, w: &mut Writer) {
        w.u16(self.provides_index);
        w.u16_list(&self.provides_with_index);
    }
}
