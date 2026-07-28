//! Local-variable slot allocation.
//!
//! A JVM method's locals are a flat, numbered array, not a scope tree. The receiver (when there is
//! one) takes slot 0, the declared parameters follow in order, and every local the body declares
//! takes the next free slot. A `long` or `double` takes **two** slots, which is why allocation
//! cannot simply count declarations.
//!
//! Slots are never reused across disjoint scopes here. Doing so would shrink `max_locals`, and
//! javac does it, but a reused slot changes type at the reuse point — which every stack-map frame
//! covering it then has to describe. Allocating monotonically keeps a slot's type fixed for the
//! whole method, which is the property that makes frame snapshots correct without a liveness pass.

use alloc::vec::Vec;

use jals_hir::DefId;
use jals_syntax::ast::{self, AstNode as _};

use crate::lower::Context;

/// Which local slot each definition lives in.
pub(crate) struct Slots {
    /// `(definition, slot)` pairs. A `Vec` rather than a map: a method has a handful of locals, and
    /// a linear scan over them beats hashing.
    entries: Vec<(DefId, u16)>,
    next: u16,
}

impl Slots {
    /// The slot map a method body starts with: the receiver and the declared parameters, already
    /// placed, matching what [`Assembler::new`](crate::jvm::Assembler::new) put in the frame.
    pub(crate) fn new(
        context: &Context<'_>,
        params: Option<&ast::ParamList>,
        is_static: bool,
    ) -> Self {
        let mut slots = Self {
            entries: Vec::new(),
            // `this` occupies slot 0 of every instance method and constructor.
            next: u16::from(!is_static),
        };
        for param in params.into_iter().flat_map(ast::ParamList::params) {
            let width = Self::width(context, param.syntax());
            if let Some(id) = context.def_at(param.syntax()) {
                slots.entries.push((id, slots.next));
            }
            slots.next += width;
        }
        slots
    }

    /// The slots of a constructor whose first `synthetic` parameters the source never wrote.
    ///
    /// An inner class's constructor takes the enclosing instance at slot 1, and an `enum`'s takes the
    /// constant's name and ordinal at slots 1 and 2 — before any parameter the source wrote, so every
    /// declared one shifts up. Reading them at the unshifted offsets reads a synthetic one instead.
    pub(crate) fn for_constructor(
        context: &Context<'_>,
        params: Option<&ast::ParamList>,
        synthetic: u16,
    ) -> Self {
        let mut slots = Self {
            entries: Vec::new(),
            next: 1 + synthetic,
        };
        for param in params.into_iter().flat_map(ast::ParamList::params) {
            let width = Self::width(context, param.syntax());
            if let Some(id) = context.def_at(param.syntax()) {
                slots.entries.push((id, slots.next));
            }
            slots.next += width;
        }
        slots
    }

    /// The next free slot, which is where a *synthetic* trailing parameter starts: every declared one is
    /// already accounted for, at its own width.
    pub(crate) const fn next_free(&self) -> u16 {
        self.next
    }

    /// Give `id` the next free slot (or `width` of them), and return it.
    pub(crate) fn declare(&mut self, id: DefId, width: u16) -> u16 {
        let slot = self.next;
        self.entries.push((id, slot));
        self.next += width;
        slot
    }

    /// Take `width` slots for a value the source never named.
    ///
    /// A lowering needs its own storage for things the program does not declare: a `for`-each over
    /// an array holds the array, an index, and the length, and none of the three has a `DefId` to
    /// look up. Nothing can refer to such a slot by name, so it gets no map entry — only the number.
    pub(crate) const fn declare_temporary(&mut self, width: u16) -> u16 {
        let slot = self.next;
        self.next += width;
        slot
    }

    /// The slot `id` was placed in.
    pub(crate) fn slot_of(&self, id: DefId) -> Option<u16> {
        self.entries
            .iter()
            .find(|(entry, _)| *entry == id)
            .map(|(_, slot)| *slot)
    }

    /// How many slots a declaration's type occupies: two for `long` / `double`, one otherwise.
    fn width(context: &Context<'_>, node: &jals_syntax::SyntaxNode) -> u16 {
        context
            .def_at(node)
            .map_or(1, |id| Self::ty_width(context.inference.type_of_def(id)))
    }

    /// How many slots a value of `ty` occupies.
    /// How many local slots a value of the type this *descriptor* names occupies.
    ///
    /// A `long` and a `double` take two, everything else one — the one place the JVM's local array is
    /// not one entry per value, and reading a parameter at the wrong offset reads the previous one's
    /// high half.
    pub(crate) const fn descriptor_width(descriptor: &str) -> u16 {
        match descriptor.as_bytes() {
            [b'J' | b'D'] => 2,
            _ => 1,
        }
    }

    pub(crate) const fn ty_width(ty: &jals_hir::Ty) -> u16 {
        match ty {
            jals_hir::Ty::Primitive(jals_hir::Primitive::Long | jals_hir::Primitive::Double) => 2,
            _ => 1,
        }
    }
}
