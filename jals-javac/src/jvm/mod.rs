//! The JVM backend: a lowered method body in, a `Code` attribute out.
//!
//! [`jals_classfile`] is a codec — it keeps branch offsets verbatim and never recomputes them, so
//! the derivations a *generator* needs live here instead: label resolution, `max_stack` /
//! `max_locals`, and the `StackMapTable`.

mod asm;
mod frame;

/// Numeric promotion is a source fact, so the type lives in `crate::facts`; it is named here
/// because it is what [`Assembler::convert`] takes.
pub use crate::facts::Numeric;
pub(crate) use asm::NumericStack;
pub use asm::{AsmError, Assembler, BinOp, Branch, Compare, Label, Receiver};
