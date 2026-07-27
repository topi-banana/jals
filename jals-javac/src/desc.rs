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
use alloc::string::String;
use alloc::vec::Vec;

use jals_classfile::{BaseType, FieldType, MethodDescriptor, ReturnType};
use jals_hir::{ClassTy, MemberId, Primitive, ProjectIndex, Ty};

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
        }
    }
}

impl core::error::Error for DescError {}

type Result<T> = core::result::Result<T, DescError>;

/// Descriptor and internal-name conversions.
pub struct Descriptor;

impl Descriptor {
    /// A type's internal binary name (`java/lang/String`), from the dotted fully-qualified name the
    /// index holds.
    ///
    /// Only package separators become `/`; a nested type's `$` separator is not recovered here,
    /// because a dotted name alone does not say which of its segments are packages.
    pub fn internal_name(fqn: &str) -> String {
        fqn.replace('.', "/")
    }

    /// The field type a value of `ty` has, erased.
    fn field_type(ty: &Ty, index: &ProjectIndex) -> Result<FieldType> {
        Ok(match ty {
            Ty::Primitive(primitive) => FieldType::Base(Self::base_type(*primitive)),
            Ty::Array(element) => {
                FieldType::Array(alloc::boxed::Box::new(Self::field_type(element, index)?))
            }
            Ty::Class(ClassTy::Project { id, .. }) => {
                FieldType::Object(Self::internal_name(index.item(*id).fqn.as_str()))
            }
            Ty::Class(ClassTy::External { name, .. }) => {
                return Err(DescError::Unresolved(name.clone()));
            }
            // Both erase to `Object`, for the same reason from opposite directions. `null` has no
            // type of its own — whatever slot it flows into supplies one, and every such slot is a
            // reference. An unbounded type variable has no type left after erasure. (A *bounded*
            // variable erases to its bound, which needs the declaring item's `TypeParamDecl`; until
            // a milestone needs it, `Object` is the conservative answer.)
            Ty::Null | Ty::TypeVar { .. } => FieldType::Object("java/lang/Object".to_owned()),
            Ty::Void => return Err(DescError::Void),
            Ty::Unknown => return Err(DescError::Unknown),
        })
    }

    /// The return descriptor for `ty`, where `void` is legal.
    fn return_type(ty: &Ty, index: &ProjectIndex) -> Result<ReturnType> {
        match ty {
            Ty::Void => Ok(ReturnType::Void),
            other => Ok(ReturnType::Type(Self::field_type(other, index)?)),
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
            .map(|ty| Self::field_type(ty, index))
            .collect::<Result<Vec<_>>>()?;
        let return_type = if constructor {
            ReturnType::Void
        } else {
            Self::return_type(&index.resolved_member_ty(id), index)?
        };
        Ok(MethodDescriptor {
            params,
            return_type,
        })
    }

    /// The field descriptor of the member `id`.
    pub fn field_descriptor(id: MemberId, index: &ProjectIndex) -> Result<FieldType> {
        Self::field_type(&index.resolved_member_ty(id), index)
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
