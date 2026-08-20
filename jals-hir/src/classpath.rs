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
    FieldAccessFlags, FieldType, MethodAccessFlags, MethodDescriptor, MethodSignature,
    ResultSignature, ReturnType, TypeArgument, TypeParameter, TypeSignature,
};

use crate::def::DefKind;
use crate::project::{Fqn, MemberModifiers, MemberType, Param, TypeParamDecl};

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
    /// How the member is reached, read straight off its access flags — the one place these are
    /// already explicit, since a class file spells out every modifier the source left implicit.
    pub modifiers: MemberModifiers,
    /// The field type or method return type (a constructor has none — [`MemberType::Unknown`]).
    pub ty: MemberType,
    /// The method's parameters (empty for a field).
    pub params: Vec<Param>,
    /// The method's own type parameters, read from its generic `Signature`. Empty for a field, for a
    /// non-generic method, and for one carrying only a descriptor (which erases them away).
    pub type_params: Vec<TypeParamDecl>,
    /// Whether the method is varargs.
    pub varargs: bool,
    /// The checked exceptions the method declares (`throws`), captured like a supertype so they
    /// resolve by fully-qualified name. Empty for a field / constructor / method that declares none.
    pub throws: Vec<MemberType>,
}

pub(crate) use api::lower;

/// Namespace for the pure `.class` → [`ClassfileClass`] lowering functions.
mod api {
    use super::{
        Attribute, AttributeBody, ClassFile, ClassSignature, ClassTypeSignature, ClassfileClass,
        ClassfileMember, ConstantPool, Cow, DefKind, FieldAccessFlags, FieldType, Fqn,
        MemberModifiers, MemberType, MethodAccessFlags, MethodDescriptor, MethodSignature, Param,
        ResultSignature, ReturnType, ToOwned, TypeArgument, TypeParamDecl, TypeParameter,
        TypeSignature, Vec, vec,
    };

    /// Lower a class file to its [`ClassfileClass`], or `None` for `module-info` (a module, not a type).
    pub(crate) async fn lower(cf: &ClassFile) -> Option<ClassfileClass> {
        if cf.access_flags.is_module() {
            return None;
        }
        let pool = &cf.constant_pool;
        let fqn = jals_decompile::types::internal_to_java(&pool.class_name(cf.this_class)?);
        let class_sig = class_signature(cf, pool);
        let type_params = class_sig
            .as_ref()
            .map(|s| lower_type_params(&s.type_parameters))
            .unwrap_or_default();
        let supertypes = lower_supertypes(cf, class_sig.as_ref(), pool);
        let members = lower_members(cf, pool, Fqn::simple_name_of(&fqn)).await;
        Some(ClassfileClass {
            fqn,
            kind: class_kind(cf),
            type_params,
            supertypes,
            members,
        })
    }

    pub(crate) fn class_kind(cf: &ClassFile) -> DefKind {
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

    pub(crate) fn class_signature(cf: &ClassFile, pool: &ConstantPool) -> Option<ClassSignature> {
        ClassSignature::parse(&jals_decompile::attrs::signature_string(
            &cf.attributes,
            pool,
        )?)
        .ok()
    }

    pub(crate) fn lower_type_params(params: &[TypeParameter]) -> Vec<TypeParamDecl> {
        params
            .iter()
            .map(|tp| {
                // An unbounded `<T>` is written `<T:Ljava/lang/Object;>`, so its class bound has to
                // be dropped for the parameter to contribute none. An *explicit* `<T extends Object
                // & Comparable<? super T>>` is written `<T:Ljava/lang/Object;:Ljava/lang/Comparable
                // <-TT;>;>`, and the two are distinguishable by exactly one thing: javac spells the
                // first form of an interface-bounded parameter with an *empty* class bound
                // (`<T::Ljava/lang/Comparable<TT;>;>`, which parses to `class_bound: None`). So a
                // class bound that survived the parse alongside interface bounds is one the source
                // wrote, and dropping it erased `T` to its first *interface* bound —
                // `java.util.Collections.max`/`min` are declared exactly that way, and a call to one
                // was emitted as `(Ljava/util/Collection;)Ljava/lang/Comparable;`.
                let explicit = !tp.interface_bounds.is_empty();
                TypeParamDecl {
                    name: tp.name.clone(),
                    bounds: tp
                        .class_bound
                        .iter()
                        .filter(|t| explicit || !t.is_java_lang_object())
                        .chain(tp.interface_bounds.iter())
                        .map(type_sig_to_member_type)
                        .collect(),
                }
            })
            .collect()
    }

    pub(crate) fn lower_supertypes(
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

    pub(crate) async fn lower_members(
        cf: &ClassFile,
        pool: &ConstantPool,
        owner_simple: &str,
    ) -> Vec<ClassfileMember> {
        let mut yielder = jals_exec::Yielder::new();
        let mut out = Vec::new();
        for field in &cf.fields {
            yielder.tick().await;
            let Some(name) = pool.utf8(field.name_index).map(Cow::into_owned) else {
                continue;
            };
            out.push(ClassfileMember {
                name,
                kind: DefKind::Field,
                type_params: Vec::new(),
                modifiers: MemberModifiers {
                    is_static: field.access_flags.contains(FieldAccessFlags::STATIC),
                    is_private: field.access_flags.contains(FieldAccessFlags::PRIVATE),
                    is_public: field.access_flags.contains(FieldAccessFlags::PUBLIC),
                    // A field is never abstract; the flag does not exist for one.
                    is_abstract: false,
                },
                ty: field_member_type(&field.attributes, field.descriptor_index, pool),
                params: Vec::new(),
                varargs: false,
                throws: Vec::new(),
            });
        }
        for method in &cf.methods {
            yielder.tick().await;
            let Some(raw_name) = pool.utf8(method.name_index).map(Cow::into_owned) else {
                continue;
            };
            if raw_name == "<clinit>" {
                continue;
            }
            let (ret, params, varargs, type_params) = method_shape(method, pool);
            // The declared checked exceptions (`throws`), from the `Exceptions` attribute, as
            // fully-qualified named types so they resolve without an import context.
            let throws = jals_decompile::attrs::declared_throws(method, pool)
                .iter()
                .map(|fqn| named(fqn, 0, Vec::new()))
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
                type_params,
                modifiers: MemberModifiers {
                    is_static: method.access_flags.contains(MethodAccessFlags::STATIC),
                    is_private: method.access_flags.contains(MethodAccessFlags::PRIVATE),
                    is_public: method.access_flags.contains(MethodAccessFlags::PUBLIC),
                    // `ACC_ABSTRACT` is the compiled form of the same bit, implicit interface
                    // modifiers already folded in by whatever compiler wrote the class file.
                    is_abstract: method.access_flags.contains(MethodAccessFlags::ABSTRACT),
                },
                ty,
                params,
                varargs,
                throws,
            });
        }
        out
    }

    /// A field's type: from its `Signature` (generic) if present, else its descriptor.
    pub(crate) fn field_member_type(
        attrs: &[Attribute],
        descriptor_index: u16,
        pool: &ConstantPool,
    ) -> MemberType {
        if let Some(sig) = jals_decompile::attrs::signature_string(attrs, pool)
            && let Ok(ts) = TypeSignature::parse(&sig)
        {
            return type_sig_to_member_type(&ts);
        }
        if let Some(desc) = pool.utf8(descriptor_index)
            && let Ok(ft) = FieldType::parse(&desc)
        {
            return field_type_to_member_type(&ft);
        }
        MemberType::Unknown
    }

    /// A method's (return type, parameters, varargs): from its `Signature` (generic) if present, else its
    /// descriptor.
    pub(crate) fn method_shape(
        method: &jals_classfile::MethodInfo,
        pool: &ConstantPool,
    ) -> (MemberType, Vec<Param>, bool, Vec<TypeParamDecl>) {
        let varargs = method.access_flags.is_varargs();
        if let Some(sig) = jals_decompile::attrs::signature_string(&method.attributes, pool)
            && let Ok(ms) = MethodSignature::parse(&sig)
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
            // A generic method's own `<E>` lives only in the `Signature` attribute; the descriptor
            // has already erased it. Without it a bare `E` in this member's types resolves to an
            // external name the index has never heard of.
            return (ret, params, varargs, lower_type_params(&ms.type_parameters));
        }
        if let Some(desc) = pool.utf8(method.descriptor_index)
            && let Ok(md) = MethodDescriptor::parse(&desc)
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
            // No `Signature`: the descriptor is already erased, so there is no type variable left
            // to name.
            return (ret, params, varargs, Vec::new());
        }
        (MemberType::Unknown, Vec::new(), varargs, Vec::new())
    }

    // --- descriptor / signature → MemberType -----------------------------------------------------

    pub(crate) fn field_type_to_member_type(ft: &FieldType) -> MemberType {
        let (base, dims) = peel_field_array(ft, 0);
        match base {
            FieldType::Base(b) => MemberType::Primitive {
                keyword: b.keyword().to_owned(),
                dims,
            },
            FieldType::Object(internal) => named(
                &jals_decompile::types::internal_to_java(internal),
                dims,
                Vec::new(),
            ),
            FieldType::Array(_) => unreachable!("peeled"),
        }
    }

    pub(crate) fn peel_field_array(ft: &FieldType, dims: u32) -> (&FieldType, u32) {
        match ft {
            FieldType::Array(inner) => peel_field_array(inner, dims + 1),
            other => (other, dims),
        }
    }

    pub(crate) fn type_sig_to_member_type(ts: &TypeSignature) -> MemberType {
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

    pub(crate) fn peel_sig_array(ts: &TypeSignature, dims: u32) -> (&TypeSignature, u32) {
        match ts {
            TypeSignature::Array(inner) => peel_sig_array(inner, dims + 1),
            other => (other, dims),
        }
    }

    pub(crate) fn class_type_sig_to_member_type(c: &ClassTypeSignature, dims: u32) -> MemberType {
        // Fold the inner-class suffixes into one dotted name; the innermost component carries the args.
        let mut fqn = jals_decompile::types::internal_to_java(&c.name);
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

    pub(crate) fn type_arg_to_member_type(arg: &TypeArgument) -> MemberType {
        match arg {
            TypeArgument::Exact(t) => type_sig_to_member_type(t),
            // Wildcards are not modelled: kept as `Unknown` so positions stay aligned and assignment
            // stays lenient (matches the source path's treatment of `?`).
            TypeArgument::Any | TypeArgument::Extends(_) | TypeArgument::Super(_) => {
                MemberType::Unknown
            }
        }
    }

    pub(crate) fn named_from_internal(internal: &str) -> MemberType {
        named(
            &jals_decompile::types::internal_to_java(internal),
            0,
            Vec::new(),
        )
    }

    /// Build a fully-qualified [`MemberType::Named`] (the `qualified` form so it resolves without imports).
    pub(crate) fn named(fqn: &str, dims: u32, args: Vec<MemberType>) -> MemberType {
        MemberType::Named {
            name: Fqn::simple_name_of(fqn).to_owned(),
            qualified: Some(fqn.to_owned()),
            dims,
            args,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::api;
    use alloc::vec::Vec;

    use jals_classfile::MethodSignature;

    use super::MemberType;

    /// The bound names lowered from the type parameters of `signature`, in order.
    fn bounds(signature: &str) -> Vec<Vec<&'static str>> {
        let parsed =
            MethodSignature::parse(signature).expect("a signature javac could have written");
        api::lower_type_params(&parsed.type_parameters)
            .iter()
            .map(|param| {
                param
                    .bounds
                    .iter()
                    .map(|bound| match bound {
                        MemberType::Named { name, .. } => match name.as_str() {
                            "Object" => "Object",
                            "Comparable" => "Comparable",
                            "Number" => "Number",
                            other => panic!("unexpected bound `{other}`"),
                        },
                        other => panic!("unexpected bound {other:?}"),
                    })
                    .collect()
            })
            .collect()
    }

    /// An `Object` class bound is dropped only when it is the *implicit* one.
    ///
    /// javac spells the two forms apart, and this is the whole reason the distinction is readable:
    /// an unbounded `<T>` is written with `java/lang/Object` as its class bound, while a parameter
    /// bounded only by interfaces is written with an *empty* one. So a surviving `Object` alongside
    /// interface bounds is a bound the source wrote, and dropping it erased `T` to its first
    /// interface — `java.util.Collections.max`/`min` are declared exactly this way, and a call to
    /// one was emitted as `(Ljava/util/Collection;)Ljava/lang/Comparable;`, which links against
    /// nothing.
    #[test]
    fn an_explicit_object_bound_survives_beside_interface_bounds() {
        // `static <T> void f(T)` — the implicit bound, which contributes none.
        assert_eq!(bounds("<T:Ljava/lang/Object;>(TT;)V"), [Vec::<&str>::new()]);
        // `static <T extends Comparable<T>> void f(T)` — an empty class bound.
        assert_eq!(
            bounds("<T::Ljava/lang/Comparable<TT;>;>(TT;)V"),
            [["Comparable"]]
        );
        // `static <T extends Object & Comparable<? super T>> T max(Collection<? extends T>)`.
        assert_eq!(
            bounds(
                "<T:Ljava/lang/Object;:Ljava/lang/Comparable<-TT;>;>(Ljava/util/Collection<+TT;>;)TT;"
            ),
            [["Object", "Comparable"]]
        );
        // A non-`Object` class bound was never filtered and still is not.
        assert_eq!(bounds("<T:Ljava/lang/Number;>(TT;)V"), [["Number"]]);
    }
}
