//! Definitions (bindings): the things a name can resolve to.

use alloc::string::String;
use core::ops::Range;

use crate::scope::ScopeId;

/// A stable, dense identifier for a [`Def`] within one analysed file.
///
/// It indexes [`FileAnalysis::defs`](crate::FileAnalysis::defs) and is stable for that value's
/// lifetime.
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
    /// Whether a *member access* (`recv.name`), a method reference (`recv::name`), or a qualified
    /// type name (`Outer.Inner`) could denote a definition of this kind.
    ///
    /// The file-local pass binds none of those three — each needs a type it has not got — so for
    /// these kinds "no reference resolved to it" is not on its own evidence of disuse. This is what
    /// [`unused_defs`](crate::FileAnalysis::unused_defs) consults the file's *mentions* for; a
    /// local, a parameter, or a type parameter is reachable only by a simple name and needs no
    /// such second opinion.
    pub(crate) const fn is_member(self) -> bool {
        matches!(
            self,
            Self::Field
                | Self::Method
                | Self::Constructor
                | Self::EnumConstant
                | Self::Class
                | Self::Interface
                | Self::Enum
                | Self::Record
                | Self::AnnotationType
        )
    }

    /// The name-space this kind of definition occupies.
    pub const fn namespace(self) -> Namespace {
        match self {
            Self::TypeParam
            | Self::Class
            | Self::Interface
            | Self::Enum
            | Self::Record
            | Self::AnnotationType => Namespace::Type,
            Self::Method | Self::Constructor => Namespace::Method,
            Self::Local
            | Self::Param
            | Self::LambdaParam
            | Self::Field
            | Self::EnumConstant
            | Self::CatchParam
            | Self::Resource
            | Self::PatternVar => Namespace::Value,
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
    /// Whether the declaration is written `private`.
    ///
    /// The one access level worth recording here, because it is the one that makes a *file-local*
    /// answer complete: a `private` member is nameable only from within its own top-level class,
    /// which is one file, so this resolution has seen every use there can be. Every wider access
    /// level leaves the question to a project-wide pass. Always `false` for a kind that carries no
    /// modifiers at all (a local, a lambda parameter, a pattern variable).
    pub is_private: bool,
    /// Whether the declaration is `static` — the keyword as written, plus the set JLS §9.3 implies.
    ///
    /// Recorded because "is this reached through an instance?" is asked of a *definition* far more
    /// often than the CST is walked back to, and every consumer that asked it was re-deriving the
    /// same ancestor check. An interface field is `public static final` with none of those tokens
    /// spelled (JLS §9.3), so a bit reporting only what the source writes would answer `false` for
    /// the one shape whose staticness is least visible. This is the fold
    /// [`MemberModifiers::is_static`](crate::MemberModifiers::is_static) performs on the project
    /// side, so the file-local answer and the project-wide one agree rather than differing by a
    /// rule one of them remembered.
    ///
    /// The fold is deliberately **not** what every consumer wants. `naming-convention` picks its
    /// `fields` / `statics` / `constants` cell off the modifiers a declaration *writes* — an
    /// interface constant reads as a `field` there on purpose — so that rule reads the tokens and
    /// not this bit. Always `false` for a kind that carries no modifiers at all (a local, a lambda
    /// parameter, a pattern variable).
    pub is_static: bool,
    /// Whether the declaration carries at least one annotation.
    ///
    /// Recorded because an annotated declaration is routinely reached by something no source names:
    /// `@Inject` / `@Autowired` / `@Mock` write a field a framework alone assigns, and it is spelled
    /// exactly like a field nobody uses. The annotation is therefore evidence *against* reading
    /// non-use as disuse. Both shapes count: the `MODIFIERS` child most declarations park their
    /// annotations in, and the direct `ANNOTATION` children a type parameter, an enum constant, and
    /// a parameter's type-use position write instead.
    pub is_annotated: bool,
    /// The scope this definition is visible in.
    pub(crate) scope: ScopeId,
}
