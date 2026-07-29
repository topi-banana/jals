//! The mutable half of one method body being lowered.
//!
//! The assembler, the slot map, the method's declared return type, the enclosing constructs a `break`
//! or `continue` can name, and the `finally` blocks that have to run before control leaves them — in
//! one place rather than five parameters threaded through every statement and expression form.
//!
//! # A `finally` is duplicated, not called
//!
//! The JVM once had `jsr` / `ret` for exactly this, and no verifier since Java 6 accepts them. So a
//! `finally` block is emitted again at *every* way out of the region it guards: falling off the end,
//! each `return`, each `break` or `continue` that leaves it, and a catch-all handler for anything
//! thrown. Which means a `return` inside a `try` does not return — it runs every enclosing `finally`
//! first, and this is where it finds out which ones those are.
//!
//! Each inlined copy also *interrupts* the protected range it sits in. An exception thrown by a
//! `finally` must not reach the handler whose job is to run that `finally`, so the range closes before
//! the copy and a new one opens after it — which is why a guard holds a list of ranges rather than
//! one. It is what javac emits too.

use alloc::string::String;
use alloc::vec::Vec;

use jals_hir::Ty;
use jals_syntax::ast;

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
    /// Whether local slot 0 holds `this`.
    ///
    /// A `static` method's slot 0 is its *first parameter*, so lowering `this` to an `aload_0` there
    /// would silently read an argument. The assembler cannot catch it — the slot is written and its
    /// type is whatever the parameter's is.
    has_this: bool,
    /// Innermost last.
    scopes: Vec<Scope>,
    /// The `finally` blocks currently in force, innermost last.
    guards: Vec<Guard>,
    /// The `switch` expressions being lowered, innermost last: where a `yield` jumps, the type it has
    /// to produce, and how deep the `switch`'s own scope is so a `yield` knows which guards it crosses.
    yields: Vec<(Label, Ty, usize)>,
}

/// What a guarded region has to run before control leaves it.
///
/// Three constructs need the same machinery, so they share it. A `synchronized` block whose body
/// `return`s has to release the monitor first, and a `return` that skipped the `monitorexit` produced a
/// method the JVM refuses to leave — `IllegalMonitorStateException`, at run time, in a class that
/// verifies.
#[derive(Clone)]
pub(crate) enum Cleanup {
    /// A `finally` block, re-lowered at every exit.
    Finally(ast::Block),
    /// `monitorexit` on the lock held in a slot, for a `synchronized` block.
    Unlock(u16),
    /// `close()` on the resource held in a slot, for a try-with-resources.
    Close {
        slot: u16,
        /// The type declaring `close()`, and whether it is an interface.
        owner: alloc::string::String,
        interface: bool,
    },
}

/// A guarded region and what it has to run on the way out.
pub(crate) struct Guard {
    /// Re-emitted at every exit from the region.
    cleanup: Cleanup,
    /// `scopes.len()` when the region was entered, so a `break` can tell which guards it crosses.
    depth: usize,
    /// Where the protected range currently open begins, if one is.
    open: Option<Label>,
    /// The ranges closed so far. More than one because each inlined copy of the block ends the range
    /// it sits in.
    ranges: Vec<(Label, Label)>,
}

impl<'a, 'pool> Emit<'a, 'pool> {
    pub(crate) const fn new(
        asm: &'a mut Assembler<'pool>,
        slots: Slots,
        returns: Ty,
        has_this: bool,
    ) -> Self {
        Self {
            asm,
            slots,
            returns,
            has_this,
            scopes: Vec::new(),
            guards: Vec::new(),
            yields: Vec::new(),
        }
    }

    /// The method's declared return type.
    pub(crate) const fn returns(&self) -> &Ty {
        &self.returns
    }

    /// Push `this`, or report that there is none.
    pub(crate) fn load_this(&mut self) -> Result<()> {
        if !self.has_this {
            return Err(LowerError::Unsupported("`this` in a `static` method"));
        }
        Ok(self.asm.load(0)?)
    }

    /// Enter a construct a `break` (and maybe a `continue`) can leave.
    pub(crate) fn enter(&mut self, labels: Vec<String>, exit: Label, next: Option<Label>) {
        self.scopes.push(Scope { labels, exit, next });
    }

    pub(crate) fn leave(&mut self) {
        self.scopes.pop();
    }

    /// Where a `break` goes, and how deep that scope is.
    ///
    /// The depth is what a `break` needs in order to know which `finally` blocks it crosses on the
    /// way out.
    pub(crate) fn exit_of(&self, label: Option<&str>) -> Result<(Label, usize)> {
        let index = self.find(label)?;
        Ok((self.scopes[index].exit, index))
    }

    /// Where a `continue` goes, and how deep that scope is.
    ///
    /// A named `continue` has to find a *loop* with that label. Naming a labelled block instead is
    /// not a Java program, and taking its exit would turn a `continue` into a `break`.
    pub(crate) fn next_of(&self, label: Option<&str>) -> Result<(Label, usize)> {
        let index = match label {
            Some(name) => self.find(Some(name))?,
            // An unlabelled `continue` skips a `switch` on its way out, because a `switch` is a
            // `break` target that is not a loop.
            None => self
                .scopes
                .iter()
                .rposition(|scope| scope.next.is_some())
                .ok_or(LowerError::Unsupported("a `continue` outside a loop"))?,
        };
        let next = self.scopes[index]
            .next
            .ok_or(LowerError::Unsupported("a `continue` naming a non-loop"))?;
        Ok((next, index))
    }

    fn find(&self, label: Option<&str>) -> Result<usize> {
        label.map_or_else(
            || {
                self.scopes
                    .len()
                    .checked_sub(1)
                    .ok_or(LowerError::Unsupported("a `break` with nothing to leave"))
            },
            |name| {
                self.scopes
                    .iter()
                    .rposition(|scope| scope.labels.iter().any(|label| label == name))
                    .ok_or_else(|| LowerError::Unresolved(String::from(name)))
            },
        )
    }

    // --- `yield` -------------------------------------------------------------

    /// Enter a `switch` expression, whose arms `yield` a `result` to `done`.
    ///
    /// Called straight after [`enter`](Self::enter), so the `switch`'s own scope is the innermost one
    /// — which is the depth a `yield` measures its crossed guards against.
    pub(crate) fn enter_yield(&mut self, done: Label, result: Ty) {
        let depth = self.scopes.len().saturating_sub(1);
        self.yields.push((done, result, depth));
    }

    pub(crate) fn leave_yield(&mut self) {
        self.yields.pop();
    }

    /// Where a `yield` goes, what it produces, and the scope depth it leaves.
    pub(crate) fn yield_target(&self) -> Result<(Label, Ty, usize)> {
        self.yields.last().cloned().ok_or(LowerError::Unsupported(
            "a `yield` outside a `switch` expression",
        ))
    }

    // --- `finally` -----------------------------------------------------------

    /// Enter a region guarded by `cleanup`, whose protected range starts at `open`.
    pub(crate) fn guard(&mut self, cleanup: Cleanup, open: Label) {
        self.guards.push(Guard {
            cleanup,
            depth: self.scopes.len(),
            open: Some(open),
            ranges: Vec::new(),
        });
    }

    /// Leave the innermost guarded region, closing its open range at `end`, and hand back every range
    /// the handler has to protect.
    pub(crate) fn unguard(&mut self, end: Label) -> Vec<(Label, Label)> {
        let Some(mut guard) = self.guards.pop() else {
            return Vec::new();
        };
        if let Some(start) = guard.open.take() {
            guard.ranges.push((start, end));
        }
        guard.ranges
    }

    /// The guards a jump out to the scope at `depth` crosses, innermost first.
    pub(crate) fn crossed(&self, depth: usize) -> Vec<usize> {
        (0..self.guards.len())
            .rev()
            .filter(|&index| self.guards[index].depth > depth)
            .collect()
    }

    /// Every guard in force, innermost first — what a `return` crosses, whatever its depth.
    pub(crate) fn all_crossed(&self) -> Vec<usize> {
        (0..self.guards.len()).rev().collect()
    }

    /// What guard `index` runs on the way out.
    pub(crate) fn guard_cleanup(&self, index: usize) -> Option<Cleanup> {
        self.guards.get(index).map(|guard| guard.cleanup.clone())
    }

    /// Close guard `index`'s open range at `end`, because an inlined copy of its cleanup follows.
    pub(crate) fn close_range(&mut self, index: usize, end: Label) {
        if let Some(guard) = self.guards.get_mut(index)
            && let Some(start) = guard.open.take()
        {
            guard.ranges.push((start, end));
        }
    }

    /// Open a fresh range for guard `index` at `start`, because the code after an inlined copy is
    /// protected again.
    pub(crate) fn open_range(&mut self, index: usize, start: Label) {
        if let Some(guard) = self.guards.get_mut(index) {
            guard.open = Some(start);
        }
    }

    /// Take every guard at or inside `index`, so a `finally` block's own body is lowered outside the
    /// guards it belongs to — a `return` in a `finally` must not re-run it.
    pub(crate) fn split_guards(&mut self, index: usize) -> Vec<Guard> {
        self.guards.split_off(index.min(self.guards.len()))
    }

    /// Put back what [`split_guards`](Self::split_guards) took.
    pub(crate) fn rejoin_guards(&mut self, tail: Vec<Guard>) {
        self.guards.extend(tail);
    }
}
