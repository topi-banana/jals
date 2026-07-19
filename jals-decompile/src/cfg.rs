//! Control-flow graph construction: splitting a method's instruction stream into basic blocks.
//!
//! Instruction offsets are reconstructed with [`Instruction::encoded_len`] so each branch's relative
//! offset resolves to a target instruction, then the code is cut at leaders (block entries) into
//! [`Block`]s carrying their terminator and successor block indices. The builder bails (`None`) on
//! anything the M2 structurer does not yet model — a `switch`, `jsr`/`ret`, or a branch to an offset
//! that is not an instruction boundary — so the caller falls back to a safe body.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use jals_classfile::{Instruction, WideInstruction};
use jals_exec::Yielder;

/// A method's basic blocks, in instruction order (block 0 is the entry).
pub(crate) struct Cfg {
    pub blocks: Vec<Block>,
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
    /// A `return` (any flavour) — a method exit.
    Ret,
    /// An `athrow` — a method exit.
    Throw,
}

impl Block {
    /// The instruction range the value-level simulator should replay: the whole block, except an
    /// explicit `goto` / conditional-branch terminator (which the structurer interprets itself).
    pub const fn body(&self) -> core::ops::Range<usize> {
        match self.term {
            Term::Goto(_) | Term::Branch { .. } => self.start..self.end - 1,
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
    /// A method exit that returns.
    Ret,
    /// A method exit that throws.
    Throw,
    /// Straight-line: control falls to the next instruction.
    Normal,
    /// Not modelled by the M2 structurer (`switch`, `jsr`/`ret`).
    Unsupported,
}

impl Cfg {
    /// Build the CFG for a method's instructions, or `None` if it uses a construct M2 does not model.
    pub(crate) async fn build(code: &[Instruction]) -> Option<Self> {
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
                I::TableSwitch { .. }
                | I::LookupSwitch { .. }
                | I::Jsr(_)
                | I::JsrW(_)
                | I::Ret(_)
                | I::Wide(WideInstruction::Ret(_)) => Flow::Unsupported,
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
        let target = |i: usize, offset: i32| -> Option<usize> {
            let dest = i64::try_from(pcs[i]).ok()? + i64::from(offset);
            pcs.binary_search(&usize::try_from(dest).ok()?).ok()
        };

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
                Flow::Ret | Flow::Throw => {
                    if i + 1 < code.len() {
                        leaders.insert(i + 1);
                    }
                }
                Flow::Unsupported => return None,
                Flow::Normal => {}
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
                Flow::Ret => Term::Ret,
                Flow::Throw => Term::Throw,
                Flow::Normal => Term::Fall(block_of(end)?),
                Flow::Unsupported => return None,
            };
            blocks.push(Block { start, end, term });
        }
        Some(Self { blocks })
    }
}
