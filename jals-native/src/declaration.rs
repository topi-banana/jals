//! A package's Java API as data: the declarations an index reads, with no Java text.
//!
//! This is the other half of [`JavaSource`](crate::JavaSource). A package that publishes Java has
//! its API read by parsing that text; a package that publishes *declarations* states the same
//! facts here, and a consumer that only ever needs the shape — an index behind an editor, a
//! linter, a signature record — never parses anything. The two are alternatives on the same
//! [`JavaPackage`](crate::JavaPackage), and a package may use either: what a declaration cannot
//! carry is a method body, so a unit with bodies publishes Java, while a declaration-only unit
//! (an interface, a container nobody implemented, a class every method of which is `native`) can
//! state itself here.
//!
//! # Names are fully qualified
//!
//! A [`TypeRef::Named`] holds an FQN rather than a written spelling, because there is no file
//! whose package and imports could resolve a simple name. Nested types keep the dotted form the
//! index uses (`java.util.Map.Entry`), and type arguments are captured recursively. That is what
//! makes a declaration unit self-contained: the index needs nothing but the data.
//!
//! # What is stated is what exists
//!
//! Unlike Java source, a declaration is **explicit**: a class with no constructor listed has no
//! constructor in the model, not the default one JLS §8.8.9 would give it. The model is the
//! complete member set by construction, which is what the `Signatures` fidelity exists to express
//! for text — and why a consumer that reads an empty annotation list as "the author wrote none"
//! may do so here (the author had a place to write one).

use alloc::borrow::Cow;
use alloc::vec::Vec;

use crate::value::Provenance;

/// Which kind of type declaration a [`DeclaredType`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TypeKind {
    Class,
    Interface,
    Enum,
    /// An `@interface`.
    Annotation,
    Record,
}

impl TypeKind {
    /// A stable discriminant, so a cache key can fold a kind without naming it.
    const fn discriminant(self) -> u32 {
        match self {
            Self::Class => 0,
            Self::Interface => 1,
            Self::Enum => 2,
            Self::Annotation => 3,
            Self::Record => 4,
        }
    }
}

/// Which kind of member a [`Member`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemberKind {
    Field,
    Method,
    Constructor,
    EnumConstant,
}

impl MemberKind {
    const fn discriminant(self) -> u32 {
        match self {
            Self::Field => 0,
            Self::Method => 1,
            Self::Constructor => 2,
            Self::EnumConstant => 3,
        }
    }
}

/// A declared type, as data.
///
/// `fqn` is the fully-qualified dotted name, nested types included (`java.util.Map.Entry`), and
/// the `members` are exactly what the type declares — nothing is implied by an absence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredType {
    /// The type's fully-qualified dotted name.
    pub fqn: Cow<'static, str>,
    /// The package the type is declared in, or empty for the default package. Stated rather than
    /// derived: `a.b.C` is a class `C` in package `a.b`, while `a.Outer.Inner` is a nested type,
    /// and the two are the same shape of string — only the author knows which one it is. A stub
    /// generator needs the split; the index does not.
    pub package: Cow<'static, str>,
    /// Which kind of declaration it is.
    pub kind: TypeKind,
    /// The type's own type parameters, in declaration order.
    pub type_params: Vec<TypeParam>,
    /// The `extends` / `implements` clause types, fully qualified.
    pub supertypes: Vec<TypeRef>,
    /// The type's direct members, in declaration order.
    pub members: Vec<Member>,
    /// The annotation types written on the declaration, fully qualified.
    pub annotations: Vec<Cow<'static, str>>,
}

/// One type parameter: its name and its upper bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeParam {
    /// The parameter's name (`T`).
    pub name: Cow<'static, str>,
    /// The bounds after `extends`; empty for an unbounded parameter.
    pub bounds: Vec<TypeRef>,
}

/// A declared member.
///
/// A constructor's `name` is its class's simple name, and its `ty` is [`TypeRef::Unknown`] — the
/// same shape the source path produces, so a consumer reads both alike.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The member's simple name.
    pub name: Cow<'static, str>,
    /// Which kind of member it is.
    pub kind: MemberKind,
    /// A field's type or a method's return type. [`TypeRef::Unknown`] for a constructor and for an
    /// enum constant (whose type is its enum, which the index derives).
    pub ty: TypeRef,
    /// A method's or constructor's formal parameters, in order.
    pub params: Vec<Param>,
    /// Whether the last parameter is a varargs.
    pub varargs: bool,
    /// The member's own type parameters (`static <E> E pick(E, E)`).
    pub type_params: Vec<TypeParam>,
    /// The checked exceptions declared in a `throws` clause.
    pub throws: Vec<TypeRef>,
    /// The annotation types written on the declaration, fully qualified.
    pub annotations: Vec<Cow<'static, str>>,
    /// The modifiers **as written**; the index folds in what the owner's kind implies.
    pub modifiers: Modifiers,
}

/// A method's or constructor's formal parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// The parameter's name, or `None` for an unnamed one.
    pub name: Option<Cow<'static, str>>,
    /// The parameter's declared type.
    pub ty: TypeRef,
    /// The annotation types written on the parameter, fully qualified.
    pub annotations: Vec<Cow<'static, str>>,
}

/// The modifiers a declaration writes, before the owner's kind implies any.
///
/// [`is_default`](Self::is_default) is not a modifier `MemberModifiers` records — it exists only
/// to decide whether an interface method is implicitly `abstract` (JLS §9.4.1.1), and the source
/// path reads the same fact from the presence of a body.
/// [`is_native`](Self::is_native) is the fact that lets a declaration-only method reach the
/// compiler at all: a stub generated from the model has no body to write, so every method it
/// carries must be one the host supplies.
// Six bools, and clippy is right that six bools are usually a type wearing a disguise. Not here:
// this *is* a modifier set, each one written independently in a declaration and read independently
// by the index's folding, and a bitflags newtype would trade six named fields for a constructor
// and six accessors saying the same thing. The `is_` prefix is what makes each field read as the
// predicate it is, which is also why the names are not shortened.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    /// Declared `static`.
    pub is_static: bool,
    /// Declared `private`.
    pub is_private: bool,
    /// Declared `public`.
    pub is_public: bool,
    /// Declared `abstract`.
    pub is_abstract: bool,
    /// Declared `default`, or an interface method that carries a body.
    pub is_default: bool,
    /// Declared `native`: the body lives in the package's Rust half.
    pub is_native: bool,
}

/// A declared type, as a reference: a primitive, `void`, a named type, or a type that could not be
/// read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    /// A primitive, by keyword spelling (`"int"`), with its array levels.
    Primitive {
        /// The keyword (`int`, `boolean`, …).
        keyword: Cow<'static, str>,
        /// The array dimension count (`int[]` → 1).
        dims: u32,
    },
    /// The `void` return type.
    Void,
    /// A named reference type, by fully-qualified dotted name, with its array levels and type
    /// arguments.
    Named {
        /// The fully-qualified dotted name.
        fqn: Cow<'static, str>,
        /// The array dimension count.
        dims: u32,
        /// The type arguments, captured recursively; empty for a raw use.
        args: Vec<Self>,
    },
    /// No resolvable type — a constructor's result, or a type that could not be read.
    Unknown,
}

impl DeclaredType {
    /// Fold every fact a cache key has to observe into `provenance`.
    ///
    /// The whole model, recursively: two packages that differ only in a parameter's type or an
    /// annotation must not share a cached compile. The binding *bodies* are still the version's
    /// job — see [`JavaPackage::new`](crate::JavaPackage::new).
    pub(crate) fn describe(&self, provenance: &mut Provenance) {
        provenance.field(self.fqn.as_bytes());
        provenance.field(self.package.as_bytes());
        provenance.number(self.kind.discriminant());
        provenance.number(u32::try_from(self.type_params.len()).unwrap_or(u32::MAX));
        for param in &self.type_params {
            param.describe(provenance);
        }
        provenance.number(u32::try_from(self.supertypes.len()).unwrap_or(u32::MAX));
        for ty in &self.supertypes {
            ty.describe(provenance);
        }
        provenance.number(u32::try_from(self.members.len()).unwrap_or(u32::MAX));
        for member in &self.members {
            member.describe(provenance);
        }
        provenance.number(u32::try_from(self.annotations.len()).unwrap_or(u32::MAX));
        for annotation in &self.annotations {
            provenance.field(annotation.as_bytes());
        }
    }
}

impl Member {
    /// Fold this member's facts into `provenance`.
    fn describe(&self, provenance: &mut Provenance) {
        provenance.field(self.name.as_bytes());
        provenance.number(self.kind.discriminant());
        self.ty.describe(provenance);
        provenance.number(u32::try_from(self.params.len()).unwrap_or(u32::MAX));
        for param in &self.params {
            if let Some(name) = &param.name {
                provenance.field(name.as_bytes());
            }
            param.ty.describe(provenance);
            for annotation in &param.annotations {
                provenance.field(annotation.as_bytes());
            }
        }
        provenance.number(u32::from(self.varargs));
        provenance.number(u32::try_from(self.type_params.len()).unwrap_or(u32::MAX));
        for param in &self.type_params {
            param.describe(provenance);
        }
        provenance.number(u32::try_from(self.throws.len()).unwrap_or(u32::MAX));
        for ty in &self.throws {
            ty.describe(provenance);
        }
        provenance.number(u32::try_from(self.annotations.len()).unwrap_or(u32::MAX));
        for annotation in &self.annotations {
            provenance.field(annotation.as_bytes());
        }
        let modifiers = self.modifiers;
        provenance.number(u32::from(modifiers.is_static));
        provenance.number(u32::from(modifiers.is_private));
        provenance.number(u32::from(modifiers.is_public));
        provenance.number(u32::from(modifiers.is_abstract));
        provenance.number(u32::from(modifiers.is_default));
        provenance.number(u32::from(modifiers.is_native));
    }
}

impl TypeParam {
    fn describe(&self, provenance: &mut Provenance) {
        provenance.field(self.name.as_bytes());
        provenance.number(u32::try_from(self.bounds.len()).unwrap_or(u32::MAX));
        for bound in &self.bounds {
            bound.describe(provenance);
        }
    }
}

impl TypeRef {
    /// Fold this type reference into `provenance`.
    fn describe(&self, provenance: &mut Provenance) {
        match self {
            Self::Primitive { keyword, dims } => {
                provenance.number(0);
                provenance.field(keyword.as_bytes());
                provenance.number(*dims);
            }
            Self::Void => {
                provenance.number(1);
            }
            Self::Named { fqn, dims, args } => {
                provenance.number(2);
                provenance.field(fqn.as_bytes());
                provenance.number(*dims);
                provenance.number(u32::try_from(args.len()).unwrap_or(u32::MAX));
                for arg in args {
                    arg.describe(provenance);
                }
            }
            Self::Unknown => {
                provenance.number(3);
            }
        }
    }
}
