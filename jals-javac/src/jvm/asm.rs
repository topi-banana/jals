//! A label-based assembler for one method body.
//!
//! A code generator emits *labels*, not byte offsets. This module turns a label-based instruction
//! stream into a `Code` attribute, which means owning the three derivations
//! [`jals_classfile`](jals_classfile) deliberately does not: branch-offset resolution (that crate
//! keeps offsets verbatim and never recomputes them), the `max_stack` / `max_locals` frame sizes,
//! and the `StackMapTable`.
//!
//! # Branch widening is a fixpoint
//!
//! `goto` and the conditional branches carry a signed 16-bit offset. A jump that does not fit has
//! to widen — but widening makes the instruction longer, which moves every instruction after it,
//! which can push a second jump out of range. So the resolver re-measures until nothing more needs
//! widening.
//!
//! What terminates it is *not* that instruction lengths only grow. A `switch` aligns its operands
//! to a four-byte boundary measured from the start of the method, so moving one can make it
//! **shorter**, and the total code length is not monotonic. What terminates it is that the *set* of
//! widened branches only grows and is finite: a branch is never narrowed once widened, so each pass
//! either adds at least one to that set or changes nothing and is the last.
//!
//! `goto` widens to `goto_w`. The conditionals have no wide form, so they invert and jump around a
//! `goto_w` instead: `if_icmplt far` becomes `if_icmpge past; goto_w far; past:`. A `switch` needs
//! no widening at all — both its forms carry 32-bit offsets.
//!
//! # The stack map is snapshotted, not inferred
//!
//! Class files at major version 50 and above must carry a `StackMapTable`, and computing one from
//! finished bytecode means a dataflow analysis over the control-flow graph. A generator never needs
//! that: it knows the abstract state at every instruction it emits, so binding a label records the
//! state then and there. Every frame is written as a `full_frame` (JVMS §4.7.4 tag 255), which can
//! express any state and removes the same/chop/append/delta selection problem entirely.

use alloc::string::ToString as _;
use alloc::vec::Vec;

use jals_classfile::{
    Attribute, AttributeBody, BaseType, CodeAttribute, ConstantPool, ExceptionTableEntry,
    FieldType, Instruction, MethodDescriptor, ReturnType, StackMapFrame, VerificationType,
    WideInstruction,
};

use crate::jvm::frame::{Slot, State};

/// A jump target within one method body.
///
/// Created by [`Assembler::label`] and given a position by [`Assembler::bind`]. Labels are scoped
/// to the assembler that made them; using one with another assembler is a caller error the
/// resolver reports as [`AsmError::UnboundLabel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Label(usize);

/// What can go wrong while assembling. Every variant is a generator bug rather than bad user
/// input: this crate assumes a well-formed program, so a malformed emission is a defect here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsmError {
    /// The constant pool is full, or a `Utf8` was longer than its `u16` length field allows.
    PoolFull,
    /// A descriptor handed to the assembler did not parse.
    BadDescriptor,
    /// An instruction wanted more operands than the stack held.
    StackUnderflow,
    /// An instruction was given an operand of the wrong kind (an `int` where a reference was due).
    TypeMismatch,
    /// A local slot was read before anything wrote it.
    UnwrittenLocal,
    /// A label was jumped to but never bound.
    UnboundLabel,
    /// A label was bound where control cannot arrive and nothing jumps to it.
    UnreachableLabel,
    /// Two paths reach one label with states that cannot be merged.
    IncompatibleFrame,
    /// Code was emitted after an unconditional transfer, with no label bound in between.
    Unreachable,
    /// Two arms of one `switch` claimed the same key.
    DuplicateCase,
    /// The body exceeded a `u16` frame size or the 64 KiB `Code` limit.
    TooLarge,
}

impl core::fmt::Display for AsmError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::PoolFull => "the constant pool is full",
            Self::BadDescriptor => "a descriptor did not parse",
            Self::StackUnderflow => "the operand stack was too shallow for an instruction",
            Self::TypeMismatch => "an instruction was given an operand of the wrong kind",
            Self::UnwrittenLocal => "a local slot was read before it was written",
            Self::UnboundLabel => "a label was jumped to but never bound",
            Self::UnreachableLabel => "a label was bound where control cannot arrive",
            Self::IncompatibleFrame => "two paths reach one label with incompatible states",
            Self::Unreachable => "code was emitted after an unconditional transfer",
            Self::DuplicateCase => "two `switch` arms claimed the same key",
            Self::TooLarge => "the method body exceeded a class-file limit",
        })
    }
}

impl core::error::Error for AsmError {}

type Result<T> = core::result::Result<T, AsmError>;

/// A conditional or unconditional branch, named by what it tests rather than by its opcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Branch {
    /// Always taken.
    Always,
    /// The `int` on top compares against zero.
    IntZero(Compare),
    /// The two `int`s on top compare against each other.
    IntCmp(Compare),
    /// The two references on top are (not) the same object.
    RefSame(bool),
    /// The reference on top is (not) `null`.
    RefNull(bool),
}

/// Which way a comparison has to go for its branch to be taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compare {
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `>=`
    Ge,
    /// `>`
    Gt,
    /// `<=`
    Le,
}

impl Compare {
    /// The comparison that is taken exactly when this one is not.
    const fn inverse(self) -> Self {
        match self {
            Self::Eq => Self::Ne,
            Self::Ne => Self::Eq,
            Self::Lt => Self::Ge,
            Self::Ge => Self::Lt,
            Self::Gt => Self::Le,
            Self::Le => Self::Gt,
        }
    }
}

impl Branch {
    /// The branch taken exactly when this one is not — what widening needs in order to jump around
    /// a `goto_w`. `Always` has no inverse and never needs one: `goto` widens directly.
    const fn inverse(self) -> Option<Self> {
        Some(match self {
            Self::Always => return None,
            Self::IntZero(compare) => Self::IntZero(compare.inverse()),
            Self::IntCmp(compare) => Self::IntCmp(compare.inverse()),
            Self::RefSame(same) => Self::RefSame(!same),
            Self::RefNull(null) => Self::RefNull(!null),
        })
    }

    /// This branch as an instruction jumping `offset` bytes from its own position.
    const fn instruction(self, offset: i16) -> Instruction {
        match self {
            Self::Always => Instruction::Goto(offset),
            Self::IntZero(Compare::Eq) => Instruction::Ifeq(offset),
            Self::IntZero(Compare::Ne) => Instruction::Ifne(offset),
            Self::IntZero(Compare::Lt) => Instruction::Iflt(offset),
            Self::IntZero(Compare::Ge) => Instruction::Ifge(offset),
            Self::IntZero(Compare::Gt) => Instruction::Ifgt(offset),
            Self::IntZero(Compare::Le) => Instruction::Ifle(offset),
            Self::IntCmp(Compare::Eq) => Instruction::IfIcmpeq(offset),
            Self::IntCmp(Compare::Ne) => Instruction::IfIcmpne(offset),
            Self::IntCmp(Compare::Lt) => Instruction::IfIcmplt(offset),
            Self::IntCmp(Compare::Ge) => Instruction::IfIcmpge(offset),
            Self::IntCmp(Compare::Gt) => Instruction::IfIcmpgt(offset),
            Self::IntCmp(Compare::Le) => Instruction::IfIcmple(offset),
            Self::RefSame(true) => Instruction::IfAcmpeq(offset),
            Self::RefSame(false) => Instruction::IfAcmpne(offset),
            Self::RefNull(true) => Instruction::IfNull(offset),
            Self::RefNull(false) => Instruction::IfNonNull(offset),
        }
    }

    /// How many stack values the branch consumes, and what each has to be.
    ///
    /// `if_icmp*` and `if*` compare **`int`s** — not "anything that is not a reference". A `long`
    /// on the stack is not a reference either, and `if_icmpeq` over two of them is a class the
    /// verifier rejects with *"Type `long_2nd` is not assignable to integer"*. The reference forms
    /// are expressed as [`Null`](VerificationType::Null), which
    /// [`compatible`](Assembler::compatible) already reads as "any reference".
    const fn operands(self) -> (usize, Option<VerificationType>) {
        match self {
            Self::Always => (0, None),
            Self::IntZero(_) => (1, Some(VerificationType::Integer)),
            Self::IntCmp(_) => (2, Some(VerificationType::Integer)),
            Self::RefSame(_) => (2, Some(VerificationType::Null)),
            Self::RefNull(_) => (1, Some(VerificationType::Null)),
        }
    }
}

/// A binary arithmetic, bitwise, or shift operator, resolved to an opcode by the type it is applied
/// to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Rem,
    /// `&`
    And,
    /// `|`
    Or,
    /// `^`
    Xor,
    /// `<<`
    Shl,
    /// `>>`, the arithmetic shift that keeps the sign bit.
    Shr,
    /// `>>>`, the logical shift that does not.
    Ushr,
}

impl BinOp {
    /// Whether this operator's right operand is an `int` shift count rather than a second value of
    /// the left operand's own type.
    ///
    /// `lshl` shifts a `long` by an **`int`** (JVMS §6.5), which is the one place the JVM's binary
    /// operators are not symmetric — and the reason `ladd`'s "two of the same" rule cannot simply be
    /// applied to every entry in this enum.
    #[must_use]
    pub(crate) const fn is_shift(self) -> bool {
        matches!(self, Self::Shl | Self::Shr | Self::Ushr)
    }
}

/// A primitive type as a *conversion* names it.
///
/// Deliberately narrower than a [`VerificationType`]: `byte`, `char`, and `short` all live on the
/// operand stack as `Integer` (JVMS §2.11.1), so a conversion between two of them changes the value
/// without changing the stack type at all. A `(VerificationType, VerificationType)` pair could not
/// say which of `i2b` / `i2c` / `i2s` was meant, because both sides would read `Integer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Numeric {
    /// `byte`
    Byte,
    /// `short`
    Short,
    /// `char`
    Char,
    /// `int`
    Int,
    /// `long`
    Long,
    /// `float`
    Float,
    /// `double`
    Double,
}

impl Numeric {
    /// The verification type a value of this type has on the operand stack.
    #[must_use]
    pub(crate) const fn stack(self) -> VerificationType {
        match self {
            Self::Long => VerificationType::Long,
            Self::Float => VerificationType::Float,
            Self::Double => VerificationType::Double,
            // Every integral type narrower than `long` computes as `int`.
            Self::Byte | Self::Short | Self::Char | Self::Int => VerificationType::Integer,
        }
    }
}

/// What local slot 0 holds when a method body starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Receiver<'a> {
    /// A `static` method: slot 0 is the first parameter.
    Static,
    /// An instance method on the named class: slot 0 is an initialised `this`.
    Instance(&'a str),
    /// A constructor of the named class: slot 0 is `uninitializedThis` until this body's
    /// `invokespecial <init>` runs, and a `putfield` or a method call on it before then is exactly
    /// what the verifier is there to reject.
    Constructor(&'a str),
}

/// One entry of the item stream: what the assembler records before offsets are known.
enum Item {
    /// An instruction whose encoding is already final.
    Fixed(Instruction),
    /// A branch whose operand is a label rather than an offset.
    Jump {
        branch: Branch,
        target: Label,
        /// The state on the not-taken path. Kept because widening a conditional turns it into a
        /// jump over a `goto_w`, and that jump's landing site becomes a branch target the class
        /// file has to describe with a frame of its own.
        fallthrough: State,
    },
    /// A `tableswitch` or `lookupswitch`, whose every operand is a label.
    ///
    /// Not an [`Item::Fixed`] even though it never widens: its operands are offsets, so it cannot
    /// be built until they are known, and its own *length* depends on where it sits — both forms
    /// align their operands to a four-byte boundary measured from the start of the method.
    Switch {
        /// The `(key, target)` arms, in ascending key order with no duplicates.
        cases: Vec<(i32, Label)>,
        /// Where a key no arm claims goes.
        default: Label,
        /// Whether the dense `tableswitch` form was chosen over `lookupswitch`.
        table: bool,
    },
    /// A label's position. Occupies no bytes; which label it is lives in [`LabelInfo::bound`].
    Mark,
}

/// One protected range and the handler covering it — an exception-table entry with labels where the
/// class file wants offsets.
struct Protected {
    /// Inclusive start of the protected range.
    start: Label,
    /// Exclusive end of it.
    end: Label,
    /// Where control goes when something in the range throws.
    handler: Label,
    /// The `Class` index of the caught type, or `0` for the catch-all (JVMS §4.7.3).
    caught: u16,
}

/// Everything the assembler tracks about one label.
struct LabelInfo {
    /// Index into the item stream, once bound.
    bound: Option<usize>,
    /// The merged state of every path that reaches the label.
    state: Option<State>,
    /// Whether anything jumps here. A label only fallen into needs no stack-map frame.
    targeted: bool,
}

/// Assembles one method body.
///
/// Emission methods mirror what a lowering wants to say (`push a string constant`, `call this
/// method`) rather than individual opcodes, because the assembler has to know the *type* of every
/// value to track the frame. Deriving that from a descriptor it interned itself is exact; deriving
/// it from a raw opcode plus a pool index would mean re-reading the pool on every instruction.
pub struct Assembler<'pool> {
    pool: &'pool mut ConstantPool,
    items: Vec<Item>,
    labels: Vec<LabelInfo>,
    /// The exception table, still in terms of labels.
    handlers: Vec<Protected>,
    state: State,
    /// `false` after an unconditional transfer, until a label is bound.
    reachable: bool,
    max_stack: u16,
    max_locals: u16,
    /// `java/lang/Object`'s `Class` index, interned once for merge-point widening.
    object: u16,
    /// The type an `uninitializedThis` becomes once this body runs `invokespecial <init>`; `None`
    /// outside a constructor.
    initialized_this: Option<VerificationType>,
}

impl<'pool> Assembler<'pool> {
    /// An assembler for a method with `descriptor`, whose locals start out holding `receiver`
    /// followed by the declared parameters.
    pub fn new(
        pool: &'pool mut ConstantPool,
        receiver: Receiver<'_>,
        descriptor: &str,
    ) -> Result<Self> {
        let parsed = MethodDescriptor::parse(descriptor).map_err(|_| AsmError::BadDescriptor)?;
        let object = pool
            .class_index("java/lang/Object")
            .ok_or(AsmError::PoolFull)?;

        let mut slots = Vec::new();
        let mut initialized_this = None;
        match receiver {
            Receiver::Static => {}
            Receiver::Instance(name) => slots.push(Slot::Value(Self::object_type(pool, name)?)),
            Receiver::Constructor(name) => {
                initialized_this = Some(Self::object_type(pool, name)?);
                slots.push(Slot::Value(VerificationType::UninitializedThis));
            }
        }
        for param in &parsed.params {
            let ty = Self::verification_type(pool, param)?;
            let wide = State::words(&ty) == 2;
            slots.push(Slot::Value(ty));
            if wide {
                slots.push(Slot::Upper);
            }
        }

        let state = State::new(slots);
        let max_locals = state.slot_count();
        Ok(Self {
            pool,
            items: Vec::new(),
            labels: Vec::new(),
            handlers: Vec::new(),
            state,
            reachable: true,
            max_stack: 0,
            max_locals,
            object,
            initialized_this,
        })
    }

    /// How many *values* are on the operand stack right now (a `long` counts once, not twice).
    ///
    /// A lowering needs this to discard whatever an expression-statement left behind: the JVM has
    /// no "evaluate and drop" instruction, so the caller pops back down to the depth it started at.
    pub(crate) const fn stack_depth(&self) -> usize {
        self.state.stack_len()
    }

    /// The type of the value on top of the stack.
    pub fn stack_top(&self) -> Option<VerificationType> {
        self.state.peek().cloned()
    }

    /// Whether control can arrive at the next instruction emitted.
    ///
    /// `false` after a `return` or an unconditional branch, until a label is bound. A lowering
    /// needs this because the *source* has statements a finished basic block does not: the jump
    /// over an `else` arm exists only when the `then` arm can fall out of it, and `if (c) { return;
    /// } …` is the ordinary shape where it cannot.
    pub(crate) const fn reachable(&self) -> bool {
        self.reachable
    }

    /// Whether anything jumps to `label` yet.
    ///
    /// A lowering needs this because the *source* has positions a finished basic block does not. A
    /// loop's update section is reachable if the body can fall out of it **or** a `continue` jumped
    /// there, and `for (;;) { return; }` is the ordinary shape where neither holds — binding the
    /// label then would report a position control cannot arrive at, which is true and not the
    /// caller's mistake.
    pub(crate) fn is_targeted(&self, label: Label) -> Result<bool> {
        Ok(self.info(label)?.targeted)
    }

    /// A fresh, unbound label.
    pub fn label(&mut self) -> Label {
        self.labels.push(LabelInfo {
            bound: None,
            state: None,
            targeted: false,
        });
        Label(self.labels.len() - 1)
    }

    /// Fix `label` at the current position, making it the merge point of the fallthrough (when
    /// control can reach here) and every jump recorded so far.
    pub fn bind(&mut self, label: Label) -> Result<()> {
        let recorded = self.info(label)?.state.clone();
        let merged = match (self.reachable, recorded) {
            (true, Some(recorded)) => self
                .state
                .join(&recorded, self.object)
                .ok_or(AsmError::IncompatibleFrame)?,
            (true, None) => self.state.clone(),
            // Falling out of unreachable code into a label something jumps to is the normal shape
            // of `if (c) { return; } ...` — the jump's state is the only one that arrives.
            (false, Some(recorded)) => recorded,
            (false, None) => return Err(AsmError::UnreachableLabel),
        };

        self.items.push(Item::Mark);
        let index = self.items.len() - 1;
        let info = self.info_mut(label)?;
        info.bound = Some(index);
        info.state = Some(merged.clone());

        self.state = merged;
        self.reachable = true;
        self.note_frame();
        Ok(())
    }

    /// Fix `label` at the current position as a *position only*: no frame, no state, and no
    /// requirement that control can arrive here.
    ///
    /// A protected range's end is exactly that. It is an offset in the exception table and nothing
    /// else — the instruction there belongs to whatever follows the `try`, and after a body that
    /// ends in a jump there is no state to record for it. [`bind`](Self::bind) would report it as a
    /// label control cannot reach, which is true and beside the point.
    pub fn mark(&mut self, label: Label) -> Result<()> {
        self.items.push(Item::Mark);
        let index = self.items.len() - 1;
        self.info_mut(label)?.bound = Some(index);
        Ok(())
    }

    /// Fix `label` as an exception handler's entry point, catching `caught`.
    ///
    /// A handler is not *jumped* to, so no [`branch`](Self::branch) ever records a state for it:
    /// control arrives from any instruction in the protected range, on an edge the JVM supplies.
    /// Its frame is therefore given rather than merged — the locals as they stood where the range
    /// began (`range_start`, already bound), and the caught reference as the only value on the
    /// stack.
    ///
    /// Those locals describe the whole range, which is what the verifier demands: the state at
    /// every protected instruction has to be assignable to this frame. A local live where the range
    /// began keeps its type throughout it — [`Slots`](crate::lower) never reuses a slot, and
    /// [`store_as`](Self::store_as) types a declared one by its declaration, so neither a reuse nor a
    /// reassignment can retype it. One the range itself declares reads as `Top` here, which every
    /// state is assignable to (JVMS §4.10.1.2).
    ///
    /// The entry always carries a stack-map frame. Nothing in the item stream jumps here to say
    /// so, so it is marked as a target rather than discovered as one.
    pub fn bind_handler(&mut self, label: Label, range_start: Label, caught: &str) -> Result<()> {
        let entry = self
            .info(range_start)?
            .state
            .clone()
            .ok_or(AsmError::UnreachableLabel)?;
        let exception = Self::object_type(self.pool, caught)?;
        let state = entry.with_stack(alloc::vec![exception]);

        self.items.push(Item::Mark);
        let index = self.items.len() - 1;
        let info = self.info_mut(label)?;
        info.bound = Some(index);
        info.state = Some(state.clone());
        info.targeted = true;

        self.state = state;
        self.reachable = true;
        self.note_frame();
        Ok(())
    }

    /// Protect `[start, end)` with `handler`, catching `caught`.
    ///
    /// `None` catches everything, which is what a `finally` clause and a `synchronized` block's
    /// unlock path both need. Order matters and is preserved: the JVM takes the *first* entry whose
    /// range covers the throwing instruction and whose type matches, so a `catch` chain has to be
    /// recorded in source order and a `finally`'s catch-all after all of them.
    pub fn protect(
        &mut self,
        start: Label,
        end: Label,
        handler: Label,
        caught: Option<&str>,
    ) -> Result<()> {
        let caught = match caught {
            Some(name) => self.pool.class_index(name).ok_or(AsmError::PoolFull)?,
            None => 0,
        };
        self.handlers.push(Protected {
            start,
            end,
            handler,
            caught,
        });
        Ok(())
    }

    /// Branch to `label`. `Branch::Always` ends the basic block.
    pub fn branch(&mut self, branch: Branch, target: Label) -> Result<()> {
        self.require_reachable()?;
        let (operands, expected) = branch.operands();
        for _ in 0..operands {
            let popped = self.state.pop().ok_or(AsmError::StackUnderflow)?;
            if let Some(expected) = &expected
                && !Self::compatible(expected, &popped)
            {
                return Err(AsmError::TypeMismatch);
            }
        }

        self.record_arrival(target)?;
        self.info_mut(target)?.targeted = true;
        let fallthrough = self.state.clone();
        self.items.push(Item::Jump {
            branch,
            target,
            fallthrough,
        });
        if branch == Branch::Always {
            self.reachable = false;
        }
        Ok(())
    }

    /// Branch on the comparison of the two values on top of the stack.
    ///
    /// One entry point for every type, because the instruction it takes is not one shape. An `int`
    /// pair has a direct `if_icmp*`; a `long` / `float` / `double` pair has to go through `lcmp` /
    /// `fcmp?` / `dcmp?`, which reduces it to the `int` -1 / 0 / 1 that an `if*` then tests against
    /// zero; a reference pair has `if_acmp*` and only for equality.
    ///
    /// **NaN decides which floating form.** `fcmpg` yields 1 for a NaN operand and `fcmpl` yields
    /// -1, and JLS §15.20.1 requires *every* numeric comparison involving a NaN to be false. So the
    /// form has to be the one whose NaN answer fails the test that follows it: `fcmpg` under `<` and
    /// `<=` (1 is neither `< 0` nor `<= 0`), `fcmpl` under `>` and `>=`. Getting this backwards
    /// produces a comparison that is *true* for NaN — a wrong answer no verifier would catch.
    pub fn branch_compare(
        &mut self,
        ty: &VerificationType,
        compare: Compare,
        target: Label,
    ) -> Result<()> {
        let reduce = match ty {
            VerificationType::Integer => None,
            VerificationType::Long => Some(Instruction::Lcmp),
            VerificationType::Float => match compare {
                Compare::Lt | Compare::Le => Some(Instruction::Fcmpg),
                Compare::Eq | Compare::Ne | Compare::Gt | Compare::Ge => Some(Instruction::Fcmpl),
            },
            VerificationType::Double => match compare {
                Compare::Lt | Compare::Le => Some(Instruction::Dcmpg),
                Compare::Eq | Compare::Ne | Compare::Gt | Compare::Ge => Some(Instruction::Dcmpl),
            },
            other if Self::is_reference(other) => {
                // Only `==` and `!=` are defined over references; `<` on two objects is not a Java
                // program, so reaching here with one is a generator bug rather than an emission.
                let same = match compare {
                    Compare::Eq => true,
                    Compare::Ne => false,
                    _ => return Err(AsmError::TypeMismatch),
                };
                return self.branch(Branch::RefSame(same), target);
            }
            _ => return Err(AsmError::TypeMismatch),
        };
        match reduce {
            None => self.branch(Branch::IntCmp(compare), target),
            Some(instruction) => {
                self.emit(
                    instruction,
                    &[ty.clone(), ty.clone()],
                    Some(VerificationType::Integer),
                )?;
                self.branch(Branch::IntZero(compare), target)
            }
        }
    }

    /// A `switch` over the `int` on top of the stack.
    ///
    /// `cases` may arrive in any order; both instruction forms require ascending keys, so they are
    /// sorted here rather than demanded of the caller. Two arms on one key would make the jump
    /// ambiguous — and `tableswitch`, which indexes rather than searches, would silently keep only
    /// one of them — so that is reported.
    ///
    /// The form is chosen by density. `tableswitch` spends four bytes per key *in the whole span*
    /// whether an arm claims it or not, and jumps in constant time; `lookupswitch` spends eight per
    /// arm and searches. So the table wins while the span stays close to the arm count, which is
    /// also what bounds the loop that fills its holes with `default`.
    pub fn switch(&mut self, cases: &[(i32, Label)], default: Label) -> Result<()> {
        self.require_reachable()?;
        let key = self.state.pop().ok_or(AsmError::StackUnderflow)?;
        if key != VerificationType::Integer {
            return Err(AsmError::TypeMismatch);
        }

        let mut cases = cases.to_vec();
        cases.sort_unstable_by_key(|&(key, _)| key);
        if cases.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(AsmError::DuplicateCase);
        }

        for &(_, target) in &cases {
            self.record_arrival(target)?;
            self.info_mut(target)?.targeted = true;
        }
        self.record_arrival(default)?;
        self.info_mut(default)?.targeted = true;

        let table = Self::prefers_table(&cases);
        self.items.push(Item::Switch {
            cases,
            default,
            table,
        });
        self.note_frame();
        self.reachable = false;
        Ok(())
    }

    /// Whether the dense `tableswitch` form is the cheaper one for these keys.
    ///
    /// A `tableswitch` costs a 12-byte header plus four bytes for every key between the lowest and
    /// the highest, claimed or not. A `lookupswitch` costs an 8-byte header plus eight per arm. The
    /// table wins exactly where the first total is no larger — which for a span of `n` arms means
    /// the keys are spread over at most about `2n` values.
    fn prefers_table(cases: &[(i32, Label)]) -> bool {
        let (Some(&(low, _)), Some(&(high, _))) = (cases.first(), cases.last()) else {
            // No arms at all: every key goes to `default`, and `lookupswitch` says so in the fewest
            // bytes. It also keeps `low..=high` from being a span this never meant to describe.
            return false;
        };
        // Widened through `i64` because `high - low` overflows `i32` for keys at both extremes.
        let span = i64::from(high) - i64::from(low) + 1;
        let count = i64::try_from(cases.len()).unwrap_or(i64::MAX);
        12 + 4 * span <= 8 + 8 * count
    }

    /// Push an `int` constant, in the shortest form that holds it.
    pub fn const_int(&mut self, value: i32) -> Result<()> {
        let instruction = match value {
            -1 => Instruction::IconstM1,
            0 => Instruction::Iconst0,
            1 => Instruction::Iconst1,
            2 => Instruction::Iconst2,
            3 => Instruction::Iconst3,
            4 => Instruction::Iconst4,
            5 => Instruction::Iconst5,
            _ => match (i8::try_from(value), i16::try_from(value)) {
                (Ok(byte), _) => Instruction::Bipush(byte),
                (_, Ok(short)) => Instruction::Sipush(short),
                _ => {
                    let index = self.pool.integer_index(value).ok_or(AsmError::PoolFull)?;
                    Self::load_constant(index)
                }
            },
        };
        self.emit(instruction, &[], Some(VerificationType::Integer))
    }

    /// Push a `long` constant.
    pub fn const_long(&mut self, value: i64) -> Result<()> {
        let instruction = match value {
            0 => Instruction::Lconst0,
            1 => Instruction::Lconst1,
            _ => Instruction::Ldc2W(self.pool.long_index(value).ok_or(AsmError::PoolFull)?),
        };
        self.emit(instruction, &[], Some(VerificationType::Long))
    }

    /// Push a `float` constant.
    ///
    /// The `fconst_*` shortcuts are matched on the *bit pattern*: `-0.0 == 0.0` is true in Rust as
    /// it is in Java, and emitting `fconst_0` for `-0.0` would change the program's arithmetic.
    pub fn const_float(&mut self, value: f32) -> Result<()> {
        let instruction = match value.to_bits() {
            bits if bits == 0.0f32.to_bits() => Instruction::Fconst0,
            bits if bits == 1.0f32.to_bits() => Instruction::Fconst1,
            bits if bits == 2.0f32.to_bits() => Instruction::Fconst2,
            _ => {
                let index = self.pool.float_index(value).ok_or(AsmError::PoolFull)?;
                Self::load_constant(index)
            }
        };
        self.emit(instruction, &[], Some(VerificationType::Float))
    }

    /// Push a `double` constant.
    pub fn const_double(&mut self, value: f64) -> Result<()> {
        let instruction = match value.to_bits() {
            bits if bits == 0.0f64.to_bits() => Instruction::Dconst0,
            bits if bits == 1.0f64.to_bits() => Instruction::Dconst1,
            _ => Instruction::Ldc2W(self.pool.double_index(value).ok_or(AsmError::PoolFull)?),
        };
        self.emit(instruction, &[], Some(VerificationType::Double))
    }

    /// Push a `String` literal.
    pub fn const_string(&mut self, text: &str) -> Result<()> {
        let index = self.pool.string_index(text).ok_or(AsmError::PoolFull)?;
        let ty = Self::object_type(self.pool, "java/lang/String")?;
        self.emit(Self::load_constant(index), &[], Some(ty))
    }

    /// Push a `Class` object — what `Foo.class` evaluates to.
    ///
    /// A `Class` constant is `ldc` over a `Class` pool entry, which is the same entry a `checkcast`
    /// names. Legal from major version 49 on (JVMS §4.4.1), which every version this crate emits is.
    pub(crate) fn const_class(&mut self, internal_name: &str) -> Result<()> {
        let index = self
            .pool
            .class_index(internal_name)
            .ok_or(AsmError::PoolFull)?;
        let ty = Self::object_type(self.pool, "java/lang/Class")?;
        self.emit(Self::load_constant(index), &[], Some(ty))
    }

    /// Push `null`.
    pub fn const_null(&mut self) -> Result<()> {
        self.emit(Instruction::AconstNull, &[], Some(VerificationType::Null))
    }

    /// Push the value in local slot `index`, whose type the assembler already tracks.
    pub fn load(&mut self, index: u16) -> Result<()> {
        let ty = self
            .state
            .local(index)
            .ok_or(AsmError::UnwrittenLocal)?
            .clone();
        let instruction = match (&ty, index) {
            (VerificationType::Integer, 0) => Instruction::Iload0,
            (VerificationType::Integer, 1) => Instruction::Iload1,
            (VerificationType::Integer, 2) => Instruction::Iload2,
            (VerificationType::Integer, 3) => Instruction::Iload3,
            (VerificationType::Integer, _) => {
                Self::wide_or_narrow(index, Instruction::Iload, WideInstruction::Iload)
            }
            (VerificationType::Long, 0) => Instruction::Lload0,
            (VerificationType::Long, 1) => Instruction::Lload1,
            (VerificationType::Long, 2) => Instruction::Lload2,
            (VerificationType::Long, 3) => Instruction::Lload3,
            (VerificationType::Long, _) => {
                Self::wide_or_narrow(index, Instruction::Lload, WideInstruction::Lload)
            }
            (VerificationType::Float, 0) => Instruction::Fload0,
            (VerificationType::Float, 1) => Instruction::Fload1,
            (VerificationType::Float, 2) => Instruction::Fload2,
            (VerificationType::Float, 3) => Instruction::Fload3,
            (VerificationType::Float, _) => {
                Self::wide_or_narrow(index, Instruction::Fload, WideInstruction::Fload)
            }
            (VerificationType::Double, 0) => Instruction::Dload0,
            (VerificationType::Double, 1) => Instruction::Dload1,
            (VerificationType::Double, 2) => Instruction::Dload2,
            (VerificationType::Double, 3) => Instruction::Dload3,
            (VerificationType::Double, _) => {
                Self::wide_or_narrow(index, Instruction::Dload, WideInstruction::Dload)
            }
            (other, _) if Self::is_reference(other) => match index {
                0 => Instruction::Aload0,
                1 => Instruction::Aload1,
                2 => Instruction::Aload2,
                3 => Instruction::Aload3,
                _ => Self::wide_or_narrow(index, Instruction::Aload, WideInstruction::Aload),
            },
            _ => return Err(AsmError::TypeMismatch),
        };
        self.emit(instruction, &[], Some(ty))
    }

    /// Pop the top of the stack into local slot `index`, typing the slot by the value written.
    ///
    /// For a slot the source *declared*, use [`store_as`](Self::store_as) instead: a declared local
    /// keeps its declared type however narrow the value written into it happens to be. This form is
    /// for the slots a lowering takes for itself, where the value written *is* the type.
    pub fn store(&mut self, index: u16) -> Result<()> {
        let ty = self.state.peek().ok_or(AsmError::StackUnderflow)?.clone();
        self.store_slot(index, ty)
    }

    /// Pop the top of the stack into local slot `index`, which the source declared as `descriptor`.
    ///
    /// The slot keeps the *declared* type rather than the written value's: a `String` assigned to an
    /// `Object` local leaves an `Object` behind, which is what javac records. That is the property
    /// [`Slots`](crate::lower) states — a slot's type is fixed for the whole method — and it is not
    /// something monotonic allocation alone can give, because a reassignment retypes a slot the
    /// allocator never reused.
    ///
    /// An exception handler is where the difference shows. It is entered from *any* instruction in
    /// its protected range, so [`bind_handler`](Self::bind_handler) gives it the range's start state;
    /// a range that reassigns a local to an unrelated type would otherwise describe that slot as
    /// whatever happened to be written first, and the verifier rejects the merge. A backward jump
    /// fails the same way earlier, as an [`IncompatibleFrame`](AsmError::IncompatibleFrame) at the
    /// loop header.
    pub fn store_as(&mut self, index: u16, descriptor: &str) -> Result<()> {
        let declared = Self::field_verification_type(self.pool, descriptor)?;
        // The value has to be *storable* in the slot, which the declared type alone cannot say: the
        // hierarchy is not here. What it can catch is a lowering that left the wrong kind of value
        // behind — an unboxed reference where an `int` is due, and the reverse.
        let actual = self.state.peek().ok_or(AsmError::StackUnderflow)?;
        if !Self::compatible(&declared, actual) {
            return Err(AsmError::TypeMismatch);
        }
        self.store_slot(index, declared)
    }

    /// The shared body of [`store`](Self::store) and [`store_as`](Self::store_as): pick the opcode
    /// from `ty`, pop the value, and record `ty` as what the slot now holds.
    fn store_slot(&mut self, index: u16, ty: VerificationType) -> Result<()> {
        let instruction = match (&ty, index) {
            (VerificationType::Integer, 0) => Instruction::Istore0,
            (VerificationType::Integer, 1) => Instruction::Istore1,
            (VerificationType::Integer, 2) => Instruction::Istore2,
            (VerificationType::Integer, 3) => Instruction::Istore3,
            (VerificationType::Integer, _) => {
                Self::wide_or_narrow(index, Instruction::Istore, WideInstruction::Istore)
            }
            (VerificationType::Long, 0) => Instruction::Lstore0,
            (VerificationType::Long, 1) => Instruction::Lstore1,
            (VerificationType::Long, 2) => Instruction::Lstore2,
            (VerificationType::Long, 3) => Instruction::Lstore3,
            (VerificationType::Long, _) => {
                Self::wide_or_narrow(index, Instruction::Lstore, WideInstruction::Lstore)
            }
            (VerificationType::Float, 0) => Instruction::Fstore0,
            (VerificationType::Float, 1) => Instruction::Fstore1,
            (VerificationType::Float, 2) => Instruction::Fstore2,
            (VerificationType::Float, 3) => Instruction::Fstore3,
            (VerificationType::Float, _) => {
                Self::wide_or_narrow(index, Instruction::Fstore, WideInstruction::Fstore)
            }
            (VerificationType::Double, 0) => Instruction::Dstore0,
            (VerificationType::Double, 1) => Instruction::Dstore1,
            (VerificationType::Double, 2) => Instruction::Dstore2,
            (VerificationType::Double, 3) => Instruction::Dstore3,
            (VerificationType::Double, _) => {
                Self::wide_or_narrow(index, Instruction::Dstore, WideInstruction::Dstore)
            }
            (other, _) if Self::is_reference(other) => match index {
                0 => Instruction::Astore0,
                1 => Instruction::Astore1,
                2 => Instruction::Astore2,
                3 => Instruction::Astore3,
                _ => Self::wide_or_narrow(index, Instruction::Astore, WideInstruction::Astore),
            },
            _ => return Err(AsmError::TypeMismatch),
        };
        self.require_reachable()?;
        self.state.pop();
        self.state.set_local(index, ty);
        self.max_locals = self.max_locals.max(self.state.slot_count());
        self.push_item(instruction);
        Ok(())
    }

    /// `iinc slot, delta` — add `delta` to an `int` local in place, without touching the stack.
    ///
    /// The one instruction that reads and writes a local without going through the operand stack,
    /// which is why `i++` on a local is one instruction and `a[i]++` is nine.
    pub fn increment(&mut self, index: u16, delta: i16) -> Result<()> {
        self.require_reachable()?;
        match self.state.local(index) {
            Some(VerificationType::Integer) => {}
            Some(_) => return Err(AsmError::TypeMismatch),
            None => return Err(AsmError::UnwrittenLocal),
        }
        let instruction = match (u8::try_from(index), i8::try_from(delta)) {
            (Ok(index), Ok(value)) => Instruction::Iinc { index, value },
            // Either operand outgrowing a byte takes the `wide` form, which widens *both*.
            _ => Instruction::Wide(WideInstruction::Iinc {
                index,
                value: delta,
            }),
        };
        self.push_item(instruction);
        Ok(())
    }

    /// Convert the value on top of the stack from `from` to `to`.
    ///
    /// Two steps, not one, whenever a narrowing to `byte` / `char` / `short` does not start from
    /// `int`: JLS §5.1.3 *defines* `double`-to-`byte` as `d2i` followed by `i2b`, and there is no
    /// single opcode for it. The second step is skipped where the source's range already fits the
    /// target's — `byte` to `short` needs nothing, while `byte` to `char` needs `i2c`, because a
    /// signed byte's negative half has no place in an unsigned `char`.
    pub fn convert(&mut self, from: Numeric, to: Numeric) -> Result<()> {
        use Numeric::{Byte, Char, Double, Float, Int, Long, Short};
        // Step one moves between the four types the JVM actually computes in. Nothing to do when
        // both sides already share one, which is every conversion among `byte` / `short` / `char` /
        // `int`.
        if from.stack() != to.stack() {
            let instruction = match (from, to) {
                (Byte | Short | Char | Int, Long) => Instruction::I2l,
                (Byte | Short | Char | Int, Float) => Instruction::I2f,
                (Byte | Short | Char | Int, Double) => Instruction::I2d,
                (Long, Byte | Short | Char | Int) => Instruction::L2i,
                (Long, Float) => Instruction::L2f,
                (Long, Double) => Instruction::L2d,
                (Float, Byte | Short | Char | Int) => Instruction::F2i,
                (Float, Long) => Instruction::F2l,
                (Float, Double) => Instruction::F2d,
                (Double, Byte | Short | Char | Int) => Instruction::D2i,
                (Double, Long) => Instruction::D2l,
                (Double, Float) => Instruction::D2f,
                // Unreachable while the two stack types differ; kept because the compiler cannot
                // see that from the guard.
                _ => return Err(AsmError::TypeMismatch),
            };
            self.emit(instruction, &[from.stack()], Some(to.stack()))?;
        }

        // Step two truncates to the sub-`int` type's range. It leaves the *stack* type alone —
        // `Integer` before and after — which is the whole reason `Numeric` is not a
        // `VerificationType`.
        let narrowing = match to {
            Byte if from != Byte => Some(Instruction::I2b),
            Short if !matches!(from, Byte | Short) => Some(Instruction::I2s),
            Char if from != Char => Some(Instruction::I2c),
            _ => None,
        };
        if let Some(instruction) = narrowing {
            self.emit(
                instruction,
                &[VerificationType::Integer],
                Some(VerificationType::Integer),
            )?;
        }
        Ok(())
    }

    /// Negate the numeric value on top of the stack.
    pub(crate) fn negate(&mut self, ty: &VerificationType) -> Result<()> {
        let instruction = match ty {
            VerificationType::Integer => Instruction::Ineg,
            VerificationType::Long => Instruction::Lneg,
            VerificationType::Float => Instruction::Fneg,
            VerificationType::Double => Instruction::Dneg,
            _ => return Err(AsmError::TypeMismatch),
        };
        self.emit(instruction, core::slice::from_ref(ty), Some(ty.clone()))
    }

    /// `new owner` — allocate an instance whose constructor has not run.
    ///
    /// The value it leaves behind is `uninitializedThis`'s sibling: an
    /// [`Uninitialized`](VerificationType::Uninitialized) naming *the bytecode offset of this very
    /// instruction*, which is how a frame distinguishes two objects awaiting different constructors.
    ///
    /// That offset does not exist yet — branch widening has not run, so nothing has an offset. What
    /// goes into the state here is the instruction's **index in the item stream**, which
    /// [`stack_map`](Self::stack_map) translates once the offsets are known. Every `Uninitialized`
    /// this assembler records is one of these, so the translation is total and cannot mistake a
    /// marker for a real offset.
    pub fn new_object(&mut self, internal_name: &str) -> Result<()> {
        let class = self
            .pool
            .class_index(internal_name)
            .ok_or(AsmError::PoolFull)?;
        // `emit` pushes one item and pops none, so the index it lands on is the current length.
        let marker = u16::try_from(self.items.len()).map_err(|_| AsmError::TooLarge)?;
        self.emit(
            Instruction::New(class),
            &[],
            Some(VerificationType::Uninitialized { offset: marker }),
        )
    }

    /// `newarray` / `anewarray` — allocate a one-dimensional array of `element` (a field
    /// descriptor), whose length is on the stack.
    pub fn new_array(&mut self, element: &str) -> Result<()> {
        let ty = FieldType::parse(element).map_err(|_| AsmError::BadDescriptor)?;
        let instruction = match &ty {
            FieldType::Base(base) => Instruction::NewArray(Self::array_code(*base)),
            // A reference element is named by a `Class` entry: a class by its internal name, and a
            // nested array by its own descriptor.
            FieldType::Object(name) => {
                Instruction::ANewArray(self.pool.class_index(name).ok_or(AsmError::PoolFull)?)
            }
            FieldType::Array(_) => {
                let name = ty.to_string();
                Instruction::ANewArray(self.pool.class_index(&name).ok_or(AsmError::PoolFull)?)
            }
        };
        let array = Self::object_type(self.pool, &alloc::format!("[{element}"))?;
        self.emit(instruction, &[VerificationType::Integer], Some(array))
    }

    /// `multianewarray array, dimensions` — allocate the outer `dimensions` levels of `array` (a
    /// full array descriptor), whose lengths are on the stack outermost first.
    pub fn new_multi_array(&mut self, array: &str, dimensions: u8) -> Result<()> {
        let index = self.pool.class_index(array).ok_or(AsmError::PoolFull)?;
        let ty = Self::object_type(self.pool, array)?;
        let lengths = alloc::vec![VerificationType::Integer; usize::from(dimensions)];
        self.emit(
            Instruction::MultiANewArray { index, dimensions },
            &lengths,
            Some(ty),
        )
    }

    /// `arraylength`.
    pub fn array_length(&mut self) -> Result<()> {
        self.emit(
            Instruction::ArrayLength,
            &[VerificationType::Null],
            Some(VerificationType::Integer),
        )
    }

    /// `*aload` — read `array[index]`, with the array below the index.
    pub fn array_load(&mut self, element: &str) -> Result<()> {
        let ty = FieldType::parse(element).map_err(|_| AsmError::BadDescriptor)?;
        let pushed = Self::verification_type(self.pool, &ty)?;
        self.emit(
            Self::array_load_op(&ty),
            &[VerificationType::Null, VerificationType::Integer],
            Some(pushed),
        )
    }

    /// `*astore` — write `array[index] = value`, with the array below the index below the value.
    pub fn array_store(&mut self, element: &str) -> Result<()> {
        let ty = FieldType::parse(element).map_err(|_| AsmError::BadDescriptor)?;
        let value = Self::verification_type(self.pool, &ty)?;
        self.emit(
            Self::array_store_op(&ty),
            &[VerificationType::Null, VerificationType::Integer, value],
            None,
        )
    }

    /// `checkcast target` — narrow the reference on top to `target`, an internal name or (for an
    /// array type) an array descriptor.
    pub fn check_cast(&mut self, target: &str) -> Result<()> {
        let index = self.pool.class_index(target).ok_or(AsmError::PoolFull)?;
        let ty = Self::object_type(self.pool, target)?;
        self.emit(
            Instruction::CheckCast(index),
            &[VerificationType::Null],
            Some(ty),
        )
    }

    /// `instanceof target` — replace the reference on top with a `boolean`.
    pub fn instance_of(&mut self, target: &str) -> Result<()> {
        let index = self.pool.class_index(target).ok_or(AsmError::PoolFull)?;
        self.emit(
            Instruction::InstanceOf(index),
            &[VerificationType::Null],
            Some(VerificationType::Integer),
        )
    }

    /// `athrow` — throw the reference on top. Ends the basic block: control leaves for a handler or
    /// out of the method, and never reaches the next instruction.
    pub fn throw(&mut self) -> Result<()> {
        self.emit(Instruction::Athrow, &[VerificationType::Null], None)?;
        self.reachable = false;
        Ok(())
    }

    /// `monitorenter` — acquire the monitor of the reference on top.
    pub fn monitor_enter(&mut self) -> Result<()> {
        self.emit(Instruction::MonitorEnter, &[VerificationType::Null], None)
    }

    /// `monitorexit` — release it.
    pub fn monitor_exit(&mut self) -> Result<()> {
        self.emit(Instruction::MonitorExit, &[VerificationType::Null], None)
    }

    /// `getstatic owner.name : descriptor`.
    pub fn get_static(&mut self, owner: &str, name: &str, descriptor: &str) -> Result<()> {
        let index = self.field_ref(owner, name, descriptor)?;
        let ty = Self::field_verification_type(self.pool, descriptor)?;
        self.emit(Instruction::GetStatic(index), &[], Some(ty))
    }

    /// `putstatic owner.name : descriptor`.
    pub fn put_static(&mut self, owner: &str, name: &str, descriptor: &str) -> Result<()> {
        let index = self.field_ref(owner, name, descriptor)?;
        let ty = Self::field_verification_type(self.pool, descriptor)?;
        self.emit(Instruction::PutStatic(index), &[ty], None)
    }

    /// `getfield owner.name : descriptor`, with the receiver on top of the stack.
    pub(crate) fn get_field(&mut self, owner: &str, name: &str, descriptor: &str) -> Result<()> {
        let index = self.field_ref(owner, name, descriptor)?;
        let receiver = Self::object_type(self.pool, owner)?;
        let ty = Self::field_verification_type(self.pool, descriptor)?;
        self.emit(Instruction::GetField(index), &[receiver], Some(ty))
    }

    /// `putfield owner.name : descriptor`, with the receiver below the value.
    pub(crate) fn put_field(&mut self, owner: &str, name: &str, descriptor: &str) -> Result<()> {
        let index = self.field_ref(owner, name, descriptor)?;
        let receiver = Self::object_type(self.pool, owner)?;
        let ty = Self::field_verification_type(self.pool, descriptor)?;
        self.emit(Instruction::PutField(index), &[receiver, ty], None)
    }

    /// `invokestatic owner.name descriptor`. `interface_owner` picks the `InterfaceMethodRef`
    /// constant form, which a `static` method declared on an interface requires.
    pub fn invoke_static(
        &mut self,
        owner: &str,
        name: &str,
        descriptor: &str,
        interface_owner: bool,
    ) -> Result<()> {
        let index = self.method_ref(owner, name, descriptor, interface_owner)?;
        self.invoke(Instruction::InvokeStatic(index), None, descriptor)
    }

    /// `invokedynamic`: a call site the JVM links by running a bootstrap method.
    ///
    /// No owner, because there is none — the call site names only itself, its descriptor, and which
    /// `BootstrapMethods` entry computes the handle it will call. That is what lets one bootstrap serve
    /// every site of the same shape.
    pub fn invoke_dynamic(&mut self, bootstrap: u16, name: &str, descriptor: &str) -> Result<()> {
        let index = self
            .pool
            .invoke_dynamic_index(bootstrap, name, descriptor)
            .ok_or(AsmError::PoolFull)?;
        self.invoke(Instruction::InvokeDynamic { index }, None, descriptor)
    }

    /// `invokevirtual owner.name descriptor`, with the receiver below the arguments.
    pub fn invoke_virtual(&mut self, owner: &str, name: &str, descriptor: &str) -> Result<()> {
        let index = self.method_ref(owner, name, descriptor, false)?;
        let receiver = Self::object_type(self.pool, owner)?;
        self.invoke(
            Instruction::InvokeVirtual(index),
            Some(receiver),
            descriptor,
        )
    }

    /// `invokespecial owner.name descriptor` — a constructor, a `private` method, or a
    /// `super` call.
    pub fn invoke_special(
        &mut self,
        owner: &str,
        name: &str,
        descriptor: &str,
        interface_owner: bool,
    ) -> Result<()> {
        let index = self.method_ref(owner, name, descriptor, interface_owner)?;
        // Which reference is about to be initialised has to be read *before* the call pops it. It
        // sits directly under the arguments, which is `params.len()` values down.
        let uninitialized = if name == "<init>" {
            let parsed =
                MethodDescriptor::parse(descriptor).map_err(|_| AsmError::BadDescriptor)?;
            self.state.peek_at(parsed.params.len()).cloned()
        } else {
            None
        };
        let receiver = Self::object_type(self.pool, owner)?;
        self.invoke(
            Instruction::InvokeSpecial(index),
            Some(receiver),
            descriptor,
        )?;
        // Running a constructor initialises every copy of the object at once, wherever it is held
        // (JVMS §4.10.1.9). Until this happens the verifier refuses to let the value be used for
        // anything but another `<init>` call, which is what makes a leaked half-built object
        // impossible.
        match uninitialized {
            // A constructor's own `super(…)` / `this(…)`: what `uninitializedThis` becomes is *this*
            // class, not the `owner` the call named — a `super(…)` names the superclass.
            Some(from @ VerificationType::UninitializedThis) => {
                if let Some(initialized) = self.initialized_this.clone() {
                    self.state.replace_type(&from, &initialized);
                }
            }
            // A `new Foo(…)`: the object becomes a `Foo`, which is exactly the owner.
            Some(from @ VerificationType::Uninitialized { .. }) => {
                let initialized = Self::object_type(self.pool, owner)?;
                self.state.replace_type(&from, &initialized);
            }
            _ => {}
        }
        Ok(())
    }

    /// `invokeinterface owner.name descriptor`.
    pub fn invoke_interface(&mut self, owner: &str, name: &str, descriptor: &str) -> Result<()> {
        let index = self.method_ref(owner, name, descriptor, true)?;
        let receiver = Self::object_type(self.pool, owner)?;
        let parsed = MethodDescriptor::parse(descriptor).map_err(|_| AsmError::BadDescriptor)?;
        // The redundant `count` operand is the receiver plus every argument, in *words*.
        let count = parsed.params.iter().try_fold(1u16, |total, param| {
            let ty = Self::verification_type(self.pool, param)?;
            Ok(total + State::words(&ty))
        })?;
        let count = u8::try_from(count).map_err(|_| AsmError::TooLarge)?;
        self.invoke(
            Instruction::InvokeInterface { index, count },
            Some(receiver),
            descriptor,
        )
    }

    /// Duplicate the value on top of the stack.
    pub fn dup(&mut self) -> Result<()> {
        self.dup_below(0)
    }

    /// Duplicate the value on top of the stack, putting the copy `words` words further down.
    ///
    /// This is the shape an assignment *expression* needs, and the reason the JVM has six `dup`
    /// opcodes rather than one. `o.f = v` leaves `[receiver, value]` and has to yield `value`, so
    /// the copy goes under the one word the receiver occupies (`dup_x1`, or `dup2_x1` for a wide
    /// value). `a[i] = v` leaves `[array, index, value]` and needs it under two (`dup_x2` /
    /// `dup2_x2`).
    ///
    /// `words` is counted in words rather than values because that is what the opcodes count: a
    /// `dup_x2` reaches over two one-word values *or* one `long`. A `words` that would land inside a
    /// wide value has no opcode and is reported.
    pub fn dup_below(&mut self, words: u16) -> Result<()> {
        self.require_reachable()?;
        let ty = self.state.peek().ok_or(AsmError::StackUnderflow)?.clone();
        let instruction = match (words, State::words(&ty)) {
            (0, 1) => Instruction::Dup,
            (0, 2) => Instruction::Dup2,
            (1, 1) => Instruction::DupX1,
            (1, 2) => Instruction::Dup2X1,
            (2, 1) => Instruction::DupX2,
            (2, 2) => Instruction::Dup2X2,
            _ => return Err(AsmError::TypeMismatch),
        };

        // Re-seat the copy: take the value off, then everything it goes under, then put the copy,
        // the intervening values, and the original back.
        self.state.pop();
        let mut skipped = Vec::new();
        let mut remaining = words;
        while remaining > 0 {
            let value = self.state.pop().ok_or(AsmError::StackUnderflow)?;
            remaining = remaining
                .checked_sub(State::words(&value))
                .ok_or(AsmError::TypeMismatch)?;
            skipped.push(value);
        }
        self.state.push(ty.clone());
        for value in skipped.into_iter().rev() {
            self.state.push(value);
        }
        self.state.push(ty);
        self.push_item(instruction);
        Ok(())
    }

    /// Duplicate the **two** one-word values on top of the stack.
    ///
    /// `dup2` over two category-1 values, which is a different operation from `dup2` over one
    /// category-2 value even though it is the same opcode. A compound assignment to an array element
    /// needs it: `a[i] += v` has to read `a[i]` and then store back into the same `(array, index)`
    /// pair, and computing the pair twice would run the index expression twice.
    pub fn dup_pair(&mut self) -> Result<()> {
        self.require_reachable()?;
        let first = self.state.peek().ok_or(AsmError::StackUnderflow)?.clone();
        let second = self
            .state
            .peek_at(1)
            .ok_or(AsmError::StackUnderflow)?
            .clone();
        if State::words(&first) != 1 || State::words(&second) != 1 {
            return Err(AsmError::TypeMismatch);
        }
        self.state.push(second);
        self.state.push(first);
        self.push_item(Instruction::Dup2);
        Ok(())
    }

    /// Swap the two one-word values on top of the stack. The JVM has no `swap2`, which is why
    /// [`dup_below`](Self::dup_below) exists instead of a general shuffle.
    pub fn swap(&mut self) -> Result<()> {
        self.require_reachable()?;
        let first = self.state.peek().ok_or(AsmError::StackUnderflow)?.clone();
        let second = self
            .state
            .peek_at(1)
            .ok_or(AsmError::StackUnderflow)?
            .clone();
        if State::words(&first) != 1 || State::words(&second) != 1 {
            return Err(AsmError::TypeMismatch);
        }
        self.state.pop();
        self.state.pop();
        self.state.push(first);
        self.state.push(second);
        self.push_item(Instruction::Swap);
        Ok(())
    }

    /// Discard the top stack value.
    pub fn pop(&mut self) -> Result<()> {
        let ty = self.state.peek().ok_or(AsmError::StackUnderflow)?.clone();
        let instruction = if State::words(&ty) == 2 {
            Instruction::Pop2
        } else {
            Instruction::Pop
        };
        self.emit(instruction, &[ty], None)
    }

    /// Apply `op` to the two values on top.
    ///
    /// Both are `ty`, except under a shift: `lshl` shifts a `long` by an **`int`** count, so the
    /// right operand's type comes from the operator rather than from `ty`.
    pub fn binary(&mut self, op: BinOp, ty: &VerificationType) -> Result<()> {
        let instruction = match (op, ty) {
            (BinOp::Add, VerificationType::Integer) => Instruction::Iadd,
            (BinOp::Add, VerificationType::Long) => Instruction::Ladd,
            (BinOp::Add, VerificationType::Float) => Instruction::Fadd,
            (BinOp::Add, VerificationType::Double) => Instruction::Dadd,
            (BinOp::Sub, VerificationType::Integer) => Instruction::Isub,
            (BinOp::Sub, VerificationType::Long) => Instruction::Lsub,
            (BinOp::Sub, VerificationType::Float) => Instruction::Fsub,
            (BinOp::Sub, VerificationType::Double) => Instruction::Dsub,
            (BinOp::Mul, VerificationType::Integer) => Instruction::Imul,
            (BinOp::Mul, VerificationType::Long) => Instruction::Lmul,
            (BinOp::Mul, VerificationType::Float) => Instruction::Fmul,
            (BinOp::Mul, VerificationType::Double) => Instruction::Dmul,
            (BinOp::Div, VerificationType::Integer) => Instruction::Idiv,
            (BinOp::Div, VerificationType::Long) => Instruction::Ldiv,
            (BinOp::Div, VerificationType::Float) => Instruction::Fdiv,
            (BinOp::Div, VerificationType::Double) => Instruction::Ddiv,
            (BinOp::Rem, VerificationType::Integer) => Instruction::Irem,
            (BinOp::Rem, VerificationType::Long) => Instruction::Lrem,
            (BinOp::Rem, VerificationType::Float) => Instruction::Frem,
            (BinOp::Rem, VerificationType::Double) => Instruction::Drem,
            // The bitwise and shift families exist only over the two integral stack types. A
            // `boolean` computes as `int`, so `&` / `|` / `^` on two of them land here too.
            (BinOp::And, VerificationType::Integer) => Instruction::Iand,
            (BinOp::And, VerificationType::Long) => Instruction::Land,
            (BinOp::Or, VerificationType::Integer) => Instruction::Ior,
            (BinOp::Or, VerificationType::Long) => Instruction::Lor,
            (BinOp::Xor, VerificationType::Integer) => Instruction::Ixor,
            (BinOp::Xor, VerificationType::Long) => Instruction::Lxor,
            (BinOp::Shl, VerificationType::Integer) => Instruction::Ishl,
            (BinOp::Shl, VerificationType::Long) => Instruction::Lshl,
            (BinOp::Shr, VerificationType::Integer) => Instruction::Ishr,
            (BinOp::Shr, VerificationType::Long) => Instruction::Lshr,
            (BinOp::Ushr, VerificationType::Integer) => Instruction::Iushr,
            (BinOp::Ushr, VerificationType::Long) => Instruction::Lushr,
            _ => return Err(AsmError::TypeMismatch),
        };
        let right = if op.is_shift() {
            VerificationType::Integer
        } else {
            ty.clone()
        };
        self.emit(instruction, &[ty.clone(), right], Some(ty.clone()))
    }

    /// Return from the method. `value` is the type left on the stack, or `None` for `void`.
    pub fn return_(&mut self, value: Option<&VerificationType>) -> Result<()> {
        let (instruction, popped) = match value {
            None => (Instruction::Return, Vec::new()),
            Some(VerificationType::Integer) => {
                (Instruction::Ireturn, alloc::vec![VerificationType::Integer])
            }
            Some(VerificationType::Long) => {
                (Instruction::Lreturn, alloc::vec![VerificationType::Long])
            }
            Some(VerificationType::Float) => {
                (Instruction::Freturn, alloc::vec![VerificationType::Float])
            }
            Some(VerificationType::Double) => {
                (Instruction::Dreturn, alloc::vec![VerificationType::Double])
            }
            Some(other) if Self::is_reference(other) => {
                (Instruction::Areturn, alloc::vec![other.clone()])
            }
            Some(_) => return Err(AsmError::TypeMismatch),
        };
        self.emit(instruction, &popped, None)?;
        self.reachable = false;
        Ok(())
    }

    /// Resolve every branch, derive the frame sizes and the `StackMapTable`, and produce the
    /// finished `Code` attribute.
    pub fn finish(self) -> Result<Attribute> {
        let widths = self.resolve_widths()?;
        let offsets = Self::offsets(&self.items, &widths);
        let code = self.materialize(&widths, &offsets)?;
        let frames = self.stack_map(&widths, &offsets)?;
        let exception_table = self.exception_table(&offsets)?;

        let mut attributes = Vec::new();
        if !frames.is_empty() {
            attributes.push(Attribute {
                name_index: self
                    .pool
                    .utf8_index("StackMapTable")
                    .ok_or(AsmError::PoolFull)?,
                body: AttributeBody::StackMapTable(frames),
            });
        }
        let name_index = self.pool.utf8_index("Code").ok_or(AsmError::PoolFull)?;
        Ok(Attribute {
            name_index,
            body: AttributeBody::Code(CodeAttribute {
                max_stack: self.max_stack,
                max_locals: self.max_locals,
                code,
                exception_table,
                attributes,
            }),
        })
    }

    /// The exception table, with every label resolved to a bytecode offset.
    fn exception_table(&self, offsets: &[usize]) -> Result<Vec<ExceptionTableEntry>> {
        let at = |label: Label| -> Result<u16> {
            let bound = self.info(label)?.bound.ok_or(AsmError::UnboundLabel)?;
            u16::try_from(offsets[bound]).map_err(|_| AsmError::TooLarge)
        };
        let mut out = Vec::with_capacity(self.handlers.len());
        for protected in &self.handlers {
            let (start, end) = (at(protected.start)?, at(protected.end)?);
            // An empty range protects nothing, and the JVM would carry the entry anyway. It is not an
            // emitter mistake either: a `finally` splits its range at every inlined copy, so a `try`
            // whose last statement is a `return` closes one range exactly where the next opens.
            if start >= end {
                continue;
            }
            out.push(ExceptionTableEntry::new(
                start,
                end,
                at(protected.handler)?,
                protected.caught,
            ));
        }
        Ok(out)
    }

    // --- emission plumbing -------------------------------------------------

    /// Emit `instruction`, popping `popped` (top of stack last) and pushing `pushed`.
    fn emit(
        &mut self,
        instruction: Instruction,
        popped: &[VerificationType],
        pushed: Option<VerificationType>,
    ) -> Result<()> {
        self.require_reachable()?;
        for expected in popped.iter().rev() {
            let actual = self.state.pop().ok_or(AsmError::StackUnderflow)?;
            if !Self::compatible(expected, &actual) {
                return Err(AsmError::TypeMismatch);
            }
        }
        if let Some(pushed) = pushed {
            self.state.push(pushed);
        }
        self.push_item(instruction);
        Ok(())
    }

    /// Pop a call's arguments (and receiver), then push its result.
    fn invoke(
        &mut self,
        instruction: Instruction,
        receiver: Option<VerificationType>,
        descriptor: &str,
    ) -> Result<()> {
        let parsed = MethodDescriptor::parse(descriptor).map_err(|_| AsmError::BadDescriptor)?;
        let mut popped = Vec::new();
        if let Some(receiver) = receiver {
            popped.push(receiver);
        }
        for param in &parsed.params {
            popped.push(Self::verification_type(self.pool, param)?);
        }
        let pushed = match &parsed.return_type {
            ReturnType::Void => None,
            ReturnType::Type(ty) => Some(Self::verification_type(self.pool, ty)?),
        };
        self.emit(instruction, &popped, pushed)
    }

    fn push_item(&mut self, instruction: Instruction) {
        self.items.push(Item::Fixed(instruction));
        self.note_frame();
    }

    fn note_frame(&mut self) {
        self.max_stack = self.max_stack.max(self.state.stack_words());
        self.max_locals = self.max_locals.max(self.state.slot_count());
    }

    const fn require_reachable(&self) -> Result<()> {
        if self.reachable {
            Ok(())
        } else {
            Err(AsmError::Unreachable)
        }
    }

    /// Merge the current state into `target`'s, recording what arrives there by this jump.
    ///
    /// A *backward* jump is held to a stricter rule than a forward one: the label is already bound,
    /// so the code after it was emitted against the state recorded then. An arrival that would
    /// widen that state has to be rejected — the frame written into the class would no longer
    /// describe what the following instructions assume, and the verifier would refuse the method
    /// with a message far from the emission that caused it.
    ///
    /// "Widen" is [`State::describes`], not inequality. A loop body declares locals of its own, so
    /// the arriving state has slots the label never described; folding them in loses nothing the
    /// frame says, and demanding equality rejected `while (…) { int x = …; }`.
    fn record_arrival(&mut self, target: Label) -> Result<()> {
        let arriving = self.state.clone();
        let object = self.object;
        let info = self.info_mut(target)?;
        let merged = match info.state.take() {
            Some(existing) => {
                let merged = existing
                    .join(&arriving, object)
                    .ok_or(AsmError::IncompatibleFrame)?;
                if info.bound.is_some() && !merged.describes(&existing) {
                    return Err(AsmError::IncompatibleFrame);
                }
                merged
            }
            None => arriving,
        };
        info.state = Some(merged);
        Ok(())
    }

    fn info(&self, label: Label) -> Result<&LabelInfo> {
        self.labels.get(label.0).ok_or(AsmError::UnboundLabel)
    }

    fn info_mut(&mut self, label: Label) -> Result<&mut LabelInfo> {
        self.labels.get_mut(label.0).ok_or(AsmError::UnboundLabel)
    }

    // --- constant-pool helpers ---------------------------------------------

    fn field_ref(&mut self, owner: &str, name: &str, descriptor: &str) -> Result<u16> {
        self.pool
            .field_ref_index(owner, name, descriptor)
            .ok_or(AsmError::PoolFull)
    }

    fn method_ref(
        &mut self,
        owner: &str,
        name: &str,
        descriptor: &str,
        interface_owner: bool,
    ) -> Result<u16> {
        let index = if interface_owner {
            self.pool
                .interface_method_ref_index(owner, name, descriptor)
        } else {
            self.pool.method_ref_index(owner, name, descriptor)
        };
        index.ok_or(AsmError::PoolFull)
    }

    /// `ldc` for a pool index that fits one byte, `ldc_w` otherwise.
    fn load_constant(index: u16) -> Instruction {
        u8::try_from(index).map_or(Instruction::LdcW(index), Instruction::Ldc)
    }

    /// The signed byte distance from `from` to `to`, as a branch operand is measured.
    ///
    /// Widened through `i64` rather than computed in `usize`: a backward jump is negative, and the
    /// two offsets are unsigned. The range check that matters — whether the result fits the
    /// operand — happens at the call site, which knows whether it is filling an `i16` or an `i32`.
    fn distance(from: usize, to: usize) -> Result<i64> {
        let from = i64::try_from(from).map_err(|_| AsmError::TooLarge)?;
        let to = i64::try_from(to).map_err(|_| AsmError::TooLarge)?;
        Ok(to - from)
    }

    /// A one-operand load or store, in the `wide` form when the slot outgrows a byte.
    ///
    /// A method with more than 256 local slots is legal, and reachable here rather than theoretical:
    /// [`Slots`](crate::lower) never reuses a slot, so `max_locals` counts every declaration in the
    /// body instead of the widest live set. Reporting `TooLarge` for that would refuse a program the
    /// class file can express.
    fn wide_or_narrow(
        index: u16,
        narrow: fn(u8) -> Instruction,
        wide: fn(u16) -> WideInstruction,
    ) -> Instruction {
        u8::try_from(index).map_or_else(|_| Instruction::Wide(wide(index)), narrow)
    }

    const fn is_reference(ty: &VerificationType) -> bool {
        matches!(
            ty,
            VerificationType::Object { .. }
                | VerificationType::Null
                | VerificationType::UninitializedThis
                | VerificationType::Uninitialized { .. }
        )
    }

    /// Whether a value of type `actual` may stand where `expected` is due.
    ///
    /// References are checked only for reference-ness: proving one class is assignable to another
    /// is the verifier's job at load time, and duplicating its hierarchy walk here would need a
    /// class hierarchy this layer deliberately does not have.
    fn compatible(expected: &VerificationType, actual: &VerificationType) -> bool {
        if Self::is_reference(expected) {
            Self::is_reference(actual)
        } else {
            expected == actual
        }
    }

    fn object_type(pool: &mut ConstantPool, internal_name: &str) -> Result<VerificationType> {
        Ok(VerificationType::Object {
            cpool_index: pool.class_index(internal_name).ok_or(AsmError::PoolFull)?,
        })
    }

    fn field_verification_type(
        pool: &mut ConstantPool,
        descriptor: &str,
    ) -> Result<VerificationType> {
        let ty = FieldType::parse(descriptor).map_err(|_| AsmError::BadDescriptor)?;
        Self::verification_type(pool, &ty)
    }

    /// The `newarray` `atype` operand for a primitive element (JVMS §6.5 `newarray`).
    const fn array_code(base: BaseType) -> u8 {
        match base {
            BaseType::Boolean => 4,
            BaseType::Char => 5,
            BaseType::Float => 6,
            BaseType::Double => 7,
            BaseType::Byte => 8,
            BaseType::Short => 9,
            BaseType::Int => 10,
            BaseType::Long => 11,
        }
    }

    /// The `*aload` opcode for an element of type `ty`.
    ///
    /// `baload` serves both `byte` and `boolean`: a `boolean[]` is stored one byte per element, and
    /// there is no `zaload`. `caload` and `saload` differ only in how they sign-extend, which is what
    /// keeps `char` unsigned.
    const fn array_load_op(ty: &FieldType) -> Instruction {
        match ty {
            FieldType::Base(BaseType::Long) => Instruction::Laload,
            FieldType::Base(BaseType::Float) => Instruction::Faload,
            FieldType::Base(BaseType::Double) => Instruction::Daload,
            FieldType::Base(BaseType::Byte | BaseType::Boolean) => Instruction::Baload,
            FieldType::Base(BaseType::Char) => Instruction::Caload,
            FieldType::Base(BaseType::Short) => Instruction::Saload,
            FieldType::Base(BaseType::Int) => Instruction::Iaload,
            FieldType::Object(_) | FieldType::Array(_) => Instruction::Aaload,
        }
    }

    /// The `*astore` opcode for an element of type `ty`.
    const fn array_store_op(ty: &FieldType) -> Instruction {
        match ty {
            FieldType::Base(BaseType::Long) => Instruction::Lastore,
            FieldType::Base(BaseType::Float) => Instruction::Fastore,
            FieldType::Base(BaseType::Double) => Instruction::Dastore,
            FieldType::Base(BaseType::Byte | BaseType::Boolean) => Instruction::Bastore,
            FieldType::Base(BaseType::Char) => Instruction::Castore,
            FieldType::Base(BaseType::Short) => Instruction::Sastore,
            FieldType::Base(BaseType::Int) => Instruction::Iastore,
            FieldType::Object(_) | FieldType::Array(_) => Instruction::Aastore,
        }
    }

    /// The verification type a value of field type `ty` has.
    ///
    /// `boolean` / `byte` / `char` / `short` all verify as `Integer`: the JVM has no narrower
    /// stack representation, and the class file's descriptors carry the distinction instead.
    fn verification_type(pool: &mut ConstantPool, ty: &FieldType) -> Result<VerificationType> {
        Ok(match ty {
            FieldType::Base(BaseType::Long) => VerificationType::Long,
            FieldType::Base(BaseType::Float) => VerificationType::Float,
            FieldType::Base(BaseType::Double) => VerificationType::Double,
            FieldType::Base(_) => VerificationType::Integer,
            FieldType::Object(name) => Self::object_type(pool, name)?,
            // An array's `Class` entry is spelled as the array *descriptor*, not a class name.
            FieldType::Array(_) => {
                use alloc::string::ToString as _;
                Self::object_type(pool, &ty.to_string())?
            }
        })
    }

    // --- branch resolution -------------------------------------------------

    /// How many bytes each item occupies, after widening every branch that needs it.
    ///
    /// Widening only ever grows an item, so re-measuring converges: each pass either changes
    /// nothing (done) or widens at least one of a finite set of branches.
    fn resolve_widths(&self) -> Result<Vec<bool>> {
        let mut wide = alloc::vec![false; self.items.len()];
        loop {
            let offsets = Self::offsets(&self.items, &wide);
            let mut changed = false;
            for (index, item) in self.items.iter().enumerate() {
                let Item::Jump { target, .. } = item else {
                    continue;
                };
                if wide[index] {
                    continue;
                }
                let bound = self.info(*target)?.bound.ok_or(AsmError::UnboundLabel)?;
                let delta = Self::distance(offsets[index], offsets[bound])?;
                if i16::try_from(delta).is_err() {
                    wide[index] = true;
                    changed = true;
                }
            }
            if !changed {
                let total = offsets.last().copied().unwrap_or(0);
                if u16::try_from(total).is_err() {
                    return Err(AsmError::TooLarge);
                }
                return Ok(wide);
            }
        }
    }

    /// The byte offset of every item, plus a final entry holding the total code length.
    fn offsets(items: &[Item], wide: &[bool]) -> Vec<usize> {
        let mut out = Vec::with_capacity(items.len() + 1);
        let mut pc = 0;
        for (index, item) in items.iter().enumerate() {
            out.push(pc);
            pc += match item {
                Item::Mark => 0,
                Item::Fixed(instruction) => instruction.encoded_len(pc),
                // Measured through the very instruction `materialize` will build, so the length a
                // pass computes and the bytes `write` produces cannot disagree about the alignment
                // padding — which is the one thing here that depends on `pc`.
                Item::Switch { cases, table, .. } => {
                    Self::switch_shape(cases, *table).encoded_len(pc)
                }
                // `goto` → `goto_w`; a conditional → itself, then a `goto_w` it jumps over.
                Item::Jump { branch, .. } => match (wide[index], branch) {
                    (false, _) => 3,
                    (true, Branch::Always) => 5,
                    (true, _) => 8,
                },
            };
        }
        out.push(pc);
        out
    }

    /// The shape a switch item encodes to, with every offset zeroed.
    ///
    /// One function serves both measurement and emission. A `tableswitch`'s length depends on its
    /// *span* and a `lookupswitch`'s on its arm count, and neither depends on the offsets, so the
    /// zeroed form measures exactly as the filled one does.
    fn switch_shape(cases: &[(i32, Label)], table: bool) -> Instruction {
        if table {
            let (low, high) = Self::span(cases);
            let slots = usize::try_from(i64::from(high) - i64::from(low) + 1).unwrap_or(0);
            Instruction::TableSwitch {
                default: 0,
                low,
                high,
                offsets: alloc::vec![0; slots],
            }
        } else {
            Instruction::LookupSwitch {
                default: 0,
                pairs: cases.iter().map(|&(key, _)| (key, 0)).collect(),
            }
        }
    }

    /// The lowest and highest key of a sorted, non-empty arm list.
    ///
    /// `(0, 0)` for an empty one, which [`prefers_table`](Self::prefers_table) has already routed to
    /// the `lookupswitch` form — so the degenerate span is never encoded.
    fn span(cases: &[(i32, Label)]) -> (i32, i32) {
        let low = cases.first().map_or(0, |&(key, _)| key);
        let high = cases.last().map_or(0, |&(key, _)| key);
        (low, high)
    }

    /// The final instruction list, with every label replaced by a resolved offset.
    fn materialize(&self, wide: &[bool], offsets: &[usize]) -> Result<Vec<Instruction>> {
        let mut out = Vec::with_capacity(self.items.len());
        for (index, item) in self.items.iter().enumerate() {
            match item {
                Item::Mark => {}
                Item::Fixed(instruction) => out.push(instruction.clone()),
                Item::Switch {
                    cases,
                    default,
                    table,
                } => out.push(self.switch_instruction(
                    cases,
                    *default,
                    *table,
                    offsets,
                    offsets[index],
                )?),
                Item::Jump { branch, target, .. } => {
                    let bound = self.info(*target)?.bound.ok_or(AsmError::UnboundLabel)?;
                    let delta = Self::distance(offsets[index], offsets[bound])?;
                    match (wide[index], branch) {
                        (false, branch) => {
                            let short = i16::try_from(delta).map_err(|_| AsmError::TooLarge)?;
                            out.push(branch.instruction(short));
                        }
                        (true, Branch::Always) => {
                            out.push(Instruction::GotoW(
                                i32::try_from(delta).map_err(|_| AsmError::TooLarge)?,
                            ));
                        }
                        (true, branch) => {
                            let inverse = branch.inverse().ok_or(AsmError::TooLarge)?;
                            // Jump past the 3-byte conditional and the 5-byte `goto_w`.
                            out.push(inverse.instruction(8));
                            // The `goto_w` sits 3 bytes into this item, so its own offset is
                            // measured from there.
                            let from_wide = delta - 3;
                            out.push(Instruction::GotoW(
                                i32::try_from(from_wide).map_err(|_| AsmError::TooLarge)?,
                            ));
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// One switch item as its finished instruction, measured from `from`.
    fn switch_instruction(
        &self,
        cases: &[(i32, Label)],
        default: Label,
        table: bool,
        offsets: &[usize],
        from: usize,
    ) -> Result<Instruction> {
        let target = |label: Label| -> Result<i32> {
            let bound = self.info(label)?.bound.ok_or(AsmError::UnboundLabel)?;
            i32::try_from(Self::distance(from, offsets[bound])?).map_err(|_| AsmError::TooLarge)
        };
        let fallback = target(default)?;
        if !table {
            let mut pairs = Vec::with_capacity(cases.len());
            for &(key, label) in cases {
                pairs.push((key, target(label)?));
            }
            return Ok(Instruction::LookupSwitch {
                default: fallback,
                pairs,
            });
        }
        let (low, high) = Self::span(cases);
        let mut slots = Vec::new();
        // A `tableswitch` indexes rather than searches, so every key in the span needs an entry —
        // and a key no arm claimed goes to `default`. That is what lets the dense form cover a span
        // with holes in it, which `prefers_table` bounds to about twice the arm count.
        for key in low..=high {
            let arm = cases.iter().find(|&&(case, _)| case == key);
            slots.push(match arm {
                Some(&(_, label)) => target(label)?,
                None => fallback,
            });
        }
        Ok(Instruction::TableSwitch {
            default: fallback,
            low,
            high,
            offsets: slots,
        })
    }

    /// One `full_frame` per branch target, in ascending offset order.
    ///
    /// "Branch target" includes the sites widening invents: an inverted conditional jumps over its
    /// `goto_w`, and the instruction it lands on is as much a branch target as any label. Omitting
    /// that frame is what a JVM reports as *"Expecting a stackmap frame at branch target N"*.
    fn stack_map(&self, wide: &[bool], offsets: &[usize]) -> Result<Vec<StackMapFrame>> {
        let mut targets = Vec::new();
        for info in &self.labels {
            if !info.targeted {
                continue;
            }
            let bound = info.bound.ok_or(AsmError::UnboundLabel)?;
            let state = info.state.as_ref().ok_or(AsmError::UnreachableLabel)?;
            targets.push((offsets[bound], state));
        }
        for (index, item) in self.items.iter().enumerate() {
            if let Item::Jump {
                branch,
                fallthrough,
                ..
            } = item
                && wide[index]
                && *branch != Branch::Always
            {
                // The widened item is 8 bytes: a 3-byte inverted conditional over a 5-byte
                // `goto_w`. Its landing site is therefore the item's own end.
                targets.push((offsets[index] + 8, fallthrough));
            }
        }
        targets.sort_by_key(|(offset, _)| *offset);
        targets.dedup_by_key(|(offset, _)| *offset);

        let mut frames = Vec::with_capacity(targets.len());
        let mut previous: Option<usize> = None;
        for (offset, state) in targets {
            // The first frame's delta is the offset itself; every later one is measured from one
            // past the previous frame's, which is what makes a delta of 0 mean "the very next
            // instruction" rather than "the same one".
            let delta = match previous {
                None => offset,
                Some(previous) => offset
                    .checked_sub(previous + 1)
                    .ok_or(AsmError::IncompatibleFrame)?,
            };
            let mut locals = state.frame_locals();
            let mut stack = state.frame_stack();
            Self::resolve_markers(&mut locals, offsets)?;
            Self::resolve_markers(&mut stack, offsets)?;
            frames.push(StackMapFrame::Full {
                offset_delta: u16::try_from(delta).map_err(|_| AsmError::TooLarge)?,
                locals,
                stack,
            });
            previous = Some(offset);
        }
        Ok(frames)
    }

    /// Turn every `Uninitialized` marker into the real bytecode offset of its `new`.
    ///
    /// See [`new_object`](Self::new_object): what a state carries is an *item index*, because the
    /// offset does not exist until widening has run. The index came from this assembler's own item
    /// stream, so `offsets` always has an entry for it — indexing directly says so, the way
    /// [`materialize`](Self::materialize) does for a jump target.
    fn resolve_markers(types: &mut [VerificationType], offsets: &[usize]) -> Result<()> {
        for ty in types {
            if let VerificationType::Uninitialized { offset } = ty {
                let resolved = offsets[usize::from(*offset)];
                *offset = u16::try_from(resolved).map_err(|_| AsmError::TooLarge)?;
            }
        }
        Ok(())
    }
}
