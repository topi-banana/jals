//! Whether one method overrides another.
//!
//! Both backends need the answer and neither could get it from [`jals_hir`], which models no
//! override relation at all — it gets override-correct *behaviour* from the nearest-first order its
//! supertype walk produces, and never asks the question directly. So each lowering answered it
//! locally, and both answered it the same wrong way: **name plus argument count**. Two same-arity
//! overloads are indistinguishable under that rule, so `class Box implements Holder<String>` with
//! both `put(String)` and `put(int)` had whichever the walk reached first treated as the override
//! of `Holder.put(T)`.
//!
//! # The rule
//!
//! An override needs the parameter types to match *after* the supertype's type arguments are
//! substituted in: `Holder<String>` binds `T := String`, so `put(String)` overrides and `put(int)`
//! does not. The arguments are on [`Supertype::args`], whose own documentation says they are kept
//! "for generic inherited-member substitution" — this is that.
//!
//! # Three answers, because the two callers collapse them oppositely
//!
//! A `bool` would make one of them wrong, which is how both got wrong. The JVM's bridge emission
//! wants leniency — a missing bridge is an `AbstractMethodError`, a spurious one is dead code — so
//! it treats [`Overrides::Unknown`] as a yes. The wasm backend's virtual dispatch wants strictness
//! — a false positive routes a call to the wrong method, which neither `wasm-tools validate` nor
//! any type check catches — so it treats `Unknown` as a no.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use jals_hir::{
    ClassTy, DefKind, FileId, ItemId, MemberId, MemberType, Primitive, ProjectIndex, Ty,
};

/// Whether one method overrides another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Overrides {
    /// Same name and arity, and every parameter matched after substitution.
    Yes,
    /// A parameter provably differs, or the declaration shapes rule an override out.
    No,
    /// Everything checkable matched, and at least one position could not be decided.
    Unknown,
}

/// The facts a [`ProjectIndex`] answers on its own, with no file in hand.
///
/// Separate from [`Facts`](super::Facts) because the wasm backend asks them from a whole-project
/// context holding no single `TypedFile`: which classes override a method is a question about the
/// index, not about a file.
#[derive(Clone, Copy)]
pub(crate) struct Hierarchy<'a> {
    index: &'a ProjectIndex,
}

/// A type parameter's binding: the type written for it, and the file whose imports resolve that
/// spelling.
///
/// The file travels with the type because a leaf substituted in from `class Box implements
/// Holder<String>` was written in `Box`'s file while one left untouched was written in `Holder`'s.
/// Resolving both against one file is how `String` becomes the wrong `String`.
type Env = BTreeMap<(ItemId, String), (MemberType, FileId)>;

/// How far a supertype walk may go before it is abandoned. Also the cycle bound.
const DEPTH: usize = 64;

impl<'a> Hierarchy<'a> {
    /// The hierarchy facts of `index`.
    pub(crate) const fn of(index: &'a ProjectIndex) -> Self {
        Self { index }
    }

    /// The one supertype that is a *superclass* — the `extends` a class file names, and the struct a
    /// wasm layout inherits from.
    ///
    /// Three sites answered this three ways. Two asked which supertype is `kind != Interface`, one
    /// asked which is `Class | Enum | Record`. The negative form is not the same question: an
    /// `@interface` is [`DefKind::AnnotationType`], which the rest of this crate treats *as* an
    /// interface, so the negative filter would follow one as a superclass. The positive form is the
    /// rule, and it is stated here once.
    ///
    /// An `enum` counts: a constant with a body is a subclass of one, and it is the only way a
    /// declaration that is not a `class` appears here at all.
    pub(crate) fn superclass(self, item: ItemId) -> Option<ItemId> {
        self.index
            .item(item)
            .supertypes
            .iter()
            .map(|supertype| supertype.id)
            .find(|&id| {
                matches!(
                    self.index.item(id).kind,
                    DefKind::Class | DefKind::Enum | DefKind::Record
                )
            })
    }

    /// The field `name` reaches from `from`, searched up the superclass chain, nearest first.
    ///
    /// File-local resolution binds a name to a declaration it can see, and a superclass's field is
    /// not one of those — it may not even be in this file. Nearest-first is what makes a shadowing
    /// field win, and it is also what the wasm struct layout relies on: a struct holds its
    /// supertype's fields first, so the slot an inherited member lands in is the enclosing type's
    /// own.
    ///
    /// Bounded by [`DEPTH`], which neither hand-written copy was: a malformed index whose supertype
    /// chain cycles hung the compiler rather than reporting anything.
    pub(crate) fn inherited_field(self, from: ItemId, name: &str) -> Option<MemberId> {
        let mut item = from;
        for _ in 0..DEPTH {
            if let Some(member) = self
                .index
                .own_members(item)
                .iter()
                .copied()
                .find(|&member| {
                    let info = self.index.member(member);
                    info.kind == DefKind::Field && info.name == name
                })
            {
                return Some(member);
            }
            // No superclass left is the end of the search, not an error.
            item = self.superclass(item)?;
        }
        None
    }

    /// The one method a functional interface declares, or `None` when it declares none or several.
    pub(crate) fn functional_member(self, item: ItemId) -> Option<MemberId> {
        let mut methods = self
            .index
            .own_members(item)
            .iter()
            .copied()
            .filter(|&id| self.index.member(id).kind == DefKind::Method);
        let only = methods.next()?;
        methods.next().is_none().then_some(only)
    }

    /// Whether `own` overrides `inherited`.
    pub(crate) fn overrides(self, own: MemberId, inherited: MemberId) -> Overrides {
        let (a, b) = (self.index.member(own), self.index.member(inherited));
        // Shape first. A `static` method *hides* rather than overrides, and a `private` one is not
        // inherited at all (JLS §8.4.8.1) even though the member walk still lists it. Two members of
        // one owner are an overload, which is precisely the case both old rules got wrong.
        if own == inherited
            || a.kind != DefKind::Method
            || b.kind != DefKind::Method
            || a.name != b.name
            || a.params.len() != b.params.len()
            || a.modifiers.is_static
            || b.modifiers.is_static
            || b.modifiers.is_private
            || a.owner == b.owner
            || !self.index.is_subtype(a.owner, b.owner)
        {
            return Overrides::No;
        }

        let Some(env) = self.substitution(a.owner, b.owner) else {
            // `is_subtype` said yes and the declared supertypes disagree; nothing is decidable.
            return Overrides::Unknown;
        };

        let own_tys = self.index.resolved_param_tys(own);
        let mut unknown = false;
        for (position, param) in b.params.iter().enumerate() {
            let Some(own_ty) = own_tys.get(position) else {
                return Overrides::Unknown;
            };
            let (substituted, file) = Self::substitute(&param.ty, b.owner, b.file, &env);
            match Self::same_parameter(own_ty, &self.lower(&substituted, file)) {
                Overrides::No => return Overrides::No,
                Overrides::Unknown => unknown = true,
                Overrides::Yes => {}
            }
        }
        if unknown {
            Overrides::Unknown
        } else {
            Overrides::Yes
        }
    }

    /// The type arguments reaching `sup`'s type parameters, from `sub`'s point of view.
    ///
    /// Composed along the path, so `class A<T> implements B<List<T>>` carries `T` through `B`'s own
    /// parameter. A *raw* use supplies no arguments, and rather than zip what is there against what
    /// is missing, every one of that supertype's parameters is left `Unknown` — a partial binding is
    /// how a confidently wrong answer gets made.
    fn substitution(self, sub: ItemId, sup: ItemId) -> Option<Env> {
        let mut stack = alloc::vec![(sub, Env::new(), 0usize)];
        let mut seen = alloc::vec![sub];
        while let Some((current, env, depth)) = stack.pop() {
            if current == sup {
                return Some(env);
            }
            if depth >= DEPTH {
                continue;
            }
            let item = self.index.item(current);
            for supertype in &item.supertypes {
                if seen.contains(&supertype.id) && supertype.id != sup {
                    continue;
                }
                let parent = self.index.item(supertype.id);
                let mut next = env.clone();
                if supertype.args.len() == parent.type_params.len() {
                    for (declared, argument) in parent.type_params.iter().zip(&supertype.args) {
                        let (written, file) = Self::substitute(argument, current, item.file, &env);
                        next.insert((supertype.id, declared.name.clone()), (written, file));
                    }
                } else {
                    // A raw use binds nothing knowable.
                    for declared in &parent.type_params {
                        next.insert(
                            (supertype.id, declared.name.clone()),
                            (MemberType::Unknown, item.file),
                        );
                    }
                }
                seen.push(supertype.id);
                stack.push((supertype.id, next, depth + 1));
            }
        }
        None
    }

    /// `written`, with `owner`'s type parameters replaced by what `env` bound them to.
    fn substitute(
        written: &MemberType,
        owner: ItemId,
        file: FileId,
        env: &Env,
    ) -> (MemberType, FileId) {
        match written {
            MemberType::Named {
                name,
                qualified: None,
                dims,
                args,
            } if args.is_empty() => {
                // Only a type parameter is ever bound, so a plain class name simply misses. A
                // parameter of the *starting* type is deliberately unbound and stays a variable:
                // comparing it against itself is what makes an identically-generic override match.
                env.get(&(owner, name.clone())).map_or_else(
                    || (written.clone(), file),
                    |(bound, bound_file)| (Self::with_dims(bound, *dims), *bound_file),
                )
            }
            MemberType::Named {
                name,
                qualified,
                dims,
                args,
            } => {
                let substituted = args
                    .iter()
                    .map(|arg| Self::substitute(arg, owner, file, env).0)
                    .collect();
                (
                    MemberType::Named {
                        name: name.clone(),
                        qualified: qualified.clone(),
                        dims: *dims,
                        args: substituted,
                    },
                    file,
                )
            }
            other => (other.clone(), file),
        }
    }

    /// A written type lowered to a resolved one, against the file whose imports name its leaves.
    ///
    /// `jals-hir`'s own converter is crate-internal, and only the two lines this needs are rebuilt:
    /// the comparison has to happen on [`Ty`] rather than on [`MemberType`], because a `MemberType`
    /// is the *spelling* and `String` and `java.lang.String` are the same type written twice.
    fn lower(self, written: &MemberType, file: FileId) -> Ty {
        let (base, dims) = match written {
            MemberType::Void => return Ty::Void,
            MemberType::Unknown => return Ty::Unknown,
            MemberType::Primitive { keyword, dims } => {
                let Some(primitive) = Self::primitive_of(keyword) else {
                    return Ty::Unknown;
                };
                (Ty::Primitive(primitive), *dims)
            }
            MemberType::Named {
                name,
                qualified,
                dims,
                ..
            } => {
                let resolved = self
                    .index
                    .resolve_type_name(file, name, qualified.as_deref())
                    .project_id();
                let base = resolved.map_or_else(
                    || {
                        Ty::Class(ClassTy::External {
                            name: name.clone(),
                            args: Vec::new(),
                        })
                    },
                    |id| {
                        Ty::Class(ClassTy::Project {
                            id,
                            name: name.clone(),
                            args: Vec::new(),
                        })
                    },
                );
                (base, *dims)
            }
        };
        let mut ty = base;
        for _ in 0..dims {
            ty = Ty::Array(alloc::boxed::Box::new(ty));
        }
        ty
    }

    /// Whether two parameter types are the same one.
    fn same_parameter(own: &Ty, inherited: &Ty) -> Overrides {
        match (own, inherited) {
            (Ty::Array(a), Ty::Array(b)) => Self::same_parameter(a, b),
            // An array against a non-array is a different parameter whatever the element is, and a
            // primitive never instantiates a type parameter — which is what rules `put(int)` out
            // against `Holder<T>.put(T)`, the case name-and-arity could not see.
            (Ty::Array(_), _)
            | (_, Ty::Array(_))
            | (Ty::Primitive(_), Ty::Class(_))
            | (Ty::Class(_), Ty::Primitive(_)) => Overrides::No,
            (Ty::Primitive(a), Ty::Primitive(b)) => {
                if a == b {
                    Overrides::Yes
                } else {
                    Overrides::No
                }
            }
            (
                Ty::Class(ClassTy::Project { id: a, .. }),
                Ty::Class(ClassTy::Project { id: b, .. }),
            ) => {
                if a == b {
                    Overrides::Yes
                } else {
                    Overrides::No
                }
            }
            // Two names the index does not hold: compare the last segment, which is the same
            // leniency `jals-hir`'s own erasure comparison applies, for the same reason.
            (
                Ty::Class(ClassTy::External { name: a, .. }),
                Ty::Class(ClassTy::External { name: b, .. }),
            ) => {
                if Self::simple(a) == Self::simple(b) {
                    Overrides::Yes
                } else {
                    Overrides::No
                }
            }
            // An indexed type against an unindexed name, a surviving type variable (a generic
            // *method*'s own parameter, which `jals_hir::Member` does not record), or a type
            // inference never worked out: not decidable, and not a licence to claim either answer.
            _ => Overrides::Unknown,
        }
    }
}

impl Hierarchy<'_> {
    /// A `MemberType` with `extra` more array dimensions. `jals-hir`'s own constructor is private, and
    /// a constructor is honest to rebuild — it is not a rule.
    fn with_dims(written: &MemberType, extra: u32) -> MemberType {
        match written {
            MemberType::Primitive { keyword, dims } => MemberType::Primitive {
                keyword: keyword.clone(),
                dims: dims + extra,
            },
            MemberType::Named {
                name,
                qualified,
                dims,
                args,
            } => MemberType::Named {
                name: name.clone(),
                qualified: qualified.clone(),
                dims: dims + extra,
                args: args.clone(),
            },
            other => other.clone(),
        }
    }

    /// The primitive a keyword names. `jals-hir`'s own reader is crate-internal.
    fn primitive_of(keyword: &str) -> Option<Primitive> {
        Some(match keyword {
            "boolean" => Primitive::Boolean,
            "byte" => Primitive::Byte,
            "short" => Primitive::Short,
            "char" => Primitive::Char,
            "int" => Primitive::Int,
            "long" => Primitive::Long,
            "float" => Primitive::Float,
            "double" => Primitive::Double,
            _ => return None,
        })
    }

    /// A dotted name's last segment.
    fn simple(name: &str) -> &str {
        name.rsplit('.').next().unwrap_or(name)
    }
}
