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

/// Descriptor and internal-name conversions.
pub struct Descriptor;

impl Descriptor {
    /// A type's internal binary name (`java/lang/String`), from the dotted fully-qualified name the
    /// index holds.
    ///
    /// Every separator becomes `/`. A dotted name alone does not say which of its segments are
    /// packages, so a nested type's `$` cannot be recovered from one —
    /// [`internal_name_of`](Self::internal_name_of) asks the index instead.
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
        Ok(match ty {
            Ty::Primitive(primitive) => FieldType::Base(Self::base_type(*primitive)),
            Ty::Array(element) => {
                FieldType::Array(alloc::boxed::Box::new(Self::field_type(element, index)?))
            }
            Ty::Class(ClassTy::Project { id, .. }) => {
                FieldType::Object(Self::internal_name_of(*id, index))
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
        Self::method_descriptor_erasing(id, index, constructor, &[])
    }

    /// The method descriptor of `id`, treating each name in `vars` as a type *variable*.
    ///
    /// A method's own type parameters (`static <E> E pick(E a, E b)`) are not the *class's*, so the index
    /// resolves them as external names it has never heard of — and an unresolved external name is
    /// reported rather than guessed at, which is right everywhere except here. The caller knows which
    /// names its declaration bound, so it says so, and each becomes the `Object` a type variable erases
    /// to. Passing an empty list is the ordinary case and changes nothing.
    pub(crate) fn method_descriptor_erasing(
        id: MemberId,
        index: &ProjectIndex,
        constructor: bool,
        vars: &[String],
    ) -> Result<MethodDescriptor> {
        let erase = |ty: &Ty| Self::erase_variables(ty, vars);
        let params = index
            .resolved_param_tys(id)
            .iter()
            .map(|ty| Self::field_type(&erase(ty), index))
            .collect::<Result<Vec<_>>>()?;
        let return_type = if constructor {
            ReturnType::Void
        } else {
            Self::return_type(&erase(&index.resolved_member_ty(id)), index)?
        };
        Ok(MethodDescriptor {
            params,
            return_type,
        })
    }

    /// Replace every external-by-name type whose name is in `vars` with a type variable, recursively
    /// through array element types. A type variable erases to `Object`, which is what a method's own
    /// unbounded type parameter erases to.
    fn erase_variables(ty: &Ty, vars: &[String]) -> Ty {
        match ty {
            Ty::Class(jals_hir::ClassTy::External { name, .. })
                if vars.iter().any(|var| var == name) =>
            {
                // `Null` and `TypeVar` share one erasure here — `Object` — and `Null` needs no owning
                // item to name, which a synthetic `TypeVar` would have to invent.
                let _ = name;
                Ty::Null
            }
            Ty::Array(element) => {
                Ty::Array(alloc::boxed::Box::new(Self::erase_variables(element, vars)))
            }
            other => other.clone(),
        }
    }

    /// The field descriptor of the member `id`.
    pub fn field_descriptor(id: MemberId, index: &ProjectIndex) -> Result<FieldType> {
        Self::field_type(&index.resolved_member_ty(id), index)
    }

    /// The field descriptor of `ty` itself, for a value the index has no member for — an array's
    /// element, a local, or the target of a cast.
    pub(crate) fn descriptor_of(ty: &Ty, index: &ProjectIndex) -> Result<FieldType> {
        Self::field_type(ty, index)
    }

    /// The `Class` entry a `checkcast` / `instanceof` / `anewarray` names for `ty`.
    ///
    /// Two spellings in one place, because the class file uses both: a class is named by its internal
    /// binary name (`java/lang/String`) and an array by its own *descriptor* (`[Ljava/lang/String;`),
    /// which JVMS §4.4.1 permits precisely so an array type can be named at all.
    pub(crate) fn class_entry(ty: &Ty, index: &ProjectIndex) -> Result<String> {
        match Self::field_type(ty, index)? {
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
