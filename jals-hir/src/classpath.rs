//! Lowering a parsed `.class` file ([`jals_classfile::ClassFile`]) to the [`ProjectIndex`] facts it
//! contributes: one [`Item`](crate::Item)-worth of type info plus its [`Member`](crate::Member)s.
//!
//! This module is the *pure* half of the classpath bridge — it produces self-contained data
//! ([`ClassfileClass`]), and [`ProjectIndexBuilder::with_classpath`](crate::ProjectIndexBuilder) folds it in
//! exactly like a source file (register types, then resolve members and supertypes by name). Generic
//! signatures (JVMS §4.7.9) are mapped through the same [`MemberType`] / [`TypeParamDecl`] shapes the
//! source path produces, so member access and generic substitution work unchanged: a type variable is
//! left as a bare name for [`is_type_param`](crate::ProjectIndex) to recognise, and every class name
//! is emitted fully-qualified so it resolves without an import context.

use alloc::borrow::{Cow, ToOwned};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use jals_classfile::{
    Attribute, AttributeBody, ClassFile, ClassSignature, ClassTypeSignature, ConstantPool,
    FieldType, MethodDescriptor, MethodSignature, ResultSignature, ReturnType, TypeArgument,
    TypeParameter, TypeSignature,
};

use crate::def::DefKind;
use crate::project::{Fqn, MemberType, Param, TypeParamDecl};

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
    /// The checked exceptions the method declares (`throws`), captured like a supertype so they
    /// resolve by fully-qualified name. Empty for a field / constructor / method that declares none.
    pub throws: Vec<MemberType>,
}

/// Namespace for the pure `.class` → [`ClassfileClass`] lowering functions.
pub(crate) struct ClasspathLower;

impl ClasspathLower {
    /// Lower a class file to its [`ClassfileClass`], or `None` for `module-info` (a module, not a type).
    pub(crate) fn lower(cf: &ClassFile) -> Option<ClassfileClass> {
        if cf.access_flags.is_module() {
            return None;
        }
        let pool = &cf.constant_pool;
        let fqn = jals_decompile::JavaType::internal_to_java(&pool.class_name(cf.this_class)?);
        let class_sig = Self::class_signature(cf, pool);
        let type_params = class_sig
            .as_ref()
            .map(|s| Self::lower_type_params(&s.type_parameters))
            .unwrap_or_default();
        let supertypes = Self::lower_supertypes(cf, class_sig.as_ref(), pool);
        let members = Self::lower_members(cf, pool, Fqn::simple_name_of(&fqn));
        Some(ClassfileClass {
            fqn,
            kind: Self::class_kind(cf),
            type_params,
            supertypes,
            members,
        })
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
        ClassSignature::parse(&jals_decompile::Attrs::signature_string(
            &cf.attributes,
            pool,
        )?)
        .ok()
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
                    .filter(|t| !t.is_java_lang_object())
                    .chain(tp.interface_bounds.iter())
                    .map(Self::type_sig_to_member_type)
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
            let mut out = vec![Self::class_type_sig_to_member_type(&sig.superclass, 0)];
            out.extend(
                sig.superinterfaces
                    .iter()
                    .map(|i| Self::class_type_sig_to_member_type(i, 0)),
            );
            return out;
        }
        let mut out = Vec::new();
        if cf.super_class != 0
            && let Some(internal) = pool.class_name(cf.super_class)
        {
            out.push(Self::named_from_internal(&internal));
        }
        for &iface in &cf.interfaces {
            if let Some(internal) = pool.class_name(iface) {
                out.push(Self::named_from_internal(&internal));
            }
        }
        out
    }

    fn lower_members(
        cf: &ClassFile,
        pool: &ConstantPool,
        owner_simple: &str,
    ) -> Vec<ClassfileMember> {
        let mut out = Vec::new();
        for field in &cf.fields {
            let Some(name) = pool.utf8(field.name_index).map(Cow::into_owned) else {
                continue;
            };
            out.push(ClassfileMember {
                name,
                kind: DefKind::Field,
                ty: Self::field_member_type(&field.attributes, field.descriptor_index, pool),
                params: Vec::new(),
                varargs: false,
                throws: Vec::new(),
            });
        }
        for method in &cf.methods {
            let Some(raw_name) = pool.utf8(method.name_index).map(Cow::into_owned) else {
                continue;
            };
            if raw_name == "<clinit>" {
                continue;
            }
            let (ret, params, varargs) = Self::method_shape(method, pool);
            // The declared checked exceptions (`throws`), from the `Exceptions` attribute, as
            // fully-qualified named types so they resolve without an import context.
            let throws = jals_decompile::Attrs::declared_throws(method, pool)
                .iter()
                .map(|fqn| Self::named(fqn, 0, Vec::new()))
                .collect();
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
                throws,
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
        if let Some(sig) = jals_decompile::Attrs::signature_string(attrs, pool)
            && let Ok(ts) = TypeSignature::parse(&sig)
        {
            return Self::type_sig_to_member_type(&ts);
        }
        if let Some(desc) = pool.utf8(descriptor_index)
            && let Ok(ft) = FieldType::parse(&desc)
        {
            return Self::field_type_to_member_type(&ft);
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
        if let Some(sig) = jals_decompile::Attrs::signature_string(&method.attributes, pool)
            && let Ok(ms) = MethodSignature::parse(&sig)
        {
            let params = ms
                .parameters
                .iter()
                .map(|p| Param {
                    name: None,
                    ty: Self::type_sig_to_member_type(p),
                })
                .collect();
            let ret = match &ms.result {
                ResultSignature::Void => MemberType::Void,
                ResultSignature::Type(t) => Self::type_sig_to_member_type(t),
            };
            return (ret, params, varargs);
        }
        if let Some(desc) = pool.utf8(method.descriptor_index)
            && let Ok(md) = MethodDescriptor::parse(&desc)
        {
            let params = md
                .params
                .iter()
                .map(|p| Param {
                    name: None,
                    ty: Self::field_type_to_member_type(p),
                })
                .collect();
            let ret = match &md.return_type {
                ReturnType::Void => MemberType::Void,
                ReturnType::Type(ft) => Self::field_type_to_member_type(ft),
            };
            return (ret, params, varargs);
        }
        (MemberType::Unknown, Vec::new(), varargs)
    }

    // --- descriptor / signature → MemberType -----------------------------------------------------

    fn field_type_to_member_type(ft: &FieldType) -> MemberType {
        let (base, dims) = Self::peel_field_array(ft, 0);
        match base {
            FieldType::Base(b) => MemberType::Primitive {
                keyword: b.keyword().to_owned(),
                dims,
            },
            FieldType::Object(internal) => Self::named(
                &jals_decompile::JavaType::internal_to_java(internal),
                dims,
                Vec::new(),
            ),
            FieldType::Array(_) => unreachable!("peeled"),
        }
    }

    fn peel_field_array(ft: &FieldType, dims: u32) -> (&FieldType, u32) {
        match ft {
            FieldType::Array(inner) => Self::peel_field_array(inner, dims + 1),
            other => (other, dims),
        }
    }

    fn type_sig_to_member_type(ts: &TypeSignature) -> MemberType {
        let (base, dims) = Self::peel_sig_array(ts, 0);
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
            TypeSignature::Class(c) => Self::class_type_sig_to_member_type(c, dims),
            TypeSignature::Array(_) => unreachable!("peeled"),
        }
    }

    fn peel_sig_array(ts: &TypeSignature, dims: u32) -> (&TypeSignature, u32) {
        match ts {
            TypeSignature::Array(inner) => Self::peel_sig_array(inner, dims + 1),
            other => (other, dims),
        }
    }

    fn class_type_sig_to_member_type(c: &ClassTypeSignature, dims: u32) -> MemberType {
        // Fold the inner-class suffixes into one dotted name; the innermost component carries the args.
        let mut fqn = jals_decompile::JavaType::internal_to_java(&c.name);
        let mut args = &c.type_arguments;
        for suffix in &c.suffixes {
            fqn.push('.');
            fqn.push_str(&suffix.name);
            args = &suffix.type_arguments;
        }
        Self::named(
            &fqn,
            dims,
            args.iter().map(Self::type_arg_to_member_type).collect(),
        )
    }

    fn type_arg_to_member_type(arg: &TypeArgument) -> MemberType {
        match arg {
            TypeArgument::Exact(t) => Self::type_sig_to_member_type(t),
            // Wildcards are not modelled: kept as `Unknown` so positions stay aligned and assignment
            // stays lenient (matches the source path's treatment of `?`).
            TypeArgument::Any | TypeArgument::Extends(_) | TypeArgument::Super(_) => {
                MemberType::Unknown
            }
        }
    }

    fn named_from_internal(internal: &str) -> MemberType {
        Self::named(
            &jals_decompile::JavaType::internal_to_java(internal),
            0,
            Vec::new(),
        )
    }

    /// Build a fully-qualified [`MemberType::Named`] (the `qualified` form so it resolves without imports).
    fn named(fqn: &str, dims: u32, args: Vec<MemberType>) -> MemberType {
        MemberType::Named {
            name: Fqn::simple_name_of(fqn).to_owned(),
            qualified: Some(fqn.to_owned()),
            dims,
            args,
        }
    }
}
