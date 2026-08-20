//! Rendering `jals_classfile` descriptor / signature types to Java source text.
//!
//! The shared type vocabulary used by both the signature skeleton renderer (`jals-classpath`) and
//! this crate's body decompiler: a JVM descriptor / generic signature type is turned into a
//! well-formed Java type reference (`[Ljava/lang/String;` → `java.lang.String[]`,
//! `Ljava/util/List<Ljava/lang/String;>;` → `java.util.List<java.lang.String>`). Pure, never panics.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use jals_classfile::{ClassTypeSignature, FieldType, ThrowsSignature, TypeArgument, TypeSignature};

pub(crate) use api::array_base;
pub use api::{
    internal_to_java, render_class_type_sig, render_field_type, render_throws, render_type_sig,
};

/// Namespace for rendering JVM descriptor / signature types to Java source text.
///
/// The shared type vocabulary used by both the signature skeleton renderer (`jals-classpath`) and
/// this crate's body decompiler.
mod api {
    use super::{
        ClassTypeSignature, FieldType, String, ThrowsSignature, ToOwned, TypeArgument,
        TypeSignature, Vec, format,
    };

    /// Convert a JVM internal binary name (`a/b/Outer$Inner`) to its dotted Java form
    /// (`a.b.Outer.Inner`).
    pub fn internal_to_java(internal: &str) -> String {
        internal.replace(['/', '$'], ".")
    }

    /// Render a field-descriptor type to Java source (`[Ljava/lang/String;` → `java.lang.String[]`).
    pub fn render_field_type(ft: &FieldType) -> String {
        match ft {
            FieldType::Base(b) => b.keyword().to_owned(),
            FieldType::Object(internal) => internal_to_java(internal),
            FieldType::Array(inner) => format!("{}[]", render_field_type(inner)),
        }
    }

    /// Peel a field-descriptor type to its base (non-array) element rendered as Java plus the
    /// array dimension count (`[[I` parsed → `("int", 2)`; a non-array type → depth `0`).
    pub(crate) fn array_base(mut ft: &FieldType) -> (String, usize) {
        let mut depth = 0;
        while let FieldType::Array(inner) = ft {
            ft = inner;
            depth += 1;
        }
        (render_field_type(ft), depth)
    }

    /// Render a generic type signature to Java source
    /// (`Ljava/util/List<Ljava/lang/String;>;` → `java.util.List<java.lang.String>`).
    pub fn render_type_sig(ts: &TypeSignature) -> String {
        match ts {
            TypeSignature::Base(b) => b.keyword().to_owned(),
            TypeSignature::TypeVariable(name) => name.clone(),
            TypeSignature::Array(inner) => format!("{}[]", render_type_sig(inner)),
            TypeSignature::Class(c) => render_class_type_sig(c),
        }
    }

    /// Render a class type signature to a Java type reference.
    ///
    /// Fold the inner-class suffixes into one dotted name, keeping the innermost type arguments
    /// (matching the HIR bridge; a navigation reference needs only a well-formed reference).
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
    fn render_type_args(args: &[TypeArgument]) -> String {
        if args.is_empty() {
            return String::new();
        }
        let rendered: Vec<String> = args.iter().map(render_type_arg).collect();
        format!("<{}>", rendered.join(", "))
    }

    /// Render one type argument (`?`, `T`, `? extends T`, `? super T`).
    fn render_type_arg(arg: &TypeArgument) -> String {
        match arg {
            TypeArgument::Any => "?".to_owned(),
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
}
