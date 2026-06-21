//! Inferred types: the structural type a declaration or expression has.
//!
//! [`Ty`] is distinct from the syntactic [`jals_syntax::ast::Type`]. That is a CST node, the type
//! *as written* (`List<String>`, `int[]`); this is the resolved, structural type that inference
//! produces and a consumer (hover) displays. The MVP is deliberately shallow — type arguments are
//! dropped, and anything inference cannot work out is [`Ty::Unknown`] rather than an error, so the
//! pass never panics and degrades to a best effort.

use std::fmt;

use crate::project::ItemId;

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
    /// A nominal reference type (class / interface / enum / record), by name. Type arguments are
    /// dropped in the MVP.
    Class(ClassTy),
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
}

/// A nominal reference type, identified by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassTy {
    /// Resolved to a type declared in the indexed project. `name` is the simple name, kept so the
    /// type displays without the index (and so a [`DefId`]-free `Ty` stays self-describing).
    Project { id: ItemId, name: String },
    /// A type known only by name — a JDK / external type, or one we chose not to resolve (the
    /// project-free [`infer_node`](crate::infer_node) path). Carries the spelling as written.
    External(String),
}

impl Ty {
    /// Whether this is the `String` class type (the only reference type the MVP recognises by
    /// name, for `+` string-concatenation).
    pub fn is_string(&self) -> bool {
        match self {
            Ty::Class(ClassTy::External(s)) => s == "String",
            Ty::Class(ClassTy::Project { name, .. }) => name == "String",
            _ => false,
        }
    }

    /// The numeric primitive this type is, if any (`boolean` excluded).
    pub(crate) fn as_numeric(&self) -> Option<Primitive> {
        match self {
            Ty::Primitive(p) if p.is_numeric() => Some(*p),
            _ => None,
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
            Ty::Class(ClassTy::Project { name, .. }) => f.write_str(name),
            Ty::Class(ClassTy::External(name)) => f.write_str(name),
            Ty::Unknown => f.write_str("?"),
        }
    }
}

/// The `java.lang.String` type as the MVP models it.
pub(crate) fn string_ty() -> Ty {
    Ty::Class(ClassTy::External("String".to_string()))
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
}
