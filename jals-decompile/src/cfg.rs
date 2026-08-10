//! Control-flow graph construction: splitting a method's instruction stream into basic blocks.
//!
//! Instruction offsets are reconstructed with [`Instruction::encoded_len`] so each branch's relative
//! offset resolves to a target instruction, then the code is cut at leaders (block entries) into
//! [`Block`]s carrying their terminator and successor block indices. The builder bails (`None`) on
//! anything the structurer does not yet model — `jsr`/`ret`, a malformed or implausibly large switch
//! table, or a branch to an offset that is not an instruction boundary — so the caller falls back to
//! a safe body.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use jals_classfile::{ExceptionTableEntry, Instruction, WideInstruction};
use jals_exec::Yielder;

/// The most `case` labels a switch may carry before the CFG declines to model it. `tableswitch`
/// encodes `low..=high` densely and the classfile reader admits up to `u32::MAX` entries, so
/// expanding a hostile table would allocate unboundedly; a real switch is orders of magnitude
/// smaller.
const MAX_SWITCH_CASES: usize = 1 << 16;

/// A method's basic blocks, in instruction order (block 0 is the entry).
pub(crate) struct Cfg {
    pub blocks: Vec<Block>,
    /// Byte offset (pc) of each instruction, parallel to the method's `code` and strictly
    /// increasing. Built here to resolve branch targets; kept so the structurer can look an
    /// instruction up in the `LineNumberTable`, which is keyed by pc rather than by index.
    pub pcs: Vec<usize>,
    /// The exception table resolved into block space, in table order.
    ///
    /// Deliberately a side table rather than [`Term`] variants. An exception leaves from *any*
    /// instruction in the protected range, not from the block's terminator, so it has no place in a
    /// vocabulary built on "what the last instruction does"; and adding the edges would show up in
    /// [`Structurer::loop_latch`](crate::body), which counts back-edges to find a loop's single
    /// latch, as extra edges that are not source control flow at all.
    pub handlers: Vec<Handler>,
}

/// One exception-table entry, resolved into block space.
pub(crate) struct Handler {
    /// First block of the protected range.
    pub try_lo: usize,
    /// One past the block holding the last protected instruction.
    ///
    /// Not `block_of(end_pc)`: for a typed handler `end_pc` commonly falls mid-expression (before
    /// the `return` that leaves the block), which is no block boundary at all.
    pub try_hi: usize,
    /// The handler's entry block.
    pub entry: usize,
    /// `Class` index of the caught type, or 0 for the catch-all a `finally` compiles to.
    pub catch_type: u16,
    /// The protected range in pc space, kept verbatim. Clause grouping and `finally` duplicate
    /// detection are decided here, where two entries are comparable even when they land in the same
    /// block.
    pub range: core::ops::Range<usize>,
}

/// A basic block: a maximal run of instructions `code[start..end]` with a single entry, ending in a
/// [`Term`] that names its successors (as block indices).
pub(crate) struct Block {
    pub start: usize,
    pub end: usize,
    pub term: Term,
}

/// A block's terminator and its successor block indices.
pub(crate) enum Term {
    /// Falls straight through to the next block (its last instruction is not a jump).
    Fall(usize),
    /// An unconditional `goto` to a block.
    Goto(usize),
    /// A conditional branch: the terminating instruction index, then the taken and fall-through
    /// block indices.
    Branch {
        instr: usize,
        taken: usize,
        fallthrough: usize,
    },
    /// A multi-way `tableswitch` / `lookupswitch`: the `default` block, then one `(key, block)`
    /// pair per `case` label.
    ///
    /// Deliberately ungrouped. Which keys are droppable — a `tableswitch`'s gap keys (which encode
    /// "no label", pointing at `default`), or the keys of a `default`-less switch (which point at
    /// the join) — is only decidable once the structurer has derived the switch's join block, so
    /// grouping by target belongs there rather than here.
    Switch {
        default: usize,
        cases: Vec<(i32, usize)>,
    },
    /// A `return` (any flavour) — a method exit.
    Ret,
    /// An `athrow` — a method exit.
    Throw,
}

impl Block {
    /// The instruction range the value-level simulator should replay: the whole block, except an
    /// explicit `goto` / conditional-branch / `switch` terminator (which the structurer interprets
    /// itself, reading the operands the terminator would have popped off the leftover stack).
    pub const fn body(&self) -> core::ops::Range<usize> {
        match self.term {
            Term::Goto(_) | Term::Branch { .. } | Term::Switch { .. } => self.start..self.end - 1,
            Term::Fall(_) | Term::Ret | Term::Throw => self.start..self.end,
        }
    }
}

/// How an instruction affects control flow.
enum Flow {
    /// A conditional branch with its relative offset.
    Cond(i32),
    /// An unconditional `goto` with its relative offset.
    Goto(i32),
    /// A `tableswitch` / `lookupswitch`, normalized to the `default`'s relative offset plus one
    /// `(key, relative offset)` pair per label — so the structurer never sees the two encodings.
    Switch {
        default: i32,
        cases: Vec<(i32, i32)>,
    },
    /// A method exit that returns.
    Ret,
    /// A method exit that throws.
    Throw,
    /// Straight-line: control falls to the next instruction.
    Normal,
    /// Not modelled by the structurer (`jsr`/`ret`), or a switch table that is malformed or too
    /// large to expand ([`MAX_SWITCH_CASES`]).
    Unsupported,
}

impl Cfg {
    /// Build the CFG for a method's instructions, or `None` if it uses a construct not modelled.
    ///
    /// The exception table contributes leaders but no edges (see [`Cfg::handlers`]). Which of its
    /// offsets may be cut at is not uniform:
    ///
    /// - `start_pc` and `handler_pc` are always statement boundaries, so both are leaders.
    /// - `end_pc` is a leader only for the **catch-all**, where it is where `javac` starts the
    ///   duplicated finalizer — a boundary the source itself has. A typed handler's `end_pc` marks
    ///   only where protection stops, which is routinely mid-expression: `try { return f(); }`
    ///   ends its range on the `return`, with `f()`'s result already on the stack. Cutting there
    ///   would leave a block that falls through with a live operand, which the structurer rejects.
    pub(crate) async fn build(
        code: &[Instruction],
        exception_table: &[ExceptionTableEntry],
    ) -> Option<Self> {
        /// How an instruction affects control flow.
        fn flow(ins: &Instruction) -> Flow {
            use Instruction as I;
            match ins {
                I::Ifeq(o)
                | I::Ifne(o)
                | I::Iflt(o)
                | I::Ifge(o)
                | I::Ifgt(o)
                | I::Ifle(o)
                | I::IfIcmpeq(o)
                | I::IfIcmpne(o)
                | I::IfIcmplt(o)
                | I::IfIcmpge(o)
                | I::IfIcmpgt(o)
                | I::IfIcmple(o)
                | I::IfAcmpeq(o)
                | I::IfAcmpne(o)
                | I::IfNull(o)
                | I::IfNonNull(o) => Flow::Cond(i32::from(*o)),
                I::Goto(o) => Flow::Goto(i32::from(*o)),
                I::GotoW(o) => Flow::Goto(*o),
                I::Return | I::Ireturn | I::Lreturn | I::Freturn | I::Dreturn | I::Areturn => {
                    Flow::Ret
                }
                I::Athrow => Flow::Throw,
                // A dense table: the keys are `low..=high`, one per offset. `high - low + 1`
                // overflows `i32` on a hostile table, so the count is checked in `i64`.
                I::TableSwitch {
                    default,
                    low,
                    high,
                    offsets,
                } => {
                    let Ok(count) = usize::try_from(i64::from(*high) - i64::from(*low) + 1) else {
                        return Flow::Unsupported;
                    };
                    if count != offsets.len() || count > MAX_SWITCH_CASES {
                        return Flow::Unsupported;
                    }
                    Flow::Switch {
                        default: *default,
                        cases: (*low..=*high).zip(offsets.iter().copied()).collect(),
                    }
                }
                // A sparse table: the keys are given.
                I::LookupSwitch { default, pairs } => {
                    if pairs.len() > MAX_SWITCH_CASES {
                        return Flow::Unsupported;
                    }
                    // A duplicate key would render as a duplicate `case` label, which is not valid
                    // Java, so decline rather than emit one.
                    let mut keys = BTreeSet::new();
                    if !pairs.iter().all(|&(key, _)| keys.insert(key)) {
                        return Flow::Unsupported;
                    }
                    Flow::Switch {
                        default: *default,
                        cases: pairs.clone(),
                    }
                }
                I::Jsr(_) | I::JsrW(_) | I::Ret(_) | I::Wide(WideInstruction::Ret(_)) => {
                    Flow::Unsupported
                }
                _ => Flow::Normal,
            }
        }

        if code.is_empty() {
            return None;
        }
        // Amortized cooperative point shared by the per-instruction and per-block loops below.
        let mut yielder = Yielder::new();
        // Byte offset (pc) of each instruction. Strictly increasing, so a branch target resolves to
        // its instruction index by binary search — no reverse map to build.
        let mut pcs = Vec::with_capacity(code.len());
        let mut pc = 0usize;
        for ins in code {
            yielder.tick().await;
            pcs.push(pc);
            pc += ins.encoded_len(pc);
        }
        let code_len = pc;
        let target = |i: usize, offset: i32| -> Option<usize> {
            let dest = i64::try_from(pcs[i]).ok()? + i64::from(offset);
            pcs.binary_search(&usize::try_from(dest).ok()?).ok()
        };
        // The instruction starting exactly at `pc`, or `None` if `pc` is not a boundary.
        let at_pc = |pc: usize| -> Option<usize> { pcs.binary_search(&pc).ok() };

        // Leaders: the entry, every branch target, and the instruction after a branch / exit.
        let mut leaders = BTreeSet::new();
        leaders.insert(0usize);
        for (i, ins) in code.iter().enumerate() {
            yielder.tick().await;
            match flow(ins) {
                Flow::Cond(o) | Flow::Goto(o) => {
                    leaders.insert(target(i, o)?);
                    if i + 1 < code.len() {
                        leaders.insert(i + 1);
                    }
                }
                Flow::Switch { default, cases } => {
                    leaders.insert(target(i, default)?);
                    for (_, offset) in cases {
                        leaders.insert(target(i, offset)?);
                    }
                    if i + 1 < code.len() {
                        leaders.insert(i + 1);
                    }
                }
                Flow::Ret | Flow::Throw => {
                    if i + 1 < code.len() {
                        leaders.insert(i + 1);
                    }
                }
                Flow::Unsupported => return None,
                Flow::Normal => {}
            }
        }

        // Exception-table leaders (see the doc comment for why `end_pc` is conditional).
        for entry in exception_table {
            yielder.tick().await;
            let (start, end) = (usize::from(entry.start_pc()), usize::from(entry.end_pc()));
            if start >= end || end > code_len {
                return None;
            }
            leaders.insert(at_pc(start)?);
            leaders.insert(at_pc(usize::from(entry.handler_pc()))?);
            if entry.catch_type() == 0 && end < code_len {
                leaders.insert(at_pc(end)?);
            }
        }

        // Cut into blocks at the leaders. `leaders` is sorted, so a block start resolves to its block
        // index by binary search — the successor lookups below need no side map.
        let leaders: Vec<usize> = leaders.into_iter().collect();
        let block_of = |start: usize| -> Option<usize> { leaders.binary_search(&start).ok() };
        let mut blocks = Vec::with_capacity(leaders.len());
        for (b, &start) in leaders.iter().enumerate() {
            yielder.tick().await;
            let end = leaders.get(b + 1).copied().unwrap_or(code.len());
            let last = end - 1;
            let term = match flow(&code[last]) {
                Flow::Cond(o) => Term::Branch {
                    instr: last,
                    taken: block_of(target(last, o)?)?,
                    fallthrough: block_of(end)?,
                },
                Flow::Goto(o) => Term::Goto(block_of(target(last, o)?)?),
                Flow::Switch { default, cases } => Term::Switch {
                    default: block_of(target(last, default)?)?,
                    cases: cases
                        .into_iter()
                        .map(|(key, offset)| Some((key, block_of(target(last, offset)?)?)))
                        .collect::<Option<Vec<_>>>()?,
                },
                Flow::Ret => Term::Ret,
                Flow::Throw => Term::Throw,
                Flow::Normal => Term::Fall(block_of(end)?),
                Flow::Unsupported => return None,
            };
            blocks.push(Block { start, end, term });
        }

        // Resolve the exception table into block space. `try_hi` is derived from the last *protected
        // instruction* rather than from `end_pc` itself, since a typed handler's `end_pc` need not be
        // an instruction boundary — and `end_pc == code_len` is never one.
        let containing = |i: usize| -> usize { leaders.partition_point(|&l| l <= i) - 1 };
        let mut handlers = Vec::with_capacity(exception_table.len());
        for entry in exception_table {
            yielder.tick().await;
            let (start, end) = (usize::from(entry.start_pc()), usize::from(entry.end_pc()));
            let last = pcs.partition_point(|&pc| pc < end).checked_sub(1)?;
            handlers.push(Handler {
                try_lo: block_of(at_pc(start)?)?,
                try_hi: containing(last) + 1,
                entry: block_of(at_pc(usize::from(entry.handler_pc()))?)?,
                catch_type: entry.catch_type(),
                range: start..end,
            });
        }
        Some(Self {
            blocks,
            pcs,
            handlers,
        })
    }
}
