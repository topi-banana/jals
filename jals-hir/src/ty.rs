//! Inferred types: the structural type a declaration or expression has.
//!
//! [`Ty`] is distinct from the syntactic [`jals_syntax::ast::Type`]. That is a CST node, the type
//! *as written* (`List<String>`, `int[]`); this is the resolved, structural type that inference
//! produces and a consumer (hover) displays. It is deliberately shallow — type arguments are carried
//! and consulted for same-nominal invariance ([`Ty::is_assignable_to`]) but variance / wildcards are
//! not modelled, and anything inference cannot work out is [`Ty::Unknown`] rather than an error, so
//! the pass never panics and degrades to a best effort.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::project::{ItemId, ItemOrigin, ProjectIndex};

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
    Array(Box<Self>),
    /// A nominal reference type (class / interface / enum / record), by name, with its type
    /// arguments (see [`ClassTy`]) — carried for display, member substitution, and same-nominal
    /// invariance.
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
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Byte => "byte",
            Self::Short => "short",
            Self::Int => "int",
            Self::Long => "long",
            Self::Char => "char",
            Self::Float => "float",
            Self::Double => "double",
        }
    }

    /// Whether this is one of the numeric primitives (everything but `boolean`).
    pub const fn is_numeric(self) -> bool {
        !matches!(self, Self::Boolean)
    }

    /// The primitive whose Java keyword spelling is `keyword` (`"int"`), if any. The inverse of
    /// [`as_str`](Primitive::as_str), single-sourced from it so the spelling table lives in one place.
    pub fn from_keyword(keyword: &str) -> Option<Self> {
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
    pub const fn widens_to(self, target: Self) -> bool {
        use Primitive::{Byte, Char, Double, Float, Int, Long, Short};
        matches!(
            (self, target),
            (Byte, Short | Int | Long | Float | Double)
                | (Short | Char, Int | Long | Float | Double)
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
        /// argument-free use (`List`). Used for display, generic member substitution, and the
        /// same-nominal invariance check in [`Ty::is_assignable_to`].
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
    pub fn external(name: impl Into<String>) -> Self {
        Self::External {
            name: name.into(),
            args: Vec::new(),
        }
    }

    /// The simple class name, common to both variants.
    pub fn name(&self) -> &str {
        match self {
            Self::Project { name, .. } | Self::External { name, .. } => name,
        }
    }

    /// The type arguments as written; empty for a raw or argument-free use.
    pub fn args(&self) -> &[Ty] {
        match self {
            Self::Project { args, .. } | Self::External { args, .. } => args,
        }
    }
}

impl Ty {
    /// Whether this is the `String` class type (the only reference type the MVP recognises by
    /// name, for `+` string-concatenation).
    pub fn is_string(&self) -> bool {
        matches!(self, Self::Class(c) if c.name() == "String")
    }

    /// The numeric primitive this type is, if any (`boolean` excluded).
    pub(crate) const fn as_numeric(&self) -> Option<Primitive> {
        match self {
            Self::Primitive(p) if p.is_numeric() => Some(*p),
            _ => None,
        }
    }

    /// The indexed project type's id, if this is one ([`ClassTy::Project`]).
    pub const fn project_id(&self) -> Option<ItemId> {
        match self {
            Self::Class(ClassTy::Project { id, .. }) => Some(*id),
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
    /// among themselves (widening only), `null`, arrays, the indexed project class hierarchy, and the
    /// same nominal type with provably-different type arguments (generic invariance) — so a consumer
    /// that emits a diagnostic on `false` never reports a spurious one.
    ///
    /// `index` supplies the project class hierarchy for reference subtyping; without it (the
    /// [`infer_node`](crate::infer_node) path, which has no [`ProjectIndex`]) subtyping between two
    /// distinct project types is unknowable, so it too stays conservatively `true`.
    // Each arm names one JLS conversion case; keeping them separate (rather than merging equal
    // bodies) is what documents which conversions are/aren't modelled.
    #[allow(clippy::match_same_arms)]
    pub fn is_assignable_to(&self, target: &Self, index: Option<&ProjectIndex>) -> bool {
        use ClassTy::{External, Project};
        use Ty::{Array, Class, Null, Primitive, TypeVar, Unknown, Void};

        // Unknown or an un-substituted type variable on either side: defer, never claim a mismatch.
        // A type variable stands for an unknown concrete type (its bound is not yet modelled), so
        // treating it leniently keeps the "never a false positive" guarantee.
        if matches!(self, Unknown | TypeVar { .. }) || matches!(target, Unknown | TypeVar { .. }) {
            return true;
        }
        // Generic invariance: the same nominal type with provably-different type arguments is not
        // assignable — Java generics are invariant, so `List<String>` is not a `List<Object>`.
        // Checked on the original types (before the stub demotion below), so the everyday JDK
        // arguments (`String`, `Integer`, …) compare precisely. Only a *definite* difference is a
        // mismatch; a raw use, a wildcard / type variable, or an external-by-name argument stays
        // lenient. A different nominal type (a genuine subtype, `List` → `Collection`) is left to the
        // nominal arm below and stays lenient on its arguments for now.
        if self.type_args_conflict(target) {
            return false;
        }
        // A type assigned to itself: the common case, and trivially assignable. Short-circuit before
        // the (allocating) stub demotion below, which would only rebuild both sides and re-compare
        // them equal anyway.
        if self == target {
            return true;
        }
        // A standard-library *stub* type carries only a partial hierarchy and member set (the common
        // members, no generics — see [`crate::stdlib`]), so checking it precisely risks a false
        // mismatch: an omitted supertype (`Integer` does not list `Comparable`) or autoboxing
        // (`Integer n = 1;`). Demote a stub-origin project type to its external (by-name, lenient)
        // form for assignment conversion; inference and hover still use the precise stub. Without an
        // index there are no stub project types, so this is a no-op (the `infer_node` path unchanged).
        let (lhs, rhs) = (self.demote_stdlib(index), target.demote_stdlib(index));
        // Identity covers equal primitives, the same project item, an equally-spelled external
        // type, and `void` to `void`; the structural `PartialEq` handles each.
        if lhs == rhs {
            return true;
        }

        match (&lhs, &rhs) {
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

            // Reference subtyping between two project types: walk the indexed supertype chain. With
            // no index (no hierarchy to consult) stay conservative rather than claim a mismatch.
            (Class(Project { id: s, .. }), Class(Project { id: t, .. })) => {
                index.is_none_or(|index| index.is_subtype(*s, *t))
            }
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

    /// Whether `self` and `target` are the *same* nominal class type carrying provably-different type
    /// arguments — a generic-invariance mismatch (`List<String>` assigned to `List<Object>`). Only the
    /// same indexed item is considered (a different nominal type is a job for the nominal-subtyping
    /// arm, which stays lenient on arguments); a raw use on either side, a differing arity, and any
    /// argument that is not fully known are all lenient, so this never reports a false positive.
    fn type_args_conflict(&self, target: &Self) -> bool {
        // The same nominal project item with provably-different arguments is exactly the top-level
        // same-item case of [`args_definitely_differ`] (raw use / arity / unknown-argument leniency
        // included). A *different* nominal type is the nominal-subtyping arm's job and stays lenient
        // here, so guard on equal ids first — `args_definitely_differ` would otherwise call two
        // different items a difference.
        matches!(
            (self, target),
            (Self::Class(ClassTy::Project { id: s, .. }), Self::Class(ClassTy::Project { id: t, .. }))
                if s == t
        ) && self.args_definitely_differ(target)
    }

    /// This type with every standard-library *stub* class type (a [`ClassTy::Project`] whose item has
    /// [`ItemOrigin::Stdlib`]) rewritten to its external (by-name) form, recursing through array
    /// elements and type arguments. The stubs are intentionally partial, so assignment conversion
    /// treats them leniently; see [`is_assignable_to`](Ty::is_assignable_to). Without an `index` (the
    /// project-free path) there are no stub project types and this clones unchanged.
    fn demote_stdlib(&self, index: Option<&ProjectIndex>) -> Self {
        let Some(index) = index else {
            return self.clone();
        };
        match self {
            Self::Class(ClassTy::Project { id, name, args })
                if index.item(*id).origin == ItemOrigin::Stdlib =>
            {
                Self::Class(ClassTy::External {
                    name: name.clone(),
                    args: args.iter().map(|a| a.demote_stdlib(Some(index))).collect(),
                })
            }
            Self::Class(ClassTy::Project { id, name, args }) => Self::Class(ClassTy::Project {
                id: *id,
                name: name.clone(),
                args: args.iter().map(|a| a.demote_stdlib(Some(index))).collect(),
            }),
            Self::Class(ClassTy::External { name, args }) => Self::Class(ClassTy::External {
                name: name.clone(),
                args: args.iter().map(|a| a.demote_stdlib(Some(index))).collect(),
            }),
            Self::Array(elem) => Self::Array(Box::new(elem.demote_stdlib(Some(index)))),
            Self::Primitive(_) | Self::Void | Self::Null | Self::TypeVar { .. } | Self::Unknown => {
                self.clone()
            }
        }
    }

    /// Returns a copy with every [`TypeVar`](Ty::TypeVar) replaced by `f(owner, name)` where that
    /// yields `Some`, recursing through array elements and class type arguments. A type variable `f`
    /// does not map (returns `None`) is left as-is — so an unbound parameter survives unchanged. The
    /// basis for binding a generic type's parameters to the arguments a use supplies.
    #[must_use]
    pub fn substitute(&self, f: &impl Fn(ItemId, &str) -> Option<Self>) -> Self {
        match self {
            Self::TypeVar { owner, name } => f(*owner, name).unwrap_or_else(|| self.clone()),
            Self::Array(elem) => Self::Array(Box::new(elem.substitute(f))),
            Self::Class(ClassTy::Project { id, name, args }) => Self::Class(ClassTy::Project {
                id: *id,
                name: name.clone(),
                args: args.iter().map(|a| a.substitute(f)).collect(),
            }),
            Self::Class(ClassTy::External { name, args }) => Self::Class(ClassTy::External {
                name: name.clone(),
                args: args.iter().map(|a| a.substitute(f)).collect(),
            }),
            Self::Primitive(_) | Self::Void | Self::Null | Self::Unknown => self.clone(),
        }
    }

    /// Whether two type arguments are *provably different* concrete types — the basis for the
    /// generic-invariance check ([`Ty::type_args_conflict`]). Lenient (`false`) whenever either side is
    /// not fully known: an [`Unknown`](Ty::Unknown) or [`TypeVar`](Ty::TypeVar), or an external
    /// ([`ClassTy::External`]) by-name type. Two project class types differ when their items differ, or
    /// (recursively, invariantly) any of their own arguments do; two primitives by inequality; two
    /// arrays by their elements. Any other (mixed-kind) pair is treated as not-provably-different to
    /// stay safe.
    fn args_definitely_differ(&self, other: &Self) -> bool {
        use ClassTy::Project;
        match (self, other) {
            (
                Self::Class(Project {
                    id: i, args: ia, ..
                }),
                Self::Class(Project {
                    id: j, args: ja, ..
                }),
            ) => {
                if i != j {
                    true
                } else if ia.is_empty() || ja.is_empty() || ia.len() != ja.len() {
                    // A raw argument at this level is lenient (`List<Map>` vs `List<Map<…>>`).
                    false
                } else {
                    ia.iter().zip(ja).any(|(x, y)| x.args_definitely_differ(y))
                }
            }
            (Self::Array(x), Self::Array(y)) => x.args_definitely_differ(y),
            (Self::Primitive(p), Self::Primitive(q)) => p != q,
            // Not fully known on either side (`Unknown`/`TypeVar`), an external by-name type, or any
            // other mixed-kind pair: not provably different, so lenient.
            _ => false,
        }
    }

    /// Wraps `self` in `dims` array levels (`dims = 2` → `self[][]`).
    #[must_use]
    pub(crate) fn array_of(self, dims: usize) -> Self {
        (0..dims).fold(self, |acc, _| Self::Array(Box::new(acc)))
    }

    /// The `java.lang.String` type as the MVP models it.
    pub(crate) fn string() -> Self {
        Self::Class(ClassTy::external("String"))
    }

    /// Unary numeric promotion (JLS §5.6.1): `byte` / `short` / `char` widen to `int`; other numeric
    /// types are unchanged; a non-numeric operand yields [`Ty::Unknown`].
    pub(crate) const fn unary_promote(&self) -> Self {
        match self.as_numeric() {
            Some(Primitive::Byte | Primitive::Short | Primitive::Char) => {
                Self::Primitive(Primitive::Int)
            }
            Some(p) => Self::Primitive(p),
            None => Self::Unknown,
        }
    }

    /// Binary numeric promotion (JLS §5.6.2): the result widens to the larger of `self` and `other`
    /// along `double > float > long > int`, with everything narrower than `int` promoted to `int`. A
    /// non-numeric operand yields [`Ty::Unknown`].
    pub(crate) fn binary_numeric(&self, other: &Self) -> Self {
        let (Some(a), Some(b)) = (self.as_numeric(), other.as_numeric()) else {
            return Self::Unknown;
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
        Self::Primitive(result)
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Primitive(p) => f.write_str(p.as_str()),
            Self::Void => f.write_str("void"),
            Self::Null => f.write_str("null"),
            Self::Array(elem) => write!(f, "{elem}[]"),
            Self::Class(c) => {
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
            Self::TypeVar { name, .. } => f.write_str(name),
            Self::Unknown => f.write_str("?"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_renders_java_spelling() {
        assert_eq!(Ty::Primitive(Primitive::Int).to_string(), "int");
        assert_eq!(Ty::Void.to_string(), "void");
        assert_eq!(Ty::Null.to_string(), "null");
        assert_eq!(Ty::string().to_string(), "String");
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
            args: vec![Ty::string()],
        });
        assert_eq!(list_of_string.to_string(), "List<String>");
        // Several arguments, comma-separated, and nesting: `Map<String, List<int>>`.
        let map = Ty::Class(ClassTy::External {
            name: "Map".to_string(),
            args: vec![Ty::string(), list_of_string],
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
        assert_eq!(byte.binary_numeric(&byte), int);
        assert_eq!(int.binary_numeric(&long), long);
        assert_eq!(long.binary_numeric(&double), double);
        // Unary promotion lifts the sub-int types to int.
        assert_eq!(byte.unary_promote(), int);
        assert_eq!(double.unary_promote(), double);
        // A non-numeric operand is unknown.
        assert_eq!(
            Ty::Primitive(Primitive::Boolean).binary_numeric(&int),
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
        let str_arr = Ty::Array(Box::new(Ty::string()));
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
