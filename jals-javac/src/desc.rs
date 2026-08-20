//! Turning a resolved semantic type into the class file's own vocabulary: internal names and
//! descriptors.
//!
//! This is where erasure happens. [`jals_hir::Ty`] carries type arguments, and the JVM's descriptor
//! grammar has no notion of them — `List<String>` and `List` are the same `Ljava/util/List;`. A
//! generic method's `T` erases to its bound, and with no bound to `java/lang/Object`.
//!
//! Every conversion can fail, and failing is the honest answer rather than a guess. A type this
//! layer cannot name — one inference never worked out, or an external class known only by the
//! spelling the source used — has no descriptor that would be *right*, and inventing one produces a
//! class file that loads and then throws `NoSuchMethodError` at the first call. A refusal here
//! surfaces the gap at compile time instead.

use alloc::borrow::ToOwned;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use jals_classfile::{BaseType, FieldType, MethodDescriptor, ReturnType};
use jals_hir::{ClassTy, ItemId, MemberId, Primitive, ProjectIndex, Ty};

/// Why a type could not be given a descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DescError {
    /// Inference produced no type for the value.
    Unknown,
    /// A type named only by its source spelling. Resolving it needs the declaring type on the
    /// classpath or in the stdlib stubs; nothing here can invent its package.
    Unresolved(String),
    /// `void` appeared where a value type is required.
    Void,
    /// A primitive appeared where the class file wants a `Class` entry — the operand of a
    /// `checkcast`, an `instanceof`, or an `anewarray`, none of which name a primitive.
    NotAClass,
}

impl core::fmt::Display for DescError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unknown => f.write_str("the type of a value could not be inferred"),
            Self::Unresolved(name) => {
                write!(
                    f,
                    "`{name}` is not an indexed type, so it has no internal name"
                )
            }
            Self::Void => f.write_str("`void` is not a value type"),
            Self::NotAClass => {
                f.write_str("a primitive type has no `Class` entry in the constant pool")
            }
        }
    }
}

impl core::error::Error for DescError {}

type Result<T> = core::result::Result<T, DescError>;

pub(crate) use api::{
    checkcast_class, class_entry, descriptor_of, field_type_of, internal_name_of,
};
pub use api::{field_descriptor, internal_name, method_descriptor};

/// Descriptor and internal-name conversions.
mod api {
    use super::{
        BaseType, ClassTy, DescError, FieldType, ItemId, MemberId, MethodDescriptor, Primitive,
        ProjectIndex, Result, ReturnType, String, ToOwned, ToString, Ty, Vec,
    };

    /// A type's internal binary name (`java/lang/String`), from the dotted fully-qualified name the
    /// index holds.
    ///
    /// Every separator becomes `/`. A dotted name alone does not say which of its segments are
    /// packages, so a nested type's `$` cannot be recovered from one —
    /// [`internal_name_of`](internal_name_of) asks the index instead.
    pub fn internal_name(fqn: &str) -> String {
        fqn.replace('.', "/")
    }

    /// An *indexed* type's internal binary name, with `$` where the nesting is.
    ///
    /// `Outer.Inner` is `Outer$Inner` and `com.example.Main` is `com/example/Main`, and nothing in the
    /// two dotted names distinguishes them — so each boundary is decided by asking whether the prefix
    /// before it is itself a type. Getting this wrong produces a class that loads under one name and is
    /// referred to under another, which is a `NoClassDefFoundError` at the first use.
    pub(crate) fn internal_name_of(id: ItemId, index: &ProjectIndex) -> String {
        let fqn = index.item(id).fqn.as_str();
        let mut out = String::with_capacity(fqn.len());
        let mut prefix = String::new();
        for (position, segment) in fqn.split('.').enumerate() {
            if position > 0 {
                out.push(if index.item_by_fqn(&prefix).is_some() {
                    '$'
                } else {
                    '/'
                });
                prefix.push('.');
            }
            out.push_str(segment);
            prefix.push_str(segment);
        }
        out
    }

    /// The field type a value of `ty` has, erased.
    fn field_type(ty: &Ty, index: &ProjectIndex) -> Result<FieldType> {
        field_type_within(ty, index, 0)
    }

    /// [`field_type`](field_type), for a lowering that has to compare a value's erasure with a
    /// slot's — the argument narrowing an unchecked call needs.
    ///
    /// # Errors
    /// As [`field_type`](field_type): a type this layer cannot name is refused rather than
    /// guessed at.
    pub(crate) fn field_type_of(ty: &Ty, index: &ProjectIndex) -> Result<FieldType> {
        field_type(ty, index)
    }

    /// How far a chain of type-variable bounds is followed before answering `Object`.
    ///
    /// `<T extends U, U extends Number>` erases `T` through `U`, so the walk is genuinely recursive
    /// and needs a stop. Real bound chains are a step or two; the limit exists so a cyclic or
    /// malformed one — this crate never checks, so it can be handed either — terminates with the
    /// conservative answer rather than the stack.
    const BOUND_DEPTH: u8 = 8;
    /// The one type every erasure falls back to.
    const OBJECT: &str = "java/lang/Object";

    /// [`field_type`](field_type), tracking how many type-variable bounds have been followed.
    fn field_type_within(ty: &Ty, index: &ProjectIndex, depth: u8) -> Result<FieldType> {
        Ok(match ty {
            Ty::Primitive(primitive) => FieldType::Base(base_type(*primitive)),
            Ty::Array(element) => FieldType::Array(alloc::boxed::Box::new(field_type_within(
                element, index, depth,
            )?)),
            Ty::Class(ClassTy::Project { id, .. }) => {
                FieldType::Object(internal_name_of(*id, index))
            }
            Ty::Class(ClassTy::External { name, .. }) => {
                return Err(DescError::Unresolved(name.clone()));
            }
            // `null` has no type of its own — whatever slot it flows into supplies one, and every
            // such slot is a reference.
            Ty::Null => FieldType::Object(OBJECT.to_owned()),
            // A type variable erases to its leftmost bound (JLS §4.6), and to `Object` when it
            // declares none. Answering `Object` for a *bounded* one is self-consistent within a
            // single compilation — the declaration and its call sites agree — and disagrees with
            // every separately compiled caller, which is a `NoSuchMethodError` rather than an
            // imprecision.
            //
            // A bound this layer cannot *name* falls back to `Object` rather than refusing, which
            // is the one place that leniency is right. Every other unresolved type is a value the
            // caller wrote and the descriptor has to spell; a bound is a fact about the index, and
            // the index is routinely partial — `Runnable`, `Cloneable`, `Comparator`, and every
            // `java.util.function` type are absent from the embedded stubs, so refusing here made
            // `<T extends Runnable>` uncompilable in the stub-only configuration the playground and
            // this crate's own tests use. `Object` is what an unbounded parameter erases to anyway,
            // so the fallback is the answer this line gave before bounds were read at all.
            Ty::TypeVar {
                owner,
                member,
                name,
            } => match index.type_var_bound(*owner, *member, name) {
                Some(bound) if depth < BOUND_DEPTH => field_type_within(&bound, index, depth + 1)
                    .unwrap_or_else(|_| FieldType::Object(OBJECT.to_owned())),
                _ => FieldType::Object(OBJECT.to_owned()),
            },
            Ty::Void => return Err(DescError::Void),
            Ty::Unknown => return Err(DescError::Unknown),
        })
    }

    /// The class a `checkcast` to `field_type` names, or `None` for a primitive — which is never
    /// cast.
    ///
    /// A class type names itself; an array names its own descriptor (`[Ljava/lang/Integer;`), which
    /// is how a `CONSTANT_Class` spells one. Reading only the class case is what let every array
    /// parameter — which is every varargs and every `T[]` — through a bridge and an unchecked call
    /// uncast.
    pub(crate) fn checkcast_class(field_type: &FieldType) -> Option<String> {
        match field_type {
            FieldType::Object(name) => Some(name.clone()),
            array @ FieldType::Array(_) => Some(array.to_string()),
            FieldType::Base(_) => None,
        }
    }

    /// The return descriptor for `ty`, where `void` is legal.
    fn return_type(ty: &Ty, index: &ProjectIndex) -> Result<ReturnType> {
        match ty {
            Ty::Void => Ok(ReturnType::Void),
            other => Ok(ReturnType::Type(field_type(other, index)?)),
        }
    }

    /// The descriptor of the member `id` as a method: its parameters and return type.
    ///
    /// A constructor returns `V` and has no declared value type of its own — [`jals_hir::Member`]
    /// records `Unknown` for it — so its return type is supplied here rather than read.
    pub fn method_descriptor(
        id: MemberId,
        index: &ProjectIndex,
        constructor: bool,
    ) -> Result<MethodDescriptor> {
        let params = index
            .resolved_param_tys(id)
            .iter()
            .map(|ty| field_type(ty, index))
            .collect::<Result<Vec<_>>>()?;
        let return_type = if constructor {
            ReturnType::Void
        } else {
            return_type(&index.resolved_member_ty(id), index)?
        };
        Ok(MethodDescriptor {
            params,
            return_type,
        })
    }

    /// The field descriptor of the member `id`.
    pub fn field_descriptor(id: MemberId, index: &ProjectIndex) -> Result<FieldType> {
        field_type(&index.resolved_member_ty(id), index)
    }

    /// The field descriptor of `ty` itself, for a value the index has no member for — an array's
    /// element, a local, or the target of a cast.
    pub(crate) fn descriptor_of(ty: &Ty, index: &ProjectIndex) -> Result<FieldType> {
        field_type(ty, index)
    }

    /// The `Class` entry a `checkcast` / `instanceof` / `anewarray` names for `ty`.
    ///
    /// Two spellings in one place, because the class file uses both: a class is named by its internal
    /// binary name (`java/lang/String`) and an array by its own *descriptor* (`[Ljava/lang/String;`),
    /// which JVMS §4.4.1 permits precisely so an array type can be named at all.
    pub(crate) fn class_entry(ty: &Ty, index: &ProjectIndex) -> Result<String> {
        match field_type(ty, index)? {
            FieldType::Object(name) => Ok(name),
            array @ FieldType::Array(_) => {
                use alloc::string::ToString as _;
                Ok(array.to_string())
            }
            FieldType::Base(_) => Err(DescError::NotAClass),
        }
    }

    const fn base_type(primitive: Primitive) -> BaseType {
        match primitive {
            Primitive::Boolean => BaseType::Boolean,
            Primitive::Byte => BaseType::Byte,
            Primitive::Short => BaseType::Short,
            Primitive::Int => BaseType::Int,
            Primitive::Long => BaseType::Long,
            Primitive::Char => BaseType::Char,
            Primitive::Float => BaseType::Float,
            Primitive::Double => BaseType::Double,
        }
    }
}
