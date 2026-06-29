//! Lowering a parsed `.class` file ([`jals_classfile::ClassFile`]) to the [`ProjectIndex`] facts it
//! contributes: one [`Item`](crate::Item)-worth of type info plus its [`Member`](crate::Member)s.
//!
//! This module is the *pure* half of the classpath bridge — it produces self-contained data
//! ([`ClassfileClass`]), and [`ProjectIndex::build_with_classpath`](crate::ProjectIndex) folds it in
//! exactly like a source file (register types, then resolve members and supertypes by name). Generic
//! signatures (JVMS §4.7.9) are mapped through the same [`MemberType`] / [`TypeParamDecl`] shapes the
//! source path produces, so member access and generic substitution work unchanged: a type variable is
//! left as a bare name for [`is_type_param`](crate::ProjectIndex) to recognise, and every class name
//! is emitted fully-qualified so it resolves without an import context.

use jals_classfile::{
    Attribute, AttributeBody, ClassFile, ClassSignature, ClassTypeSignature, ConstantPool,
    FieldType, ResultSignature, ReturnType, TypeArgument, TypeParameter, TypeSignature,
    parse_class_signature, parse_field_descriptor, parse_field_signature, parse_method_descriptor,
    parse_method_signature,
};

use crate::def::DefKind;
use crate::project::{MemberType, Param, TypeParamDecl};

/// A `.class` file reduced to the type-level facts the index needs.
pub(crate) struct ClassfileClass {
    /// The class's fully-qualified, dotted name (`java.util.Map.Entry`).
    pub fqn: String,
    /// Which kind of type it is.
    pub kind: DefKind,
    /// Its declared type parameters (from the class `Signature`), empty for a raw class.
    pub type_params: Vec<TypeParamDecl>,
    /// Its supertypes (superclass then superinterfaces), captured like a source `extends`/`implements`
    /// clause and resolved by FQN later.
    pub supertypes: Vec<MemberType>,
    /// Its declared fields, methods, and constructors.
    pub members: Vec<ClassfileMember>,
}

/// One member (field / method / constructor) of a [`ClassfileClass`].
pub(crate) struct ClassfileMember {
    /// The member's simple name (a constructor uses the class's simple name, matching the source path).
    pub name: String,
    /// What kind of member it is.
    pub kind: DefKind,
    /// The field type or method return type (a constructor has none — [`MemberType::Unknown`]).
    pub ty: MemberType,
    /// The method's parameters (empty for a field).
    pub params: Vec<Param>,
    /// Whether the method is varargs.
    pub varargs: bool,
}

/// Lower a class file to its [`ClassfileClass`], or `None` for `module-info` (a module, not a type).
pub(crate) fn lower(cf: &ClassFile) -> Option<ClassfileClass> {
    if cf.access_flags.is_module() {
        return None;
    }
    let pool = &cf.constant_pool;
    let fqn = internal_to_fqn(&pool.class_name(cf.this_class)?);
    let class_sig = class_signature(cf, pool);
    let type_params = class_sig
        .as_ref()
        .map(|s| lower_type_params(&s.type_parameters))
        .unwrap_or_default();
    let supertypes = lower_supertypes(cf, class_sig.as_ref(), pool);
    let members = lower_members(cf, pool, simple_name(&fqn));
    Some(ClassfileClass {
        fqn,
        kind: class_kind(cf),
        type_params,
        supertypes,
        members,
    })
}

/// Convert a JVM internal binary name (`a/b/Outer$Inner`) to the dotted source form (`a.b.Outer.Inner`)
/// the index keys types by.
fn internal_to_fqn(internal: &str) -> String {
    internal.replace(['/', '$'], ".")
}

/// The last dotted segment of a name.
fn simple_name(fqn: &str) -> &str {
    fqn.rsplit('.').next().unwrap_or(fqn)
}

fn class_kind(cf: &ClassFile) -> DefKind {
    let flags = cf.access_flags;
    if flags.is_annotation() {
        DefKind::AnnotationType
    } else if flags.is_interface() {
        DefKind::Interface
    } else if flags.is_enum() {
        DefKind::Enum
    } else if cf
        .attributes
        .iter()
        .any(|a| matches!(a.body, AttributeBody::Record(_)))
    {
        DefKind::Record
    } else {
        DefKind::Class
    }
}

fn class_signature(cf: &ClassFile, pool: &ConstantPool) -> Option<ClassSignature> {
    parse_class_signature(&signature_string(&cf.attributes, pool)?).ok()
}

/// The `Signature` attribute's string, if present.
fn signature_string(attrs: &[Attribute], pool: &ConstantPool) -> Option<String> {
    attrs.iter().find_map(|a| match &a.body {
        AttributeBody::Signature { signature_index } => {
            pool.utf8(*signature_index).map(|c| c.into_owned())
        }
        _ => None,
    })
}

fn lower_type_params(params: &[TypeParameter]) -> Vec<TypeParamDecl> {
    params
        .iter()
        .map(|tp| TypeParamDecl {
            name: tp.name.clone(),
            // Like the source path, an implicit `Object` class bound contributes no listed bound.
            bounds: tp
                .class_bound
                .iter()
                .filter(|t| !is_object(t))
                .chain(tp.interface_bounds.iter())
                .map(type_sig_to_member_type)
                .collect(),
        })
        .collect()
}

fn lower_supertypes(
    cf: &ClassFile,
    class_sig: Option<&ClassSignature>,
    pool: &ConstantPool,
) -> Vec<MemberType> {
    if let Some(sig) = class_sig {
        let mut out = vec![class_type_sig_to_member_type(&sig.superclass, 0)];
        out.extend(
            sig.superinterfaces
                .iter()
                .map(|i| class_type_sig_to_member_type(i, 0)),
        );
        return out;
    }
    let mut out = Vec::new();
    if cf.super_class != 0
        && let Some(internal) = pool.class_name(cf.super_class)
    {
        out.push(named_from_internal(&internal));
    }
    for &iface in &cf.interfaces {
        if let Some(internal) = pool.class_name(iface) {
            out.push(named_from_internal(&internal));
        }
    }
    out
}

fn lower_members(cf: &ClassFile, pool: &ConstantPool, owner_simple: &str) -> Vec<ClassfileMember> {
    let mut out = Vec::new();
    for field in &cf.fields {
        let Some(name) = pool.utf8(field.name_index).map(|c| c.into_owned()) else {
            continue;
        };
        out.push(ClassfileMember {
            name,
            kind: DefKind::Field,
            ty: field_member_type(&field.attributes, field.descriptor_index, pool),
            params: Vec::new(),
            varargs: false,
        });
    }
    for method in &cf.methods {
        let Some(raw_name) = pool.utf8(method.name_index).map(|c| c.into_owned()) else {
            continue;
        };
        if raw_name == "<clinit>" {
            continue;
        }
        let (ret, params, varargs) = method_shape(method, pool);
        let (name, kind, ty) = if raw_name == "<init>" {
            // A constructor's source name is the class's simple name (matches `members_of_decl`).
            (
                owner_simple.to_owned(),
                DefKind::Constructor,
                MemberType::Unknown,
            )
        } else {
            (raw_name, DefKind::Method, ret)
        };
        out.push(ClassfileMember {
            name,
            kind,
            ty,
            params,
            varargs,
        });
    }
    out
}

/// A field's type: from its `Signature` (generic) if present, else its descriptor.
fn field_member_type(
    attrs: &[Attribute],
    descriptor_index: u16,
    pool: &ConstantPool,
) -> MemberType {
    if let Some(sig) = signature_string(attrs, pool)
        && let Ok(ts) = parse_field_signature(&sig)
    {
        return type_sig_to_member_type(&ts);
    }
    if let Some(desc) = pool.utf8(descriptor_index)
        && let Ok(ft) = parse_field_descriptor(&desc)
    {
        return field_type_to_member_type(&ft);
    }
    MemberType::Unknown
}

/// A method's (return type, parameters, varargs): from its `Signature` (generic) if present, else its
/// descriptor.
fn method_shape(
    method: &jals_classfile::MethodInfo,
    pool: &ConstantPool,
) -> (MemberType, Vec<Param>, bool) {
    let varargs = method.access_flags.is_varargs();
    if let Some(sig) = signature_string(&method.attributes, pool)
        && let Ok(ms) = parse_method_signature(&sig)
    {
        let params = ms
            .parameters
            .iter()
            .map(|p| Param {
                name: None,
                ty: type_sig_to_member_type(p),
            })
            .collect();
        let ret = match &ms.result {
            ResultSignature::Void => MemberType::Void,
            ResultSignature::Type(t) => type_sig_to_member_type(t),
        };
        return (ret, params, varargs);
    }
    if let Some(desc) = pool.utf8(method.descriptor_index)
        && let Ok(md) = parse_method_descriptor(&desc)
    {
        let params = md
            .params
            .iter()
            .map(|p| Param {
                name: None,
                ty: field_type_to_member_type(p),
            })
            .collect();
        let ret = match &md.return_type {
            ReturnType::Void => MemberType::Void,
            ReturnType::Type(ft) => field_type_to_member_type(ft),
        };
        return (ret, params, varargs);
    }
    (MemberType::Unknown, Vec::new(), varargs)
}

// --- descriptor / signature → MemberType ---------------------------------------------------------

fn field_type_to_member_type(ft: &FieldType) -> MemberType {
    let (base, dims) = peel_field_array(ft, 0);
    match base {
        FieldType::Base(b) => MemberType::Primitive {
            keyword: b.keyword().to_owned(),
            dims,
        },
        FieldType::Object(internal) => named(&internal_to_fqn(internal), dims, Vec::new()),
        FieldType::Array(_) => unreachable!("peeled"),
    }
}

fn peel_field_array(ft: &FieldType, dims: u32) -> (&FieldType, u32) {
    match ft {
        FieldType::Array(inner) => peel_field_array(inner, dims + 1),
        other => (other, dims),
    }
}

fn type_sig_to_member_type(ts: &TypeSignature) -> MemberType {
    let (base, dims) = peel_sig_array(ts, 0);
    match base {
        TypeSignature::Base(b) => MemberType::Primitive {
            keyword: b.keyword().to_owned(),
            dims,
        },
        // A bare type variable: left unqualified so `is_type_param` turns it into a `Ty::TypeVar`.
        TypeSignature::TypeVariable(name) => MemberType::Named {
            name: name.clone(),
            qualified: None,
            dims,
            args: Vec::new(),
        },
        TypeSignature::Class(c) => class_type_sig_to_member_type(c, dims),
        TypeSignature::Array(_) => unreachable!("peeled"),
    }
}

fn peel_sig_array(ts: &TypeSignature, dims: u32) -> (&TypeSignature, u32) {
    match ts {
        TypeSignature::Array(inner) => peel_sig_array(inner, dims + 1),
        other => (other, dims),
    }
}

fn class_type_sig_to_member_type(c: &ClassTypeSignature, dims: u32) -> MemberType {
    // Fold the inner-class suffixes into one dotted name; the innermost component carries the args.
    let mut fqn = internal_to_fqn(&c.name);
    let mut args = &c.type_arguments;
    for suffix in &c.suffixes {
        fqn.push('.');
        fqn.push_str(&suffix.name);
        args = &suffix.type_arguments;
    }
    named(
        &fqn,
        dims,
        args.iter().map(type_arg_to_member_type).collect(),
    )
}

fn type_arg_to_member_type(arg: &TypeArgument) -> MemberType {
    match arg {
        TypeArgument::Exact(t) => type_sig_to_member_type(t),
        // Wildcards are not modelled: kept as `Unknown` so positions stay aligned and assignment
        // stays lenient (matches the source path's treatment of `?`).
        TypeArgument::Any | TypeArgument::Extends(_) | TypeArgument::Super(_) => {
            MemberType::Unknown
        }
    }
}

fn named_from_internal(internal: &str) -> MemberType {
    named(&internal_to_fqn(internal), 0, Vec::new())
}

/// Build a fully-qualified [`MemberType::Named`] (the `qualified` form so it resolves without imports).
fn named(fqn: &str, dims: u32, args: Vec<MemberType>) -> MemberType {
    MemberType::Named {
        name: simple_name(fqn).to_owned(),
        qualified: Some(fqn.to_owned()),
        dims,
        args,
    }
}

/// Whether a type signature is exactly `java.lang.Object` (a non-restrictive class bound).
fn is_object(ts: &TypeSignature) -> bool {
    matches!(ts, TypeSignature::Class(c)
        if c.name == "java/lang/Object" && c.suffixes.is_empty() && c.type_arguments.is_empty())
}
