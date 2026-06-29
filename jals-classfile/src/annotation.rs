//! Annotation structures shared by the `Runtime[In]Visible[Parameter|Type]Annotations` and
//! `AnnotationDefault` attributes (JVMS §4.7.16–§4.7.22).

use serde::{Deserialize, Serialize};

use crate::bytes::{Reader, Writer};
use crate::error::{ClassfileError, Result};

/// A single `annotation` (JVMS §4.7.16).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    /// `Utf8` index of the annotation type's field descriptor.
    pub type_index: u16,
    /// The `(name, value)` pairs of the annotation's elements.
    pub element_value_pairs: Vec<ElementValuePair>,
}

/// One `element_value_pair` of an [`Annotation`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElementValuePair {
    /// `Utf8` index of the element's name.
    pub element_name_index: u16,
    /// The element's value.
    pub value: ElementValue,
}

/// An `element_value` (JVMS §4.7.16.1): a tagged union of constant, enum, class, nested-annotation,
/// and array values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ElementValue {
    /// A primitive or `String` constant; `tag` is one of `BCDFIJSZs` and `const_value_index` points
    /// at the matching constant-pool entry (a `Utf8` for `s`).
    Const {
        /// The value's type tag (`B`, `C`, …, `s`).
        tag: u8,
        /// Constant-pool index of the value.
        const_value_index: u16,
    },
    /// An enum constant (`e`).
    Enum {
        /// `Utf8` index of the enum type's descriptor.
        type_name_index: u16,
        /// `Utf8` index of the constant's name.
        const_name_index: u16,
    },
    /// A class literal (`c`).
    Class {
        /// `Utf8` index of the return descriptor naming the class.
        class_info_index: u16,
    },
    /// A nested annotation (`@`).
    Annotation(Annotation),
    /// An array of element values (`[`).
    Array(Vec<ElementValue>),
}

/// A `type_annotation` (JVMS §4.7.20): an annotation plus the location in a type it targets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypeAnnotation {
    /// What kind of program element the annotated type appears in.
    pub target_info: TargetInfo,
    /// The path from the annotated type to the specific part it applies to.
    pub target_path: Vec<TypePathEntry>,
    /// `Utf8` index of the annotation type's field descriptor.
    pub type_index: u16,
    /// The annotation's element values.
    pub element_value_pairs: Vec<ElementValuePair>,
}

/// The `target_type` + `target_info` union of a [`TypeAnnotation`] (JVMS §4.7.20.1). The `target_type`
/// byte is reconstructed from the variant on write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TargetInfo {
    /// `type_parameter_target` (0x00 / 0x01).
    TypeParameter {
        /// The `target_type` byte (0x00 or 0x01).
        target_type: u8,
        /// Index of the targeted type parameter.
        type_parameter_index: u8,
    },
    /// `supertype_target` (0x10).
    Supertype {
        /// Index into the supertype list (0xFFFF means the superclass).
        supertype_index: u16,
    },
    /// `type_parameter_bound_target` (0x11 / 0x12).
    TypeParameterBound {
        /// The `target_type` byte (0x11 or 0x12).
        target_type: u8,
        /// Index of the targeted type parameter.
        type_parameter_index: u8,
        /// Index of the targeted bound.
        bound_index: u8,
    },
    /// `empty_target` (0x13 / 0x14 / 0x15).
    Empty {
        /// The `target_type` byte.
        target_type: u8,
    },
    /// `formal_parameter_target` (0x16).
    FormalParameter {
        /// Index of the targeted formal parameter.
        formal_parameter_index: u8,
    },
    /// `throws_target` (0x17).
    Throws {
        /// Index into the `Exceptions` attribute / `throws` clause.
        throws_type_index: u16,
    },
    /// `localvar_target` (0x40 / 0x41).
    LocalVar {
        /// The `target_type` byte (0x40 or 0x41).
        target_type: u8,
        /// The live ranges of the targeted local variable.
        table: Vec<LocalVarTargetEntry>,
    },
    /// `catch_target` (0x42).
    Catch {
        /// Index into the `Code` attribute's exception table.
        exception_table_index: u16,
    },
    /// `offset_target` (0x43–0x46).
    Offset {
        /// The `target_type` byte (0x43–0x46).
        target_type: u8,
        /// Bytecode offset of the targeted instruction.
        offset: u16,
    },
    /// `type_argument_target` (0x47–0x4B).
    TypeArgument {
        /// The `target_type` byte (0x47–0x4B).
        target_type: u8,
        /// Bytecode offset of the targeted instruction.
        offset: u16,
        /// Index of the targeted type argument.
        type_argument_index: u8,
    },
}

/// One live range in a [`TargetInfo::LocalVar`] table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalVarTargetEntry {
    /// Start of the range (bytecode offset).
    pub start_pc: u16,
    /// Length of the range.
    pub length: u16,
    /// Local-variable slot index.
    pub index: u16,
}

/// One step of a [`TypeAnnotation::target_path`] (JVMS §4.7.20.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypePathEntry {
    /// How to step (array, nested, wildcard bound, or type argument).
    pub type_path_kind: u8,
    /// Which type argument to step into (only meaningful for kind 3).
    pub type_argument_index: u8,
}

impl Annotation {
    pub(crate) fn read(r: &mut Reader<'_>) -> Result<Annotation> {
        let type_index = r.u16()?;
        let count = r.u16()?;
        let mut element_value_pairs = Vec::with_capacity(count as usize);
        for _ in 0..count {
            element_value_pairs.push(ElementValuePair {
                element_name_index: r.u16()?,
                value: ElementValue::read(r)?,
            });
        }
        Ok(Annotation {
            type_index,
            element_value_pairs,
        })
    }

    pub(crate) fn write(&self, w: &mut Writer) {
        w.u16(self.type_index);
        w.u16(self.element_value_pairs.len() as u16);
        for pair in &self.element_value_pairs {
            w.u16(pair.element_name_index);
            pair.value.write(w);
        }
    }
}

impl ElementValue {
    pub(crate) fn read(r: &mut Reader<'_>) -> Result<ElementValue> {
        let tag = r.u8()?;
        Ok(match tag {
            b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' | b's' => ElementValue::Const {
                tag,
                const_value_index: r.u16()?,
            },
            b'e' => ElementValue::Enum {
                type_name_index: r.u16()?,
                const_name_index: r.u16()?,
            },
            b'c' => ElementValue::Class {
                class_info_index: r.u16()?,
            },
            b'@' => ElementValue::Annotation(Annotation::read(r)?),
            b'[' => {
                let count = r.u16()?;
                let mut values = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    values.push(ElementValue::read(r)?);
                }
                ElementValue::Array(values)
            }
            _ => return Err(ClassfileError::Malformed("element_value tag")),
        })
    }

    pub(crate) fn write(&self, w: &mut Writer) {
        match self {
            ElementValue::Const {
                tag,
                const_value_index,
            } => {
                w.u8(*tag);
                w.u16(*const_value_index);
            }
            ElementValue::Enum {
                type_name_index,
                const_name_index,
            } => {
                w.u8(b'e');
                w.u16(*type_name_index);
                w.u16(*const_name_index);
            }
            ElementValue::Class { class_info_index } => {
                w.u8(b'c');
                w.u16(*class_info_index);
            }
            ElementValue::Annotation(a) => {
                w.u8(b'@');
                a.write(w);
            }
            ElementValue::Array(values) => {
                w.u8(b'[');
                w.u16(values.len() as u16);
                for v in values {
                    v.write(w);
                }
            }
        }
    }
}

impl TypeAnnotation {
    pub(crate) fn read(r: &mut Reader<'_>) -> Result<TypeAnnotation> {
        let target_info = TargetInfo::read(r)?;
        let path_length = r.u8()?;
        let mut target_path = Vec::with_capacity(path_length as usize);
        for _ in 0..path_length {
            target_path.push(TypePathEntry {
                type_path_kind: r.u8()?,
                type_argument_index: r.u8()?,
            });
        }
        let type_index = r.u16()?;
        let count = r.u16()?;
        let mut element_value_pairs = Vec::with_capacity(count as usize);
        for _ in 0..count {
            element_value_pairs.push(ElementValuePair {
                element_name_index: r.u16()?,
                value: ElementValue::read(r)?,
            });
        }
        Ok(TypeAnnotation {
            target_info,
            target_path,
            type_index,
            element_value_pairs,
        })
    }

    pub(crate) fn write(&self, w: &mut Writer) {
        self.target_info.write(w);
        w.u8(self.target_path.len() as u8);
        for step in &self.target_path {
            w.u8(step.type_path_kind);
            w.u8(step.type_argument_index);
        }
        w.u16(self.type_index);
        w.u16(self.element_value_pairs.len() as u16);
        for pair in &self.element_value_pairs {
            w.u16(pair.element_name_index);
            pair.value.write(w);
        }
    }
}

impl TargetInfo {
    fn read(r: &mut Reader<'_>) -> Result<TargetInfo> {
        let target_type = r.u8()?;
        Ok(match target_type {
            0x00 | 0x01 => TargetInfo::TypeParameter {
                target_type,
                type_parameter_index: r.u8()?,
            },
            0x10 => TargetInfo::Supertype {
                supertype_index: r.u16()?,
            },
            0x11 | 0x12 => TargetInfo::TypeParameterBound {
                target_type,
                type_parameter_index: r.u8()?,
                bound_index: r.u8()?,
            },
            0x13..=0x15 => TargetInfo::Empty { target_type },
            0x16 => TargetInfo::FormalParameter {
                formal_parameter_index: r.u8()?,
            },
            0x17 => TargetInfo::Throws {
                throws_type_index: r.u16()?,
            },
            0x40 | 0x41 => {
                let count = r.u16()?;
                let mut table = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    table.push(LocalVarTargetEntry {
                        start_pc: r.u16()?,
                        length: r.u16()?,
                        index: r.u16()?,
                    });
                }
                TargetInfo::LocalVar { target_type, table }
            }
            0x42 => TargetInfo::Catch {
                exception_table_index: r.u16()?,
            },
            0x43..=0x46 => TargetInfo::Offset {
                target_type,
                offset: r.u16()?,
            },
            0x47..=0x4B => TargetInfo::TypeArgument {
                target_type,
                offset: r.u16()?,
                type_argument_index: r.u8()?,
            },
            _ => return Err(ClassfileError::Malformed("type_annotation target_type")),
        })
    }

    fn write(&self, w: &mut Writer) {
        match self {
            TargetInfo::TypeParameter {
                target_type,
                type_parameter_index,
            } => {
                w.u8(*target_type);
                w.u8(*type_parameter_index);
            }
            TargetInfo::Supertype { supertype_index } => {
                w.u8(0x10);
                w.u16(*supertype_index);
            }
            TargetInfo::TypeParameterBound {
                target_type,
                type_parameter_index,
                bound_index,
            } => {
                w.u8(*target_type);
                w.u8(*type_parameter_index);
                w.u8(*bound_index);
            }
            TargetInfo::Empty { target_type } => w.u8(*target_type),
            TargetInfo::FormalParameter {
                formal_parameter_index,
            } => {
                w.u8(0x16);
                w.u8(*formal_parameter_index);
            }
            TargetInfo::Throws { throws_type_index } => {
                w.u8(0x17);
                w.u16(*throws_type_index);
            }
            TargetInfo::LocalVar { target_type, table } => {
                w.u8(*target_type);
                w.u16(table.len() as u16);
                for e in table {
                    w.u16(e.start_pc);
                    w.u16(e.length);
                    w.u16(e.index);
                }
            }
            TargetInfo::Catch {
                exception_table_index,
            } => {
                w.u8(0x42);
                w.u16(*exception_table_index);
            }
            TargetInfo::Offset {
                target_type,
                offset,
            } => {
                w.u8(*target_type);
                w.u16(*offset);
            }
            TargetInfo::TypeArgument {
                target_type,
                offset,
                type_argument_index,
            } => {
                w.u8(*target_type);
                w.u16(*offset);
                w.u8(*type_argument_index);
            }
        }
    }
}
