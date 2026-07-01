//! Rendering `jals_classfile` descriptor / signature types to Java source text.
//!
//! The shared type vocabulary used by both the signature skeleton renderer (`jals-classpath`) and
//! this crate's body decompiler: a JVM descriptor / generic signature type is turned into a
//! well-formed Java type reference (`[Ljava/lang/String;` → `java.lang.String[]`,
//! `Ljava/util/List<Ljava/lang/String;>;` → `java.util.List<java.lang.String>`). Pure, never panics.

use jals_classfile::{ClassTypeSignature, FieldType, ThrowsSignature, TypeArgument, TypeSignature};

/// Convert a JVM internal binary name (`a/b/Outer$Inner`) to its dotted Java form (`a.b.Outer.Inner`).
pub fn internal_to_java(internal: &str) -> String {
    internal.replace(['/', '$'], ".")
}

/// Render a field-descriptor type to Java source (`[Ljava/lang/String;` → `java.lang.String[]`).
pub fn render_field_type(ft: &FieldType) -> String {
    match ft {
        FieldType::Base(b) => b.keyword().to_string(),
        FieldType::Object(internal) => internal_to_java(internal),
        FieldType::Array(inner) => format!("{}[]", render_field_type(inner)),
    }
}

/// Render a generic type signature to Java source
/// (`Ljava/util/List<Ljava/lang/String;>;` → `java.util.List<java.lang.String>`).
pub fn render_type_sig(ts: &TypeSignature) -> String {
    match ts {
        TypeSignature::Base(b) => b.keyword().to_string(),
        TypeSignature::TypeVariable(name) => name.clone(),
        TypeSignature::Array(inner) => format!("{}[]", render_type_sig(inner)),
        TypeSignature::Class(c) => render_class_type_sig(c),
    }
}

/// Render a class type signature: fold the inner-class suffixes into one dotted name, keeping the
/// innermost type arguments (matching the HIR bridge; a navigation reference needs only a
/// well-formed reference).
pub fn render_class_type_sig(c: &ClassTypeSignature) -> String {
    let mut name = internal_to_java(&c.name);
    let mut args = &c.type_arguments;
    for suffix in &c.suffixes {
        name.push('.');
        name.push_str(&suffix.name);
        args = &suffix.type_arguments;
    }
    format!("{name}{}", render_type_args(args))
}

/// Render a `<...>` type-argument list, or `""` for none.
pub(crate) fn render_type_args(args: &[TypeArgument]) -> String {
    if args.is_empty() {
        return String::new();
    }
    let rendered: Vec<String> = args.iter().map(render_type_arg).collect();
    format!("<{}>", rendered.join(", "))
}

/// Render one type argument (`?`, `T`, `? extends T`, `? super T`).
pub(crate) fn render_type_arg(arg: &TypeArgument) -> String {
    match arg {
        TypeArgument::Any => "?".to_string(),
        TypeArgument::Exact(t) => render_type_sig(t),
        TypeArgument::Extends(t) => format!("? extends {}", render_type_sig(t)),
        TypeArgument::Super(t) => format!("? super {}", render_type_sig(t)),
    }
}

/// Render a `throws` clause entry (a class type or a type variable).
pub fn render_throws(t: &ThrowsSignature) -> String {
    match t {
        ThrowsSignature::Class(c) => render_class_type_sig(c),
        ThrowsSignature::TypeVariable(name) => name.clone(),
    }
}
