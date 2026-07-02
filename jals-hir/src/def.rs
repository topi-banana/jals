//! Definitions (bindings): the things a name can resolve to.

use alloc::string::String;
use core::ops::Range;

use crate::scope::ScopeId;

/// A stable, dense identifier for a [`Def`] within one [`Resolved`](crate::Resolved) file.
///
/// It indexes [`Resolved::defs`](crate::Resolved::defs) and is stable for that value's lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DefId(pub(crate) u32);

/// The name-space a name lives in.
///
/// Java resolves the *same* spelling differently depending on syntactic position (JLS §6.5): a
/// type context, a variable/value context, and a method-invocation context are independent, so a
/// class, a field, and a method may all share a name without colliding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Namespace {
    /// Types: classes, interfaces, enums, records, annotation types, and type parameters.
    Type,
    /// Values: locals, parameters, fields, enum constants, catch / resource / pattern variables.
    Value,
    /// Methods, in invocation position.
    Method,
}

/// What kind of declaration a [`Def`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefKind {
    /// A local variable (`int x = 1;`).
    Local,
    /// A method or constructor parameter (of a body-bearing executable).
    Param,
    /// A lambda parameter. Distinct from [`Param`](DefKind::Param) because an unused lambda
    /// parameter is routinely intentional, so consumers treat the two differently.
    LambdaParam,
    /// A type parameter (`<T>`).
    TypeParam,
    /// A field.
    Field,
    /// A method.
    Method,
    /// A constructor.
    Constructor,
    /// A class.
    Class,
    /// An interface.
    Interface,
    /// An enum.
    Enum,
    /// A record.
    Record,
    /// An annotation type (`@interface`).
    AnnotationType,
    /// An enum constant.
    EnumConstant,
    /// A `catch` clause's exception variable.
    CatchParam,
    /// A try-with-resources resource variable.
    Resource,
    /// A pattern variable bound by a `switch` / `instanceof` pattern.
    PatternVar,
}

impl DefKind {
    /// The name-space this kind of definition occupies.
    pub fn namespace(self) -> Namespace {
        match self {
            DefKind::TypeParam
            | DefKind::Class
            | DefKind::Interface
            | DefKind::Enum
            | DefKind::Record
            | DefKind::AnnotationType => Namespace::Type,
            DefKind::Method | DefKind::Constructor => Namespace::Method,
            DefKind::Local
            | DefKind::Param
            | DefKind::LambdaParam
            | DefKind::Field
            | DefKind::EnumConstant
            | DefKind::CatchParam
            | DefKind::Resource
            | DefKind::PatternVar => Namespace::Value,
        }
    }
}

/// A definition: a binding introduced somewhere in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Def {
    /// This definition's identifier.
    pub id: DefId,
    /// What kind of declaration it is.
    pub kind: DefKind,
    /// The declared name.
    pub name: String,
    /// The byte range of the declaring identifier token (not the whole declaration). This is the
    /// go-to-definition target and the span an "unused binding" diagnostic points at.
    pub name_range: Range<usize>,
    /// The scope this definition is visible in.
    pub scope: ScopeId,
}
