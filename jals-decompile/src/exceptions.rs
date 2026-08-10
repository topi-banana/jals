//! Reading a `Code` attribute's exception table back as the `try` statements that wrote it.
//!
//! The table is a flat list of `(range, handler, caught type)` rows with no record of which rows
//! belong together, so the grouping has to be recovered:
//!
//! - Rows sharing a `handler_pc` are **one clause**. Several caught types aimed at one handler is
//!   how `javac` spells a multi-catch (`catch (A | B e)`); the parameter's `LocalVariableTable`
//!   type is their least upper bound, which is not what the source said.
//! - Clauses sharing a protected range belong to **one `try` statement**, in table order — which is
//!   source order, since `javac` emits the clauses as written.
//! - A `catch_type` of 0 is the catch-all a `finally` compiles to. Its rows cover the try body *and*
//!   each catch clause separately, one row per exit the finalizer was duplicated onto.
//!
//! Everything that does not fit those rules is reported as `None`, which falls the whole method back
//! to a safe body. That includes shapes that are perfectly legal bytecode but not a `try` statement
//! this crate models: a range split in two by a `return`, a `synchronized` block's unlock path, and
//! a handler whose entry does not store the caught reference.

use alloc::vec::Vec;

use jals_classfile::Instruction;

use crate::body::{JvmKind, MethodBody};
use crate::cfg::Cfg;

/// Recovering `try` statements from an exception table.
pub(crate) struct Exceptions;

/// One `try` statement, in block space.
pub(crate) struct TryRegion {
    /// The protected blocks. For a `try` with a `finally` this stops where the duplicated finalizer
    /// begins, not where the clause handlers do.
    pub body: core::ops::Range<usize>,
    /// The `catch` clauses, in source order. Empty for a bare `try`/`finally`.
    pub clauses: Vec<Clause>,
    /// The catch-all handler a `finally` compiles to.
    pub finally: Option<Finally>,
    /// The protected range in pc space — what identifies the statement while it is being assembled,
    /// since two clauses belong together exactly when they guard the same range.
    pub range: core::ops::Range<usize>,
}

/// One `catch` clause.
pub(crate) struct Clause {
    /// The handler's entry block.
    pub handler: usize,
    /// Constant-pool `Class` indices of the caught types, in source order. More than one is a
    /// multi-catch.
    pub types: Vec<u16>,
    /// The local slot the handler's entry `astore` writes the caught reference to.
    pub slot: u16,
    /// The pc just past the entry `astore`, which is where the parameter's `LocalVariableTable`
    /// entry starts — the anchor that tells sibling clauses sharing a slot apart.
    pub param_pc: usize,
}

/// The catch-all handler a `finally` compiles to.
pub(crate) struct Finally {
    /// The handler's entry block.
    pub handler: usize,
    /// The slot the entry `astore` writes the pending exception to. Synthetic — no
    /// `LocalVariableTable` entry names it, which is why it must be kept out of local hoisting.
    pub slot: u16,
    /// The finalizer as the handler spells it: instruction indices, between the entry `astore` and
    /// the trailing `aload`/`athrow` that rethrows.
    pub body: core::ops::Range<usize>,
    /// Where each duplicate of the finalizer begins, in instruction space, ascending. One per
    /// protected range that can complete normally; the handler's own copy is [`Finally::body`] and
    /// is not repeated here.
    pub copies: Vec<usize>,
}

impl Exceptions {
    /// Recover the `try` statements, or `None` when the table has a shape this crate does not model.
    ///
    /// The regions come back sorted by entry block, widest first, so a caller looking for "the `try`
    /// starting here" finds the outermost one — its body region then contains any nested `try`, which
    /// is discovered on the way in. Choosing the narrower one first would invert the nesting, and
    /// inverted nesting still visits every block exactly once, so the structurer's own guard would
    /// not notice.
    pub(crate) fn regions(cfg: &Cfg, code: &[Instruction]) -> Option<Vec<TryRegion>> {
        if cfg.handlers.is_empty() {
            return Some(Vec::new());
        }
        let groups = Self::clauses(cfg, code)?;
        let mut regions = Self::statements(cfg, code, groups)?;
        for region in &mut regions {
            // With a `finally`, the body ends where the duplicated finalizer begins, and that is
            // exactly `end_pc` — which is a block boundary, because the builder makes a catch-all's
            // `end_pc` a leader for this reason. Nothing to widen.
            if region.finally.is_some() {
                continue;
            }
            // Without one, the body runs to the first handler, which is past where protection stops:
            // a protected range excludes the trailing `goto` that leaves the statement, and that
            // `goto` is the body's own normal exit. The two coincide whenever the `goto` shares a
            // block with the code before it, and differ as soon as something else made it a leader.
            let Some(first) = region.clauses.first().map(|c| c.handler) else {
                continue;
            };
            if region.body.end > first {
                return None;
            }
            region.body.end = first;
        }
        // Entry block ascending, and at equal entry the wider body first.
        regions.sort_by(|a, b| {
            a.body
                .start
                .cmp(&b.body.start)
                .then(b.body.end.cmp(&a.body.end))
        });
        Some(regions)
    }

    /// Group the table's rows into clauses: one per `handler_pc`, carrying every type aimed at it.
    fn clauses(cfg: &Cfg, code: &[Instruction]) -> Option<Vec<Group>> {
        let mut groups: Vec<Group> = Vec::new();
        for entry in &cfg.handlers {
            let known = groups.iter().position(|g| g.handler == entry.entry);
            let Some(index) = known else {
                groups.push(Group {
                    handler: entry.entry,
                    param_pc: Self::param_pc(cfg, entry.entry)?,
                    slot: Self::entry_slot(cfg, code, entry.entry)?,
                    types: alloc::vec![entry.catch_type],
                    ranges: alloc::vec![entry.range.clone()],
                });
                continue;
            };
            let group = groups.get_mut(index)?;
            // A clause is caught once per type. Two rows that share a handler but disagree on the
            // range are not a multi-catch — that is one `try` whose protected range was split by a
            // `return`, which is a different statement shape and not modelled.
            if entry.catch_type == 0 {
                group.ranges.push(entry.range.clone());
            } else {
                if group.ranges.first() != Some(&entry.range) {
                    return None;
                }
                group.types.push(entry.catch_type);
            }
        }
        for group in &groups {
            // A clause is either typed or the catch-all, never both.
            let zeros = group.types.iter().filter(|&&t| t == 0).count();
            if zeros != 0 && zeros != group.types.len() {
                return None;
            }
            if group.types.iter().any(|&t| t != 0) && group.ranges.len() != 1 {
                return None;
            }
        }
        Some(groups)
    }

    /// The pc just past a handler's entry `astore` — where its parameter's `LocalVariableTable`
    /// entry begins.
    fn param_pc(cfg: &Cfg, handler: usize) -> Option<usize> {
        cfg.pcs.get(cfg.blocks.get(handler)?.start + 1).copied()
    }

    /// The slot a handler's entry instruction stores the caught reference into.
    ///
    /// Required, not assumed: a handler is entered with the exception on the stack and `javac`
    /// always stores it first, so an entry that is not a reference store is a shape this crate has
    /// not seen — and skipping the instruction anyway would silently drop real work.
    fn entry_slot(cfg: &Cfg, code: &[Instruction], handler: usize) -> Option<u16> {
        let start = cfg.blocks.get(handler)?.start;
        match MethodBody::store_info(code.get(start)?) {
            Some((slot, JvmKind::Reference)) => Some(slot),
            _ => None,
        }
    }

    /// Assemble the clauses into `try` statements: typed clauses sharing one range are one
    /// statement, and a catch-all joins the statement whose body its first range covers.
    fn statements(cfg: &Cfg, code: &[Instruction], groups: Vec<Group>) -> Option<Vec<TryRegion>> {
        let (finals, typed): (Vec<Group>, Vec<Group>) = groups
            .into_iter()
            .partition(|g| g.types.iter().all(|&t| t == 0));
        let mut regions: Vec<TryRegion> = Vec::new();
        for group in typed {
            let range = group.ranges.first()?.clone();
            let clause = Clause {
                handler: group.handler,
                types: group.types,
                slot: group.slot,
                param_pc: group.param_pc,
            };
            if let Some(region) = regions.iter_mut().find(|r| r.range == range) {
                region.clauses.push(clause);
            } else {
                regions.push(TryRegion {
                    body: Self::body_blocks(cfg, &range)?,
                    clauses: alloc::vec![clause],
                    finally: None,
                    range,
                });
            }
        }
        for group in finals {
            // The catch-all's *first* range is the try body it guards; the rest cover the clause
            // handlers of that same statement.
            let first = group.ranges.first()?.clone();
            let known = regions.iter().position(|r| r.range == first);
            let index = if let Some(index) = known {
                index
            } else {
                regions.push(TryRegion {
                    body: Self::body_blocks(cfg, &first)?,
                    clauses: Vec::new(),
                    finally: None,
                    range: first,
                });
                regions.len() - 1
            };
            let region = regions.get_mut(index)?;
            if region.finally.is_some() {
                return None;
            }
            region.finally = Some(Self::finalizer(cfg, code, &group)?);
        }
        Some(regions)
    }

    /// Read a catch-all handler back as a `finally` clause: the finalizer it holds, and where each
    /// duplicate of it sits.
    ///
    /// `javac` compiles `finally` by *copying* the clause onto every way out of the statement, and
    /// the exception table says where: each catch-all row covers one range that can complete
    /// normally, and its `end_pc` is where that range's copy begins. A row whose `end_pc` lands
    /// *inside* the handler's own entry instead marks a part that leaves abruptly — a clause ending
    /// in `throw` or `return` never reaches a finalizer copy of its own.
    ///
    /// Bails on anything else. In particular a row that covers its own handler — which is how a
    /// `synchronized` block's unlock path is spelled, and how a nested finalizer appears — is not
    /// modelled, and a finalizer holding a `switch`, a `return`, or a `throw` is refused outright:
    /// the fold rests on the copies being instruction-for-instruction equal, and a switch's operands
    /// are padded to a 4-byte boundary, so two equal instruction runs can still occupy different
    /// numbers of bytes.
    fn finalizer(cfg: &Cfg, code: &[Instruction], group: &Group) -> Option<Finally> {
        let entry = cfg.blocks.get(group.handler)?.start;
        let body_start = entry + 1;
        // The rethrow closes the handler: `aload <slot>; athrow`, with the slot the entry stored.
        let athrow = (body_start..code.len()).find(|&i| matches!(code[i], Instruction::Athrow))?;
        let body_end = athrow.checked_sub(1)?;
        if body_end < body_start || MethodBody::load_slot(code.get(body_end)?) != Some(group.slot) {
            return None;
        }
        // An empty finalizer has nothing to fold and nothing to render. `javac` never emits one —
        // it drops the whole handler for a `finally { }` — so refusing here costs nothing and keeps
        // every length calculation below working on a non-empty run.
        let body = body_start..body_end;
        if body.is_empty() || !Self::foldable(&code[body.clone()]) {
            return None;
        }

        let handler_pc = cfg.pcs.get(entry).copied()?;
        let past_entry = cfg.pcs.get(body_start).copied()?;
        let mut copies = Vec::new();
        for range in &group.ranges {
            if range.start == handler_pc {
                // The handler guarding its own entry `astore`. `javac` emits this routinely and it
                // marks no exit: an exception there would arrive at the very handler already being
                // entered. Discarded — but only when it covers the entry and nothing more, since a
                // range reaching further is a handler protecting real work.
                if range.end > past_entry {
                    return None;
                }
            } else if range.end <= handler_pc {
                copies.push(cfg.pcs.binary_search(&range.end).ok()?);
            } else if range.end > past_entry {
                return None;
            }
            // Otherwise the range stops inside the handler's entry: that part leaves abruptly and
            // carries no copy.
        }
        copies.sort_unstable();
        copies.dedup();
        // Every copy must repeat the handler's finalizer exactly. Branch offsets are relative, so a
        // jump *within* the finalizer is identical in every copy — and a jump that escapes it aims
        // at one shared absolute target, so its offset differs and the comparison fails, which is
        // the behaviour wanted.
        for &copy in &copies {
            let end = copy.checked_add(body.len())?;
            if end > code.len() || code.get(copy..end)? != &code[body.clone()] {
                return None;
            }
            // A `try` inside the finalizer puts exception-table rows inside every copy, and folding
            // the copies away would drop the handlers with them.
            let (lo, hi) = (cfg.pcs.get(copy).copied()?, cfg.pcs.get(end).copied()?);
            let covered = |pc: usize| (lo..hi).contains(&pc);
            if cfg.handlers.iter().any(|h| {
                covered(h.range.start)
                    || cfg
                        .blocks
                        .get(h.entry)
                        .and_then(|block| cfg.pcs.get(block.start))
                        .is_some_and(|&pc| covered(pc))
            }) {
                return None;
            }
        }
        Some(Finally {
            handler: group.handler,
            slot: group.slot,
            body,
            copies,
        })
    }

    /// Whether a finalizer's instructions may be folded: no `switch` (whose padding makes equal
    /// instruction runs occupy unequal byte counts), and no `return`/`athrow`/`jsr` (each changes
    /// what the pending exception means, which the rethrowing copy can no longer express).
    fn foldable(body: &[Instruction]) -> bool {
        use Instruction as I;
        !body.iter().any(|ins| {
            matches!(
                ins,
                I::TableSwitch { .. }
                    | I::LookupSwitch { .. }
                    | I::Return
                    | I::Ireturn
                    | I::Lreturn
                    | I::Freturn
                    | I::Dreturn
                    | I::Areturn
                    | I::Athrow
                    | I::Jsr(_)
                    | I::JsrW(_)
            )
        })
    }

    /// The blocks a protected pc range covers.
    fn body_blocks(cfg: &Cfg, range: &core::ops::Range<usize>) -> Option<core::ops::Range<usize>> {
        let handler = cfg.handlers.iter().find(|h| h.range == *range)?;
        Some(handler.try_lo..handler.try_hi)
    }
}

/// One clause under construction: the rows sharing a `handler_pc`.
struct Group {
    handler: usize,
    param_pc: usize,
    /// The caught types in table order, or a single 0 for the catch-all.
    types: Vec<u16>,
    /// The protected ranges. Exactly one for a typed clause; one per duplicated exit for a
    /// catch-all.
    ranges: Vec<core::ops::Range<usize>>,
    slot: u16,
}
