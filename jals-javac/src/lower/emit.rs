//! The mutable half of one method body being lowered.
//!
//! The assembler, the slot map, the method's declared return type, and the enclosing constructs a
//! `break` or `continue` can name — in one place rather than four parameters threaded through every
//! statement and expression form. The list only grows: `finally` will hang off the same scopes.

use alloc::string::String;
use alloc::vec::Vec;

use jals_hir::Ty;

use crate::jvm::{Assembler, Label};
use crate::lower::slots::Slots;
use crate::lower::{LowerError, Result};

/// One enclosing construct a `break` or `continue` can leave.
struct Scope {
    /// Every source label naming it. More than one because `a: b: for (…)` is legal and both names
    /// leave the same loop; empty for an unlabelled construct, which an unlabelled `break` still
    /// finds by being the innermost.
    labels: Vec<String>,
    /// Where a `break` goes.
    exit: Label,
    /// Where a `continue` goes.
    ///
    /// `None` for a construct `continue` cannot name: a `switch` and a labelled block are both
    /// `break` targets without being loops, and JLS §14.16 restricts `continue` to a loop.
    next: Option<Label>,
}

/// The mutable state one method body is lowered into.
pub(crate) struct Emit<'a, 'pool> {
    pub(crate) asm: &'a mut Assembler<'pool>,
    pub(crate) slots: Slots,
    /// The declared return type, which is what a `return` converts its value to.
    ///
    /// Read from the declaration rather than from the operand stack. Picking the return opcode from
    /// whatever the expression happened to leave behind emitted `ireturn` for
    /// `long f() { return 1; }` — a class file that verifies against the wrong descriptor.
    returns: Ty,
    /// Innermost last.
    scopes: Vec<Scope>,
}

impl<'a, 'pool> Emit<'a, 'pool> {
    pub(crate) const fn new(asm: &'a mut Assembler<'pool>, slots: Slots, returns: Ty) -> Self {
        Self {
            asm,
            slots,
            returns,
            scopes: Vec::new(),
        }
    }

    /// The method's declared return type.
    pub(crate) const fn returns(&self) -> &Ty {
        &self.returns
    }

    /// Enter a construct a `break` (and maybe a `continue`) can leave.
    pub(crate) fn enter(&mut self, labels: Vec<String>, exit: Label, next: Option<Label>) {
        self.scopes.push(Scope { labels, exit, next });
    }

    pub(crate) fn leave(&mut self) {
        self.scopes.pop();
    }

    /// Where a `break` goes: the scope `label` names, or the innermost one.
    pub(crate) fn exit_of(&self, label: Option<&str>) -> Result<Label> {
        self.find(label).map(|scope| scope.exit)
    }

    /// Where a `continue` goes.
    ///
    /// A named `continue` has to find a *loop* with that label. Naming a labelled block instead is
    /// not a Java program, and taking its exit would turn a `continue` into a `break`.
    pub(crate) fn next_of(&self, label: Option<&str>) -> Result<Label> {
        match label {
            Some(name) => self
                .find(Some(name))?
                .next
                .ok_or(LowerError::Unsupported("a `continue` naming a non-loop")),
            // An unlabelled `continue` skips a `switch` on its way out, because a `switch` is a
            // `break` target that is not a loop.
            None => self
                .scopes
                .iter()
                .rev()
                .find_map(|scope| scope.next)
                .ok_or(LowerError::Unsupported("a `continue` outside a loop")),
        }
    }

    fn find(&self, label: Option<&str>) -> Result<&Scope> {
        label.map_or_else(
            || {
                self.scopes
                    .last()
                    .ok_or(LowerError::Unsupported("a `break` with nothing to leave"))
            },
            |name| {
                self.scopes
                    .iter()
                    .rev()
                    .find(|scope| scope.labels.iter().any(|label| label == name))
                    .ok_or_else(|| LowerError::Unresolved(String::from(name)))
            },
        )
    }
}
