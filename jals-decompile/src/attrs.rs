//! Reading the class-file attributes a signature skeleton needs but bytecode analysis does not:
//! a field's `ConstantValue` initializer, a method's declared checked exceptions (`Exceptions`), and
//! its real parameter names (`MethodParameters` / `LocalVariableTable`). Every function is total and
//! conservative — it returns `None`/empty when it cannot produce something a Java parser accepts, so
//! the caller falls back to a safe form (no initializer, `argN` names) and the output stays valid.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use jals_classfile::{
    Attribute, AttributeBody, BaseType, CodeAttribute, ConstantPool, ConstantPoolEntry, FieldInfo,
    FieldType, LocalVariableEntry, MethodDescriptor, MethodInfo,
};

use crate::literal::Literal;
use crate::types::JavaType;

/// Namespace for the class-file attribute readers a signature skeleton needs but bytecode analysis
/// does not.
pub struct Attrs;

impl Attrs {
    /// A field's `ConstantValue` rendered as a Java initializer expression (the text after `=`), or
    /// `None` if the field has no constant value or it cannot be rendered.
    ///
    /// A boolean field's `0`/`1` becomes `false`/`true`; `long`/`float`/`double` get their type
    /// suffix; a `String` is escaped.
    pub fn constant_value_initializer(field: &FieldInfo, pool: &ConstantPool) -> Option<String> {
        let index = field.attributes.iter().find_map(|a| match &a.body {
            AttributeBody::ConstantValue {
                constantvalue_index,
            } => Some(*constantvalue_index),
            _ => None,
        })?;
        let is_boolean = pool
            .utf8(field.descriptor_index)
            .and_then(|d| FieldType::parse(&d).ok())
            .is_some_and(|ft| matches!(ft, FieldType::Base(BaseType::Boolean)));
        Some(match pool.get(index)? {
            ConstantPoolEntry::Integer(v) => {
                if is_boolean {
                    if *v != 0 { "true" } else { "false" }.to_owned()
                } else {
                    v.to_string()
                }
            }
            ConstantPoolEntry::Long(v) => format!("{v}L"),
            ConstantPoolEntry::Float(v) => Literal::float_literal(*v),
            ConstantPoolEntry::Double(v) => Literal::double_literal(*v),
            ConstantPoolEntry::String { string_index } => {
                Literal::string_literal(&pool.utf8(*string_index)?)
            }
            _ => return None,
        })
    }

    /// The `Signature` attribute's generic-signature string, if present — the raw JVMS §4.7.9 text a
    /// caller parses into a class/field/method signature.
    pub fn signature_string(attrs: &[Attribute], pool: &ConstantPool) -> Option<String> {
        attrs.iter().find_map(|a| match &a.body {
            AttributeBody::Signature { signature_index } => pool
                .utf8(*signature_index)
                .map(alloc::borrow::Cow::into_owned),
            _ => None,
        })
    }

    /// The checked exceptions a method declares via its `Exceptions` attribute, in Java dotted form.
    ///
    /// The non-generic counterpart to the `throws` clause carried by a generic `Signature`; empty
    /// when the method declares none.
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
            .map(|name| JavaType::internal_to_java(&name))
            .collect()
    }

    /// A method's real source parameter names, in order, or `None` if they cannot be recovered
    /// confidently (no debug info, a count mismatch, or a name that is not a valid identifier).
    ///
    /// `arity` is the number of source parameters the caller renders; the result, when `Some`, has
    /// that length.
    pub fn parameter_names(
        method: &MethodInfo,
        pool: &ConstantPool,
        is_static: bool,
        arity: usize,
    ) -> Option<Vec<String>> {
        if arity == 0 {
            return Some(Vec::new());
        }
        Self::params_from_method_parameters(method, pool, arity)
            .or_else(|| Self::params_from_local_variable_table(method, pool, is_static, arity))
    }

    /// Names from the `MethodParameters` attribute (`-parameters`): one entry per parameter, in
    /// order.
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
            if !Self::is_java_identifier(&name) {
                return None;
            }
            names.push(name);
        }
        Some(names)
    }

    /// Names from the `Code` attribute's `LocalVariableTable` (`-g`): parameters occupy the first
    /// local slots (slot 0 is `this` for an instance method; a `long`/`double` takes two slots).
    fn params_from_local_variable_table(
        method: &MethodInfo,
        pool: &ConstantPool,
        is_static: bool,
        arity: usize,
    ) -> Option<Vec<String>> {
        let descriptor = pool.utf8(method.descriptor_index)?;
        let params = MethodDescriptor::parse(&descriptor).ok()?.params;
        // The slot-based mapping is only unambiguous when the descriptor's arity is exactly what the
        // caller renders (a generic signature can expose a different formal-parameter count).
        if params.len() != arity {
            return None;
        }
        let code = method.attributes.iter().find_map(|a| match &a.body {
            AttributeBody::Code(code) => Some(code),
            _ => None,
        })?;
        let table = Self::local_variable_table(code)?;
        let mut names = Vec::with_capacity(arity);
        for (slot, _param) in Self::parameter_slots(&params, is_static) {
            let name = table
                .iter()
                .find(|e| e.index == slot && e.start_pc == 0)
                .and_then(|e| pool.utf8(e.name_index))
                .map(alloc::borrow::Cow::into_owned)?;
            if !Self::is_java_identifier(&name) {
                return None;
            }
            names.push(name);
        }
        Some(names)
    }

    /// Enumerate each parameter with the local-variable slot it occupies. Slot 0 is `this` for an
    /// instance method (so the first parameter starts at slot 1); a `long`/`double` parameter takes
    /// two slots, everything else one. The single source of truth for the parameter → slot mapping,
    /// shared by parameter-name recovery here and the body decompiler's local map ([`crate::body`]).
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

    /// The method `Code`'s `LocalVariableTable` (present when compiled with `-g`), or `None` — the
    /// source of names/types for both parameter-name and hoisted-local recovery.
    pub(crate) fn local_variable_table(code: &CodeAttribute) -> Option<&[LocalVariableEntry]> {
        code.attributes.iter().find_map(|a| match &a.body {
            AttributeBody::LocalVariableTable(table) => Some(table.as_slice()),
            _ => None,
        })
    }

    /// Resolve a non-parameter local `slot` to its `(name, rendered-Java-type)` from a method's
    /// `LocalVariableTable`, used to hoist a typed declaration for every local a method stores into.
    /// Returns `None` — bailing the whole method — when the slot cannot be resolved unambiguously:
    /// - it has no entry (a synthetic temporary, or the class was compiled without `-g`),
    /// - a name is not a valid Java identifier,
    /// - a descriptor does not parse, or
    /// - the slot is reused for two variables with a differing name/type across disjoint live ranges
    ///   (M3 does not split a reused slot — a later milestone keys locals by `(slot, pc)`).
    ///
    /// `javac` emits several entries for one source variable (one per live sub-range across
    /// branches); they agree on name + type, so collecting the *distinct* `(name, type)` yields
    /// exactly one.
    pub(crate) fn local_variable(
        table: &[LocalVariableEntry],
        pool: &ConstantPool,
        slot: u16,
    ) -> Option<(String, String)> {
        let mut resolved: Option<(String, String)> = None;
        for entry in table.iter().filter(|e| e.index == slot) {
            let name = pool.utf8(entry.name_index)?.into_owned();
            if !Self::is_java_identifier(&name) {
                return None;
            }
            let descriptor = pool.utf8(entry.descriptor_index)?;
            let ty = JavaType::render_field_type(&FieldType::parse(&descriptor).ok()?);
            let pair = (name, ty);
            match &resolved {
                Some(prev) if *prev != pair => return None,
                _ => resolved = Some(pair),
            }
        }
        resolved
    }

    /// A conservative Java-identifier check, so a recovered name can never break the parse.
    pub(crate) fn is_java_identifier(s: &str) -> bool {
        let mut chars = s.chars();
        match chars.next() {
            Some(c) if c == '_' || c == '$' || c.is_alphabetic() => {}
            _ => return false,
        }
        chars.all(|c| c == '_' || c == '$' || c.is_alphanumeric())
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use jals_classfile::{ClassFile, MethodParameterEntry};

    use super::*;

    fn fixture(bytes: &[u8]) -> ClassFile {
        ClassFile::read(bytes).expect("parse fixture")
    }

    fn consts() -> ClassFile {
        fixture(include_bytes!(
            "../../jals-classpath/tests/fixtures/Consts.class"
        ))
    }

    fn field<'a>(cf: &'a ClassFile, name: &str) -> &'a FieldInfo {
        cf.fields
            .iter()
            .find(|f| cf.constant_pool.utf8(f.name_index).as_deref() == Some(name))
            .expect("field present")
    }

    fn method<'a>(cf: &'a ClassFile, name: &str) -> &'a MethodInfo {
        cf.methods
            .iter()
            .find(|m| cf.constant_pool.utf8(m.name_index).as_deref() == Some(name))
            .expect("method present")
    }

    fn utf8_index(pool: &ConstantPool, text: &str) -> u16 {
        (1..1024)
            .find(|&i| pool.utf8(i).as_deref() == Some(text))
            .expect("utf8 entry present")
    }

    #[test]
    fn identifier_check_rejects_non_identifiers() {
        assert!(Attrs::is_java_identifier("name"));
        assert!(Attrs::is_java_identifier("$1"));
        assert!(Attrs::is_java_identifier("_x"));
        assert!(!Attrs::is_java_identifier(""));
        assert!(!Attrs::is_java_identifier("1x"));
        assert!(!Attrs::is_java_identifier("a-b"));
        assert!(Attrs::is_java_identifier("a_$9"));
        assert!(Attrs::is_java_identifier("名2"));
    }

    #[test]
    fn renders_every_constant_value_kind_and_absence() {
        let cf = consts();
        for (name, expected) in [
            ("MAX", "42"),
            ("BIG", "9000000000L"),
            ("RATE", "1.5d"),
            ("RATIO", "0.25f"),
            ("ENABLED", "true"),
            ("NAME", "\"jals\""),
        ] {
            assert_eq!(
                Attrs::constant_value_initializer(field(&cf, name), &cf.constant_pool).as_deref(),
                Some(expected),
                "{name}"
            );
        }

        let mut no_value = field(&cf, "MAX").clone();
        no_value.attributes.clear();
        assert_eq!(
            Attrs::constant_value_initializer(&no_value, &cf.constant_pool),
            None
        );

        let mut wrong_kind = field(&cf, "MAX").clone();
        let AttributeBody::ConstantValue {
            constantvalue_index,
        } = &mut wrong_kind.attributes[0].body
        else {
            panic!("constant value attribute")
        };
        *constantvalue_index = wrong_kind.descriptor_index;
        assert_eq!(
            Attrs::constant_value_initializer(&wrong_kind, &cf.constant_pool),
            None
        );
    }

    #[test]
    fn reads_signature_and_declared_exceptions() {
        let box_cf = fixture(include_bytes!(
            "../../jals-classpath/tests/fixtures/Box.class"
        ));
        assert_eq!(
            Attrs::signature_string(&box_cf.attributes, &box_cf.constant_pool).as_deref(),
            Some("<T:Ljava/lang/Object;>Ljava/lang/Object;")
        );
        assert_eq!(Attrs::signature_string(&[], &box_cf.constant_pool), None);

        let cf = consts();
        assert_eq!(
            Attrs::declared_throws(method(&cf, "risky"), &cf.constant_pool),
            ["java.io.IOException"]
        );
        assert!(Attrs::declared_throws(method(&cf, "add"), &cf.constant_pool).is_empty());
    }

    #[test]
    fn recovers_parameter_names_from_both_attributes() {
        let cf = consts();
        let add = method(&cf, "add");
        assert_eq!(
            Attrs::parameter_names(add, &cf.constant_pool, false, 1),
            Some(vec!["delta".to_owned()])
        );
        assert_eq!(
            Attrs::params_from_method_parameters(add, &cf.constant_pool, 1),
            Some(vec!["delta".to_owned()])
        );
        assert_eq!(
            Attrs::parameter_names(method(&cf, "reset"), &cf.constant_pool, false, 0),
            Some(Vec::new())
        );

        let mut lvt_only = add.clone();
        lvt_only
            .attributes
            .retain(|a| !matches!(a.body, AttributeBody::MethodParameters(_)));
        assert_eq!(
            Attrs::parameter_names(&lvt_only, &cf.constant_pool, false, 1),
            Some(vec!["delta".to_owned()])
        );
        assert_eq!(
            Attrs::parameter_names(&lvt_only, &cf.constant_pool, false, 2),
            None
        );
    }

    #[test]
    fn rejects_incomplete_or_invalid_method_parameters() {
        let cf = consts();
        let mut method = method(&cf, "add").clone();
        let parameter_attr = |method: &mut MethodInfo, body| {
            method
                .attributes
                .iter_mut()
                .find(|a| matches!(a.body, AttributeBody::MethodParameters(_)))
                .expect("method parameters")
                .body = body;
        };

        parameter_attr(&mut method, AttributeBody::MethodParameters(Vec::new()));
        assert_eq!(
            Attrs::params_from_method_parameters(&method, &cf.constant_pool, 1),
            None
        );
        parameter_attr(
            &mut method,
            AttributeBody::MethodParameters(vec![MethodParameterEntry {
                name_index: 0,
                access_flags: 0,
            }]),
        );
        assert_eq!(
            Attrs::params_from_method_parameters(&method, &cf.constant_pool, 1),
            None
        );
        let descriptor_index = method.descriptor_index;
        parameter_attr(
            &mut method,
            AttributeBody::MethodParameters(vec![MethodParameterEntry {
                name_index: descriptor_index,
                access_flags: 0,
            }]),
        );
        assert_eq!(
            Attrs::params_from_method_parameters(&method, &cf.constant_pool, 1),
            None
        );
    }

    #[test]
    fn local_variable_table_requires_the_parameter_entry_at_method_start() {
        let cf = consts();
        let mut method = method(&cf, "add").clone();
        method
            .attributes
            .retain(|a| !matches!(a.body, AttributeBody::MethodParameters(_)));
        let edit_parameter = |method: &mut MethodInfo, edit: fn(&mut LocalVariableEntry)| {
            let table = method
                .attributes
                .iter_mut()
                .find_map(|a| match &mut a.body {
                    AttributeBody::Code(code) => Some(code),
                    _ => None,
                })
                .expect("code")
                .attributes
                .iter_mut()
                .find_map(|a| match &mut a.body {
                    AttributeBody::LocalVariableTable(table) => Some(table),
                    _ => None,
                })
                .expect("local variable table");
            edit(
                table
                    .iter_mut()
                    .find(|entry| entry.index == 1)
                    .expect("delta entry"),
            );
        };
        edit_parameter(&mut method, |parameter| parameter.start_pc = 1);
        assert_eq!(
            Attrs::params_from_local_variable_table(&method, &cf.constant_pool, false, 1),
            None
        );
        edit_parameter(&mut method, |parameter| {
            parameter.start_pc = 0;
            parameter.index = 2;
        });
        assert_eq!(
            Attrs::params_from_local_variable_table(&method, &cf.constant_pool, false, 1),
            None
        );
    }

    #[test]
    fn local_variable_resolution_rejects_slot_reuse() {
        let cf = consts();
        let code = method(&cf, "add")
            .attributes
            .iter()
            .find_map(|a| match &a.body {
                AttributeBody::Code(code) => Some(code),
                _ => None,
            })
            .expect("code");
        let table = Attrs::local_variable_table(code).expect("table");
        assert_eq!(
            Attrs::local_variable(table, &cf.constant_pool, 1),
            Some(("delta".to_owned(), "int".to_owned()))
        );

        let mut reused = table.to_vec();
        let mut second = reused
            .iter()
            .find(|entry| entry.index == 1)
            .expect("delta")
            .clone();
        second.name_index = utf8_index(&cf.constant_pool, "count");
        reused.push(second);
        assert_eq!(Attrs::local_variable(&reused, &cf.constant_pool, 1), None);
        assert_eq!(Attrs::local_variable(&reused, &cf.constant_pool, 99), None);
    }

    #[test]
    fn parameter_slots_account_for_receiver_and_wide_values() {
        let params = [
            FieldType::Base(BaseType::Int),
            FieldType::Base(BaseType::Long),
            FieldType::Base(BaseType::Double),
            FieldType::Object("java/lang/String".to_owned()),
        ];
        assert_eq!(
            Attrs::parameter_slots(&params, false)
                .map(|(slot, _)| slot)
                .collect::<Vec<_>>(),
            [1, 2, 4, 6]
        );
        assert_eq!(
            Attrs::parameter_slots(&params, true)
                .map(|(slot, _)| slot)
                .collect::<Vec<_>>(),
            [0, 1, 3, 5]
        );
    }
}
