//! Where an assignment writes.
//!
//! Four kinds of assignable location, and one protocol over all of them, because every
//! read-modify-write in Java is the same shape: a simple assignment, a compound one (`+=`), and an
//! increment (`++`) differ only in what they compute between the read and the write.
//!
//! What makes them one thing is that the JVM splits a location into an *address* on the operand
//! stack and an instruction that consumes it. A local has no address; an instance field has its
//! receiver; an array element has the array and the index. So the number of words the address
//! occupies is the whole difference — it is what a read has to duplicate to write back to the same
//! place, and what the assigned value has to be duplicated *under* for the assignment to yield it.

use alloc::string::{String, ToString as _};

use jals_hir::Ty;
use jals_syntax::ast::{self, AstNode as _};

use crate::desc::Descriptor;
use crate::facts::Facts;
use crate::jvm::Assembler;
use crate::lower::expr::Expr;
use crate::lower::{Context, Emit, LowerError, Result};

/// An assignable location, with the part of its address that lives on the operand stack already
/// emitted.
pub(crate) enum Place {
    /// A local variable or parameter.
    Local {
        slot: u16,
        /// The declaration's field descriptor, which is the type the slot keeps across a write —
        /// see [`Assembler::store_as`].
        descriptor: String,
        ty: Ty,
    },
    /// A `static` field, which needs no receiver.
    Static {
        owner: String,
        name: String,
        descriptor: String,
        ty: Ty,
    },
    /// An instance field, whose receiver is on the stack.
    Field {
        owner: String,
        name: String,
        descriptor: String,
        ty: Ty,
    },
    /// An array element, whose array and index are on the stack.
    Element {
        /// The element's field descriptor, which selects the `*aload` / `*astore` opcode.
        element: String,
        ty: Ty,
    },
}

impl Place {
    /// Resolve `target` to a place, emitting whatever part of its address goes on the stack.
    pub(crate) fn resolve(
        target: &ast::Expr,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<Self> {
        match target {
            // `(x) = 1` is a Java program, and the parentheses are not part of the location.
            ast::Expr::Paren(paren) => {
                let inner = paren
                    .expr()
                    .ok_or(LowerError::Unsupported("an expression with no operand"))?;
                Self::resolve(&inner, context, emit)
            }
            ast::Expr::NameRef(name) => Self::name(name, context, emit),
            ast::Expr::FieldAccess(access) => Self::field(access, context, emit),
            ast::Expr::Index(index) => Self::element(index, context, emit),
            _ => Err(LowerError::Unsupported("assignment to this target")),
        }
    }

    /// A bare name: a local, or an unqualified field of the enclosing type.
    fn name(name: &ast::NameRef, context: &Context<'_>, emit: &mut Emit<'_, '_>) -> Result<Self> {
        // `this = …` is not a Java program, and `this` is the one name with nothing to resolve.
        if Facts::is_this(name.syntax()) {
            return Err(LowerError::Unsupported("an assignment to `this`"));
        }
        let written = name.syntax().text().to_string();
        let text = || LowerError::Unresolved(written.trim().into());
        let member = match context.facts().def_at(name.syntax()) {
            Some(id) => {
                if let Some(slot) = emit.slots.slot_of(id) {
                    let ty = context.typed.type_of_def(id).clone();
                    return Ok(Self::Local {
                        slot,
                        descriptor: Descriptor::descriptor_of(&ty, context.index)?.to_string(),
                        ty,
                    });
                }
                context.facts().member_of_def(id)
            }
            // Nothing in the file declared it, which an *inherited* field never is.
            None => Facts::name_token(name.syntax()).and_then(|token| {
                Expr::inherited_field(&jals_syntax::decoded_ident(&token), context)
            }),
        };
        let member = member.ok_or_else(text)?;
        let (owner, field, descriptor) = Expr::field_ref(member, context)?;
        let ty = context.index.resolved_member_ty(member);
        if context.index.member(member).modifiers.is_static {
            Ok(Self::Static {
                owner,
                name: field,
                descriptor,
                ty,
            })
        } else {
            // The receiver the source left unwritten. It is `this` for this class's own and inherited
            // fields, and the enclosing instance for an enclosing class's; either way it goes on the
            // stack now.
            Expr::load_unqualified_receiver(context.index.member(member).owner, context, emit)?;
            Ok(Self::Field {
                owner,
                name: field,
                descriptor,
                ty,
            })
        }
    }

    /// `receiver.name`.
    fn field(
        access: &ast::FieldAccess,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<Self> {
        let member = context
            .typed
            .field_target_of(Facts::span(access.syntax()))
            .ok_or_else(|| LowerError::Unresolved(access.field().unwrap_or_default()))?;
        let (owner, name, descriptor) = Expr::field_ref(member, context)?;
        let ty = context.index.resolved_member_ty(member);
        if context.index.member(member).modifiers.is_static {
            return Ok(Self::Static {
                owner,
                name,
                descriptor,
                ty,
            });
        }
        let receiver = access
            .receiver()
            .ok_or(LowerError::Unsupported("a field access with no receiver"))?;
        Expr::lower(&receiver, context, emit)?;
        Ok(Self::Field {
            owner,
            name,
            descriptor,
            ty,
        })
    }

    /// `array[index]`.
    fn element(
        index: &ast::IndexExpr,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<Self> {
        let mut parts = index.parts();
        let array = parts
            .next()
            .ok_or(LowerError::Unsupported("an index with no array"))?;
        let subscript = parts
            .next()
            .ok_or(LowerError::Unsupported("an index with no subscript"))?;

        // The element type comes from the *array's* type rather than from the index expression's,
        // because the index expression is what is being assigned to and may have no recorded type of
        // its own until the assignment gives it one.
        let Ty::Array(element) = Expr::type_of(array.syntax(), context)? else {
            return Err(LowerError::Unsupported("an index into a non-array"));
        };
        let descriptor = Descriptor::descriptor_of(&element, context.index)?.to_string();

        Expr::lower(&array, context, emit)?;
        Expr::lower_as(
            &subscript,
            &Ty::Primitive(jals_hir::Primitive::Int),
            context,
            emit,
        )?;
        Ok(Self::Element {
            element: descriptor,
            ty: *element,
        })
    }

    /// The type a value written here has to be converted to first.
    pub(crate) const fn ty(&self) -> &Ty {
        match self {
            Self::Local { ty, .. }
            | Self::Static { ty, .. }
            | Self::Field { ty, .. }
            | Self::Element { ty, .. } => ty,
        }
    }

    /// How many words of this place's address are already on the operand stack.
    pub(crate) const fn words(&self) -> u16 {
        match self {
            Self::Local { .. } | Self::Static { .. } => 0,
            Self::Field { .. } => 1,
            Self::Element { .. } => 2,
        }
    }

    /// Duplicate the address, so a read can be followed by a write to the *same* place.
    ///
    /// A compound assignment needs this: `a[i()] += 1` may only call `i()` once, so the pair the
    /// read consumes has to be a copy of the pair the write will.
    pub(crate) fn dup_address(&self, asm: &mut Assembler<'_>) -> Result<()> {
        match self {
            // Nothing on the stack to copy: the address is the slot number or the constant-pool
            // entry, and reading through it does not consume anything.
            Self::Local { .. } | Self::Static { .. } => Ok(()),
            Self::Field { .. } => asm.dup(),
            Self::Element { .. } => asm.dup_pair(),
        }
        .map_err(LowerError::from)
    }

    /// Read the current value, consuming the address.
    pub(crate) fn read(&self, asm: &mut Assembler<'_>) -> Result<()> {
        match self {
            Self::Local { slot, .. } => asm.load(*slot),
            Self::Static {
                owner,
                name,
                descriptor,
                ..
            } => asm.get_static(owner, name, descriptor),
            Self::Field {
                owner,
                name,
                descriptor,
                ..
            } => asm.get_field(owner, name, descriptor),
            Self::Element { element, .. } => asm.array_load(element),
        }
        .map_err(LowerError::from)
    }

    /// Write the value on top of the stack, consuming the address.
    ///
    /// With `keep`, the value is left behind as the assignment expression's own result — which is
    /// what makes `a = b = 1` and `println(x = 2)` work. It is duplicated *under* the address rather
    /// than saved to a temporary, which is how javac does it too.
    pub(crate) fn write(&self, asm: &mut Assembler<'_>, keep: bool) -> Result<()> {
        if keep {
            asm.dup_below(self.words())?;
        }
        match self {
            Self::Local {
                slot, descriptor, ..
            } => asm.store_as(*slot, descriptor),
            Self::Static {
                owner,
                name,
                descriptor,
                ..
            } => asm.put_static(owner, name, descriptor),
            Self::Field {
                owner,
                name,
                descriptor,
                ..
            } => asm.put_field(owner, name, descriptor),
            Self::Element { element, .. } => asm.array_store(element),
        }
        .map_err(LowerError::from)
    }
}
