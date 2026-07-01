//! Reading the class-file attributes a signature skeleton needs but bytecode analysis does not:
//! a field's `ConstantValue` initializer, a method's declared checked exceptions (`Exceptions`), and
//! its real parameter names (`MethodParameters` / `LocalVariableTable`). Every function is total and
//! conservative — it returns `None`/empty when it cannot produce something a Java parser accepts, so
//! the caller falls back to a safe form (no initializer, `argN` names) and the output stays valid.

use jals_classfile::{
    AttributeBody, BaseType, ConstantPool, ConstantPoolEntry, FieldInfo, FieldType, MethodInfo,
    parse_field_descriptor, parse_method_descriptor,
};

use crate::literal::{double_literal, float_literal, string_literal};
use crate::types::internal_to_java;

/// A field's `ConstantValue` rendered as a Java initializer expression (the text after `=`), or
/// `None` if the field has no constant value or it cannot be rendered. A boolean field's `0`/`1`
/// becomes `false`/`true`; `long`/`float`/`double` get their type suffix; a `String` is escaped.
pub fn constant_value_initializer(field: &FieldInfo, pool: &ConstantPool) -> Option<String> {
    let index = field.attributes.iter().find_map(|a| match &a.body {
        AttributeBody::ConstantValue {
            constantvalue_index,
        } => Some(*constantvalue_index),
        _ => None,
    })?;
    let is_boolean = pool
        .utf8(field.descriptor_index)
        .and_then(|d| parse_field_descriptor(&d).ok())
        .is_some_and(|ft| matches!(ft, FieldType::Base(BaseType::Boolean)));
    Some(match pool.get(index)? {
        ConstantPoolEntry::Integer(v) => {
            if is_boolean {
                if *v != 0 { "true" } else { "false" }.to_string()
            } else {
                v.to_string()
            }
        }
        ConstantPoolEntry::Long(v) => format!("{v}L"),
        ConstantPoolEntry::Float(v) => float_literal(*v),
        ConstantPoolEntry::Double(v) => double_literal(*v),
        ConstantPoolEntry::String { string_index } => string_literal(&pool.utf8(*string_index)?),
        _ => return None,
    })
}

/// The checked exceptions a method declares via its `Exceptions` attribute, in Java dotted form. The
/// non-generic counterpart to the `throws` clause carried by a generic `Signature`; empty when the
/// method declares none.
pub fn declared_throws(method: &MethodInfo, pool: &ConstantPool) -> Vec<String> {
    method
        .attributes
        .iter()
        .find_map(|a| match &a.body {
            AttributeBody::Exceptions {
                exception_index_table,
            } => Some(exception_index_table),
            _ => None,
        })
        .into_iter()
        .flatten()
        .filter_map(|&idx| pool.class_name(idx))
        .map(|name| internal_to_java(&name))
        .collect()
}

/// A method's real source parameter names, in order, or `None` if they cannot be recovered
/// confidently (no debug info, a count mismatch, or a name that is not a valid identifier). `arity`
/// is the number of source parameters the caller renders; the result, when `Some`, has that length.
pub fn parameter_names(
    method: &MethodInfo,
    pool: &ConstantPool,
    is_static: bool,
    arity: usize,
) -> Option<Vec<String>> {
    if arity == 0 {
        return Some(Vec::new());
    }
    params_from_method_parameters(method, pool, arity)
        .or_else(|| params_from_local_variable_table(method, pool, is_static, arity))
}

/// Names from the `MethodParameters` attribute (`-parameters`): one entry per parameter, in order.
fn params_from_method_parameters(
    method: &MethodInfo,
    pool: &ConstantPool,
    arity: usize,
) -> Option<Vec<String>> {
    let entries = method.attributes.iter().find_map(|a| match &a.body {
        AttributeBody::MethodParameters(entries) => Some(entries),
        _ => None,
    })?;
    if entries.len() != arity {
        return None;
    }
    let mut names = Vec::with_capacity(arity);
    for entry in entries {
        if entry.name_index == 0 {
            return None;
        }
        let name = pool.utf8(entry.name_index)?.into_owned();
        if !is_java_identifier(&name) {
            return None;
        }
        names.push(name);
    }
    Some(names)
}

/// Names from the `Code` attribute's `LocalVariableTable` (`-g`): parameters occupy the first local
/// slots (slot 0 is `this` for an instance method; a `long`/`double` takes two slots).
fn params_from_local_variable_table(
    method: &MethodInfo,
    pool: &ConstantPool,
    is_static: bool,
    arity: usize,
) -> Option<Vec<String>> {
    let descriptor = pool.utf8(method.descriptor_index)?;
    let params = parse_method_descriptor(&descriptor).ok()?.params;
    // The slot-based mapping is only unambiguous when the descriptor's arity is exactly what the
    // caller renders (a generic signature can expose a different formal-parameter count).
    if params.len() != arity {
        return None;
    }
    let code = method.attributes.iter().find_map(|a| match &a.body {
        AttributeBody::Code(code) => Some(code),
        _ => None,
    })?;
    let table = code.attributes.iter().find_map(|a| match &a.body {
        AttributeBody::LocalVariableTable(table) => Some(table),
        _ => None,
    })?;
    let mut names = Vec::with_capacity(arity);
    for (slot, _param) in parameter_slots(&params, is_static) {
        let name = table
            .iter()
            .find(|e| e.index == slot && e.start_pc == 0)
            .and_then(|e| pool.utf8(e.name_index))
            .map(|c| c.into_owned())?;
        if !is_java_identifier(&name) {
            return None;
        }
        names.push(name);
    }
    Some(names)
}

/// Enumerate each parameter with the local-variable slot it occupies. Slot 0 is `this` for an
/// instance method (so the first parameter starts at slot 1); a `long`/`double` parameter takes two
/// slots, everything else one. The single source of truth for the parameter → slot mapping, shared by
/// parameter-name recovery here and the body decompiler's local map ([`crate::body`]).
pub(crate) fn parameter_slots(
    params: &[FieldType],
    is_static: bool,
) -> impl Iterator<Item = (u16, &FieldType)> {
    let mut slot = u16::from(!is_static);
    params.iter().map(move |param| {
        let at = slot;
        slot += if matches!(param, FieldType::Base(BaseType::Long | BaseType::Double)) {
            2
        } else {
            1
        };
        (at, param)
    })
}

/// A conservative Java-identifier check, so a recovered name can never break the parse.
fn is_java_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c == '$' || c.is_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c == '$' || c.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_check_rejects_non_identifiers() {
        assert!(is_java_identifier("name"));
        assert!(is_java_identifier("$1"));
        assert!(is_java_identifier("_x"));
        assert!(!is_java_identifier(""));
        assert!(!is_java_identifier("1x"));
        assert!(!is_java_identifier("a-b"));
    }
}
