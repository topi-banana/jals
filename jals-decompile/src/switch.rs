//! Recovering what a `switch` was written *on*.
//!
//! Every `tableswitch` / `lookupswitch` dispatches on an `int`, but the source rarely did. `javac`
//! lowers a `char` switch by widening, and an `enum` switch by switching on the constant's
//! `ordinal()`. This module reads those lowerings back, so the recovered statement spells the
//! selector and its `case` labels the way they were written (`case 'a':`, `case RED:`) instead of
//! the code units and ordinals the bytecode carries.
//!
//! It is deliberately narrow: a lowering it does not recognise leaves the plain `int` reading, and a
//! lowering it recognises but cannot resolve *confidently* declines the whole method rather than
//! emit labels that name the wrong constants.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};

use jals_classfile::{
    AttributeBody, ClassFile, ConstantPool, ConstantPoolEntry, Instruction, MethodInfo,
};

use crate::expr::Expr;
use crate::hierarchy::ClassHierarchy;
use crate::literal::Literal;

/// The prefix of the synthetic ordinal-mapping array `javac` generates for an `enum` switch in the
/// older, indirection-based lowering (`Outer$1.$SwitchMap$p$Color[c.ordinal()]`).
const SWITCH_MAP_PREFIX: &str = "$SwitchMap$";

/// How a recovered `switch` spells its `case` labels.
pub(crate) enum Labels {
    /// Plain integer constants (`case 3:`) — an `int` / `short` / `byte` selector.
    Int,
    /// Character literals (`case 'a':`) — a `char` selector.
    Char,
    /// Enum constant names (`case RED:`), indexed by ordinal.
    Enum(BTreeMap<i32, String>),
}

impl Labels {
    /// Render one `case` constant, or `None` when the key names nothing in this vocabulary (an
    /// ordinal with no constant, a code unit that is not a valid `char` literal).
    pub(crate) fn render(&self, key: i32) -> Option<String> {
        match self {
            Self::Int => Some(key.to_string()),
            Self::Char => Literal::char_code_unit(i64::from(key)),
            Self::Enum(constants) => constants.get(&key).cloned(),
        }
    }
}

/// Namespace for reading a lowered `switch` selector back to its source form.
pub(crate) struct Subject;

impl Subject {
    /// Read the selector `expr` — the `int` the switch actually dispatches on, whose last
    /// contributing instruction is `last` — back to the expression and label vocabulary the source
    /// switched on.
    ///
    /// Returns `None` to decline the method: either the lowering is recognisable but cannot be
    /// resolved, or it is one whose plain `int` reading would be actively misleading.
    pub(crate) fn recover(
        expr: Expr,
        is_char: bool,
        last: Option<&Instruction>,
        pool: &ConstantPool,
        hierarchy: &ClassHierarchy<'_>,
    ) -> Option<(Expr, Labels)> {
        // The `$SwitchMap$` lowering reads an ordinal through a synthetic array held by a synthetic
        // class. That class is never rendered into a skeleton, so the plain reading would reference
        // a type that does not exist there — decline instead.
        if Self::indexes_a_switch_map(&expr) {
            return None;
        }
        // `enum`: the selector is `<receiver>.ordinal()`, and the keys are ordinals.
        if let Some(owner) = last.and_then(|ins| Self::ordinal_call_owner(ins, pool)) {
            let Expr::Call { recv, .. } = expr else {
                return None;
            };
            let constants = Self::enum_constants(&owner, hierarchy)?;
            return Some((*recv?, Labels::Enum(constants)));
        }
        Some((expr, if is_char { Labels::Char } else { Labels::Int }))
    }

    /// Whether the selector reads an element of a `$SwitchMap$…` array.
    fn indexes_a_switch_map(expr: &Expr) -> bool {
        let Expr::Index { array, .. } = expr else {
            return false;
        };
        matches!(array.as_ref(), Expr::Field { name, .. } if name.starts_with(SWITCH_MAP_PREFIX))
    }

    /// The owner of `ins` when it is an `invokevirtual …ordinal()I`.
    fn ordinal_call_owner(ins: &Instruction, pool: &ConstantPool) -> Option<String> {
        let Instruction::InvokeVirtual(index) = ins else {
            return None;
        };
        let ConstantPoolEntry::MethodRef {
            class_index,
            name_and_type_index,
        } = pool.get(*index)?
        else {
            return None;
        };
        let ConstantPoolEntry::NameAndType {
            name_index,
            descriptor_index,
        } = pool.get(*name_and_type_index)?
        else {
            return None;
        };
        if pool.utf8(*name_index).as_deref() != Some("ordinal")
            || pool.utf8(*descriptor_index).as_deref() != Some("()I")
        {
            return None;
        }
        Some(pool.class_name(*class_index)?.into_owned())
    }

    /// The `ordinal -> constant name` map of the enum class `owner`, read out of its `<clinit>`.
    ///
    /// `javac` initializes each constant as `new E("NAME", ordinal, …)` followed by
    /// `putstatic E.NAME`, so the ordinal is the `int` pushed directly after the name. Requiring the
    /// pushed name to equal the field it is stored into is what makes this authoritative rather than
    /// an assumption about emission order: a class whose `<clinit>` does not match that shape
    /// exactly yields `None`, and the method falls back.
    fn enum_constants(
        owner: &str,
        hierarchy: &ClassHierarchy<'_>,
    ) -> Option<BTreeMap<i32, String>> {
        let cf = hierarchy.class(owner)?;
        if !cf.access_flags.is_enum() {
            return None;
        }
        let code = Self::clinit_code(cf)?;
        let descriptor = format!("L{owner};");
        let pool = &cf.constant_pool;

        let mut constants = BTreeMap::new();
        // The name pushed by the immediately preceding `ldc <String>`, still awaiting its ordinal.
        let mut named: Option<String> = None;
        // A complete `(name, ordinal)`, awaiting the `putstatic` that stores the constant.
        let mut pending: Option<(String, i32)> = None;
        for ins in code {
            if let Some(name) = Self::string_constant(ins, pool) {
                named = Some(name);
                pending = None;
                continue;
            }
            if let Some(name) = named.take() {
                // A name not followed by an ordinal is not a constant initializer.
                pending = Self::int_constant(ins).map(|ordinal| (name, ordinal));
                continue;
            }
            let Instruction::PutStatic(index) = ins else {
                continue;
            };
            let Some((field, field_descriptor)) = Self::field_ref(*index, owner, pool) else {
                continue;
            };
            // `$VALUES` and any other static of a different type are not constants.
            if field_descriptor != descriptor {
                continue;
            }
            let (name, ordinal) = pending.take()?;
            if name != field || constants.insert(ordinal, name).is_some() {
                return None;
            }
        }
        (!constants.is_empty()).then_some(constants)
    }

    /// The `Code` of the class initializer.
    fn clinit_code(cf: &ClassFile) -> Option<&[Instruction]> {
        let clinit: &MethodInfo = cf
            .methods
            .iter()
            .find(|m| cf.constant_pool.utf8(m.name_index).as_deref() == Some("<clinit>"))?;
        clinit.attributes.iter().find_map(|a| match &a.body {
            AttributeBody::Code(code) => Some(code.code.as_slice()),
            _ => None,
        })
    }

    /// The `(name, descriptor)` of a `Fieldref` whose owner is `owner`.
    fn field_ref(index: u16, owner: &str, pool: &ConstantPool) -> Option<(String, String)> {
        let ConstantPoolEntry::FieldRef {
            class_index,
            name_and_type_index,
        } = pool.get(index)?
        else {
            return None;
        };
        if pool.class_name(*class_index).as_deref() != Some(owner) {
            return None;
        }
        let ConstantPoolEntry::NameAndType {
            name_index,
            descriptor_index,
        } = pool.get(*name_and_type_index)?
        else {
            return None;
        };
        Some((
            pool.utf8(*name_index)?.into_owned(),
            pool.utf8(*descriptor_index)?.into_owned(),
        ))
    }

    /// The `String` an `ldc` / `ldc_w` pushes.
    fn string_constant(ins: &Instruction, pool: &ConstantPool) -> Option<String> {
        let index = match ins {
            Instruction::Ldc(i) => u16::from(*i),
            Instruction::LdcW(i) => *i,
            _ => return None,
        };
        let ConstantPoolEntry::String { string_index } = pool.get(index)? else {
            return None;
        };
        Some(pool.utf8(*string_index)?.into_owned())
    }

    /// The value an `int`-pushing constant instruction pushes.
    fn int_constant(ins: &Instruction) -> Option<i32> {
        match ins {
            Instruction::IconstM1 => Some(-1),
            Instruction::Iconst0 => Some(0),
            Instruction::Iconst1 => Some(1),
            Instruction::Iconst2 => Some(2),
            Instruction::Iconst3 => Some(3),
            Instruction::Iconst4 => Some(4),
            Instruction::Iconst5 => Some(5),
            Instruction::Bipush(v) => Some(i32::from(*v)),
            Instruction::Sipush(v) => Some(i32::from(*v)),
            _ => None,
        }
    }
}
