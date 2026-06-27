//! Inferred types: the structural type a declaration or expression has.
//!
//! [`Ty`] is distinct from the syntactic [`jals_syntax::ast::Type`]. That is a CST node, the type
//! *as written* (`List<String>`, `int[]`); this is the resolved, structural type that inference
//! produces and a consumer (hover) displays. The MVP is deliberately shallow — type arguments are
//! carried for display but not yet consulted in subtyping, and anything inference cannot work out is
//! [`Ty::Unknown`] rather than an error, so the pass never panics and degrades to a best effort.

use std::fmt;

use crate::project::{ItemId, ProjectIndex};

/// An inferred Java type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    /// A primitive type (`int`, `boolean`, …).
    Primitive(Primitive),
    /// The `void` pseudo-type (a `void` method, a `void` cast slot).
    Void,
    /// The type of the `null` literal: assignable to any reference type.
    Null,
    /// An array; the boxed type is the element type (`int[][]` nests).
    Array(Box<Ty>),
    /// A nominal reference type (class / interface / enum / record), by name, with its type
    /// arguments (see [`ClassTy`]) carried for display but not yet used in subtyping.
    Class(ClassTy),
    /// An un-substituted type variable: a reference to the type parameter `name` of the indexed type
    /// `owner` (`class Box<E>` → `E` is `TypeVar { owner: Box, name: "E" }`). Substituted by a
    /// concrete type when a use supplies arguments (`Box<String>`); left as-is for a raw use, where
    /// it displays as its name. Treated leniently in subtyping (like [`Unknown`](Ty::Unknown)).
    TypeVar { owner: ItemId, name: String },
    /// The error / could-not-infer type. Propagates instead of failing; never surfaced as a real
    /// type to a consumer (hover suppresses it).
    Unknown,
}

/// A Java primitive type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Primitive {
    Boolean,
    Byte,
    Short,
    Int,
    Long,
    Char,
    Float,
    Double,
}

impl Primitive {
    /// The Java keyword spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Primitive::Boolean => "boolean",
            Primitive::Byte => "byte",
            Primitive::Short => "short",
            Primitive::Int => "int",
            Primitive::Long => "long",
            Primitive::Char => "char",
            Primitive::Float => "float",
            Primitive::Double => "double",
        }
    }

    /// Whether this is one of the numeric primitives (everything but `boolean`).
    pub fn is_numeric(self) -> bool {
        !matches!(self, Primitive::Boolean)
    }

    /// The primitive whose Java keyword spelling is `keyword` (`"int"`), if any. The inverse of
    /// [`as_str`](Primitive::as_str), single-sourced from it so the spelling table lives in one place.
    pub fn from_keyword(keyword: &str) -> Option<Primitive> {
        const ALL: [Primitive; 8] = [
            Primitive::Boolean,
            Primitive::Byte,
            Primitive::Short,
            Primitive::Int,
            Primitive::Long,
            Primitive::Char,
            Primitive::Float,
            Primitive::Double,
        ];
        ALL.into_iter().find(|p| p.as_str() == keyword)
    }

    /// Widening primitive conversion (JLS §5.1.2): can a value of `self` widen to `target`
    /// without a cast? `boolean` widens to nothing. Reflexive pairs (`self == target`) are *not*
    /// widenings — the caller handles identity separately.
    pub fn widens_to(self, target: Primitive) -> bool {
        use Primitive::*;
        matches!(
            (self, target),
            (Byte, Short | Int | Long | Float | Double)
                | (Short, Int | Long | Float | Double)
                | (Char, Int | Long | Float | Double)
                | (Int, Long | Float | Double)
                | (Long, Float | Double)
                | (Float, Double)
        )
    }
}

/// A nominal reference type, identified by name and carrying its type arguments as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassTy {
    /// Resolved to a type declared in the indexed project. `name` is the simple name, kept so the
    /// type displays without the index (and so a [`DefId`]-free `Ty` stays self-describing).
    Project {
        id: ItemId,
        name: String,
        /// The type arguments as written (`List<String>` → `[String]`). Empty for a raw or
        /// argument-free use (`List`). Carried for display and future generic substitution; the
        /// MVP's subtyping ([`Ty::is_assignable_to`]) does not yet consult them.
        args: Vec<Ty>,
    },
    /// A type known only by name — a JDK / external type, or one we chose not to resolve (the
    /// project-free [`infer_node`](crate::infer_node) path). Carries the spelling as written.
    External {
        name: String,
        /// The type arguments as written, like [`Project`](ClassTy::Project)'s `args`.
        args: Vec<Ty>,
    },
}

impl ClassTy {
    /// An external (JDK / unindexed) class type with no type arguments.
    pub fn external(name: impl Into<String>) -> ClassTy {
        ClassTy::External {
            name: name.into(),
            args: Vec::new(),
        }
    }

    /// The simple class name, common to both variants.
    pub fn name(&self) -> &str {
        match self {
            ClassTy::Project { name, .. } | ClassTy::External { name, .. } => name,
        }
    }

    /// The type arguments as written; empty for a raw or argument-free use.
    pub fn args(&self) -> &[Ty] {
        match self {
            ClassTy::Project { args, .. } | ClassTy::External { args, .. } => args,
        }
    }
}

impl Ty {
    /// Whether this is the `String` class type (the only reference type the MVP recognises by
    /// name, for `+` string-concatenation).
    pub fn is_string(&self) -> bool {
        matches!(self, Ty::Class(c) if c.name() == "String")
    }

    /// The numeric primitive this type is, if any (`boolean` excluded).
    pub(crate) fn as_numeric(&self) -> Option<Primitive> {
        match self {
            Ty::Primitive(p) if p.is_numeric() => Some(*p),
            _ => None,
        }
    }

    /// The indexed project type's id, if this is one ([`ClassTy::Project`]).
    pub fn project_id(&self) -> Option<ItemId> {
        match self {
            Ty::Class(ClassTy::Project { id, .. }) => Some(*id),
            _ => None,
        }
    }

    /// Assignment conversion (JLS §5.2): may a value of `self` be assigned to a slot of type
    /// `target` without a cast?
    ///
    /// **Conservative — never a false mismatch.** It returns `true` whenever the answer is not
    /// fully knowable: either side [`Unknown`](Ty::Unknown), an external ([`ClassTy::External`])
    /// type whose hierarchy we do not index, or a primitive/reference pair that boxing or unboxing
    /// could bridge. It returns `false` only for combinations we model completely — primitives
    /// among themselves (widening only), `null`, arrays, and the indexed project class hierarchy —
    /// so a consumer that emits a diagnostic on `false` never reports a spurious one.
    ///
    /// `index` supplies the project class hierarchy for reference subtyping; without it (the
    /// [`infer_node`](crate::infer_node) path, which has no [`ProjectIndex`]) subtyping between two
    /// distinct project types is unknowable, so it too stays conservatively `true`.
    pub fn is_assignable_to(&self, target: &Ty, index: Option<&ProjectIndex>) -> bool {
        use ClassTy::{External, Project};
        use Ty::*;

        // Unknown or an un-substituted type variable on either side: defer, never claim a mismatch.
        // A type variable stands for an unknown concrete type (its bound is not yet modelled), so
        // treating it leniently keeps the "never a false positive" guarantee.
        if matches!(self, Unknown | TypeVar { .. }) || matches!(target, Unknown | TypeVar { .. }) {
            return true;
        }
        // Identity covers equal primitives, the same project item, an equally-spelled external
        // type, and `void` to `void`; the structural `PartialEq` handles each.
        if self == target {
            return true;
        }

        match (self, target) {
            // `null` is assignable to any reference type, never to a primitive or `void`.
            (Null, Class(_) | Array(_)) => true,
            (Null, _) => false,

            // Widening primitive conversion; identity (equal primitives) handled above.
            (Primitive(s), Primitive(t)) => s.widens_to(*t),
            // Boxing: a primitive may box to an external wrapper / `Object`, never to a user type
            // or an array.
            (Primitive(_), Class(External { .. })) => true,
            (Primitive(_), Class(Project { .. }) | Array(_) | Void) => false,

            // Unboxing: an external reference may be a numeric wrapper; a user type or array is not.
            (Class(External { .. }), Primitive(_)) => true,
            (Class(Project { .. }) | Array(_), Primitive(_)) => false,

            // Reference subtyping between two project types: walk the indexed supertype chain.
            (Class(Project { id: s, .. }), Class(Project { id: t, .. })) => match index {
                Some(index) => index.is_subtype(*s, *t),
                // No hierarchy to consult: stay conservative rather than claim a mismatch.
                None => true,
            },
            // A project type may widen to an external supertype (`Object`, a JDK interface); an
            // external type might really be an unindexed project type — both conservatively `true`.
            (Class(Project { .. }), Class(External { .. }))
            | (Class(External { .. }), Class(Project { .. }))
            | (Class(External { .. }), Class(External { .. })) => true,

            // Arrays: invariant for primitive elements, covariant for reference elements.
            (Array(s), Array(t)) => match (s.as_ref(), t.as_ref()) {
                (Primitive(a), Primitive(b)) => a == b,
                _ => s.is_assignable_to(t, index),
            },
            // An array is a reference type: it widens to `Object` / `Cloneable` / `Serializable`
            // (external), but never to a user class.
            (Array(_), Class(External { .. })) => true,
            (Array(_), Class(Project { .. })) => false,

            // `void` is assignable only to itself (handled by identity above).
            (Void, _) => false,

            // Anything left (e.g. a reference assigned where the `null`/array cases did not match)
            // is a confident mismatch.
            _ => false,
        }
    }

    /// Returns a copy with every [`TypeVar`](Ty::TypeVar) replaced by `f(owner, name)` where that
    /// yields `Some`, recursing through array elements and class type arguments. A type variable `f`
    /// does not map (returns `None`) is left as-is — so an unbound parameter survives unchanged. The
    /// basis for binding a generic type's parameters to the arguments a use supplies.
    pub fn substitute(&self, f: &impl Fn(ItemId, &str) -> Option<Ty>) -> Ty {
        match self {
            Ty::TypeVar { owner, name } => f(*owner, name).unwrap_or_else(|| self.clone()),
            Ty::Array(elem) => Ty::Array(Box::new(elem.substitute(f))),
            Ty::Class(ClassTy::Project { id, name, args }) => Ty::Class(ClassTy::Project {
                id: *id,
                name: name.clone(),
                args: args.iter().map(|a| a.substitute(f)).collect(),
            }),
            Ty::Class(ClassTy::External { name, args }) => Ty::Class(ClassTy::External {
                name: name.clone(),
                args: args.iter().map(|a| a.substitute(f)).collect(),
            }),
            Ty::Primitive(_) | Ty::Void | Ty::Null | Ty::Unknown => self.clone(),
        }
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Primitive(p) => f.write_str(p.as_str()),
            Ty::Void => f.write_str("void"),
            Ty::Null => f.write_str("null"),
            Ty::Array(elem) => write!(f, "{elem}[]"),
            Ty::Class(c) => {
                f.write_str(c.name())?;
                if let [first, rest @ ..] = c.args() {
                    write!(f, "<{first}")?;
                    for arg in rest {
                        write!(f, ", {arg}")?;
                    }
                    f.write_str(">")?;
                }
                Ok(())
            }
            Ty::TypeVar { name, .. } => f.write_str(name),
            Ty::Unknown => f.write_str("?"),
        }
    }
}

/// The `java.lang.String` type as the MVP models it.
pub(crate) fn string_ty() -> Ty {
    Ty::Class(ClassTy::external("String"))
}

/// Unary numeric promotion (JLS §5.6.1): `byte` / `short` / `char` widen to `int`; other numeric
/// types are unchanged; a non-numeric operand yields [`Ty::Unknown`].
pub(crate) fn unary_promote(t: &Ty) -> Ty {
    match t.as_numeric() {
        Some(Primitive::Byte | Primitive::Short | Primitive::Char) => Ty::Primitive(Primitive::Int),
        Some(p) => Ty::Primitive(p),
        None => Ty::Unknown,
    }
}

/// Binary numeric promotion (JLS §5.6.2): the result widens to the larger of the two operands
/// along `double > float > long > int`, with everything narrower than `int` promoted to `int`. A
/// non-numeric operand yields [`Ty::Unknown`].
pub(crate) fn binary_numeric(l: &Ty, r: &Ty) -> Ty {
    let (Some(a), Some(b)) = (l.as_numeric(), r.as_numeric()) else {
        return Ty::Unknown;
    };
    let result = if a == Primitive::Double || b == Primitive::Double {
        Primitive::Double
    } else if a == Primitive::Float || b == Primitive::Float {
        Primitive::Float
    } else if a == Primitive::Long || b == Primitive::Long {
        Primitive::Long
    } else {
        Primitive::Int
    };
    Ty::Primitive(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_renders_java_spelling() {
        assert_eq!(Ty::Primitive(Primitive::Int).to_string(), "int");
        assert_eq!(Ty::Void.to_string(), "void");
        assert_eq!(Ty::Null.to_string(), "null");
        assert_eq!(string_ty().to_string(), "String");
        assert_eq!(
            Ty::Array(Box::new(Ty::Primitive(Primitive::Long))).to_string(),
            "long[]"
        );
        assert_eq!(
            Ty::Array(Box::new(Ty::Array(Box::new(Ty::Primitive(Primitive::Int))))).to_string(),
            "int[][]"
        );
        assert_eq!(Ty::Unknown.to_string(), "?");
    }

    #[test]
    fn display_renders_type_arguments() {
        // A raw / argument-free class renders as the bare name.
        assert_eq!(
            Ty::Class(ClassTy::external("List")).to_string(),
            "List",
            "no args: bare name"
        );
        // A single type argument: `List<String>`.
        let list_of_string = Ty::Class(ClassTy::External {
            name: "List".to_string(),
            args: vec![string_ty()],
        });
        assert_eq!(list_of_string.to_string(), "List<String>");
        // Several arguments, comma-separated, and nesting: `Map<String, List<int>>`.
        let map = Ty::Class(ClassTy::External {
            name: "Map".to_string(),
            args: vec![string_ty(), list_of_string],
        });
        assert_eq!(map.to_string(), "Map<String, List<String>>");
    }

    #[test]
    fn numeric_promotion_widens_to_the_larger_operand() {
        let byte = Ty::Primitive(Primitive::Byte);
        let int = Ty::Primitive(Primitive::Int);
        let long = Ty::Primitive(Primitive::Long);
        let double = Ty::Primitive(Primitive::Double);
        // byte + byte = int (the common surprise).
        assert_eq!(binary_numeric(&byte, &byte), int);
        assert_eq!(binary_numeric(&int, &long), long);
        assert_eq!(binary_numeric(&long, &double), double);
        // Unary promotion lifts the sub-int types to int.
        assert_eq!(unary_promote(&byte), int);
        assert_eq!(unary_promote(&double), double);
        // A non-numeric operand is unknown.
        assert_eq!(
            binary_numeric(&Ty::Primitive(Primitive::Boolean), &int),
            Ty::Unknown
        );
    }

    #[test]
    fn widening_primitive_conversion() {
        use Primitive::*;
        assert!(Int.widens_to(Long));
        assert!(Byte.widens_to(Int));
        assert!(Char.widens_to(Int));
        assert!(Long.widens_to(Double));
        // Narrowing, sideways, and `boolean` never widen.
        assert!(!Double.widens_to(Int));
        assert!(!Long.widens_to(Int));
        assert!(!Byte.widens_to(Char));
        assert!(!Boolean.widens_to(Int));
        assert!(!Int.widens_to(Boolean));
        // Not reflexive: identity is the caller's responsibility.
        assert!(!Int.widens_to(Int));
    }

    /// The index-free (`infer_node`) cases: primitives, `null`, `void`, externals, `Unknown`.
    #[test]
    fn assignability_without_an_index() {
        let int = Ty::Primitive(Primitive::Int);
        let long = Ty::Primitive(Primitive::Long);
        let boolean = Ty::Primitive(Primitive::Boolean);
        let obj = Ty::Class(ClassTy::external("Object"));

        // Identity and widening; narrowing is a mismatch.
        assert!(int.is_assignable_to(&int, None));
        assert!(int.is_assignable_to(&long, None));
        assert!(!long.is_assignable_to(&int, None));
        assert!(!boolean.is_assignable_to(&int, None));

        // `null` flows to a reference type, not to a primitive.
        assert!(Ty::Null.is_assignable_to(&obj, None));
        assert!(!Ty::Null.is_assignable_to(&int, None));

        // Boxing / unboxing against an external type stays lenient (no false mismatch).
        assert!(int.is_assignable_to(&obj, None));
        assert!(obj.is_assignable_to(&int, None));

        // `Unknown` is compatible in both directions.
        assert!(Ty::Unknown.is_assignable_to(&int, None));
        assert!(int.is_assignable_to(&Ty::Unknown, None));

        // `void` is assignable only to `void`.
        assert!(Ty::Void.is_assignable_to(&Ty::Void, None));
        assert!(!Ty::Void.is_assignable_to(&int, None));
        assert!(!int.is_assignable_to(&Ty::Void, None));
    }

    #[test]
    fn assignability_of_arrays() {
        let int_arr = Ty::Array(Box::new(Ty::Primitive(Primitive::Int)));
        let long_arr = Ty::Array(Box::new(Ty::Primitive(Primitive::Long)));
        let obj = Ty::Class(ClassTy::external("Object"));
        let str_arr = Ty::Array(Box::new(string_ty()));
        let cs_arr = Ty::Array(Box::new(Ty::Class(ClassTy::external("CharSequence"))));

        // Primitive element arrays are invariant.
        assert!(int_arr.is_assignable_to(&int_arr, None));
        assert!(!long_arr.is_assignable_to(&int_arr, None));
        // Reference element arrays are covariant (external elements stay lenient).
        assert!(str_arr.is_assignable_to(&cs_arr, None));
        // An array is a reference type: assignable to `Object`.
        assert!(int_arr.is_assignable_to(&obj, None));
    }
}
