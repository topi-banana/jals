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
//! widening. It only ever grows, so it terminates.
//!
//! `goto` widens to `goto_w`. The conditionals have no wide form, so they invert and jump around a
//! `goto_w` instead: `if_icmplt far` becomes `if_icmpge past; goto_w far; past:`.
//!
//! # The stack map is snapshotted, not inferred
//!
//! Class files at major version 50 and above must carry a `StackMapTable`, and computing one from
//! finished bytecode means a dataflow analysis over the control-flow graph. A generator never needs
//! that: it knows the abstract state at every instruction it emits, so binding a label records the
//! state then and there. Every frame is written as a `full_frame` (JVMS §4.7.4 tag 255), which can
//! express any state and removes the same/chop/append/delta selection problem entirely.

use alloc::vec::Vec;

use jals_classfile::{
    Attribute, AttributeBody, BaseType, CodeAttribute, ConstantPool, FieldType, Instruction,
    MethodDescriptor, ReturnType, StackMapFrame, VerificationType,
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

    /// How many stack values the branch consumes, and whether they are references.
    const fn operands(self) -> (usize, bool) {
        match self {
            Self::Always => (0, false),
            Self::IntZero(_) => (1, false),
            Self::IntCmp(_) => (2, false),
            Self::RefSame(_) => (2, true),
            Self::RefNull(_) => (1, true),
        }
    }
}

/// A binary arithmetic or bitwise operator, resolved to an opcode by the type it is applied to.
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
    /// A label's position. Occupies no bytes; which label it is lives in [`LabelInfo::bound`].
    Mark,
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
    pub(crate) fn stack_top(&self) -> Option<VerificationType> {
        self.state.peek().cloned()
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

    /// Branch to `label`. `Branch::Always` ends the basic block.
    pub fn branch(&mut self, branch: Branch, target: Label) -> Result<()> {
        self.require_reachable()?;
        let (operands, reference) = branch.operands();
        for _ in 0..operands {
            let popped = self.state.pop().ok_or(AsmError::StackUnderflow)?;
            if reference != Self::is_reference(&popped) {
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
    pub(crate) fn const_float(&mut self, value: f32) -> Result<()> {
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
    pub(crate) fn const_double(&mut self, value: f64) -> Result<()> {
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
            (VerificationType::Integer, _) => Instruction::Iload(Self::slot(index)?),
            (VerificationType::Long, 0) => Instruction::Lload0,
            (VerificationType::Long, 1) => Instruction::Lload1,
            (VerificationType::Long, 2) => Instruction::Lload2,
            (VerificationType::Long, 3) => Instruction::Lload3,
            (VerificationType::Long, _) => Instruction::Lload(Self::slot(index)?),
            (VerificationType::Float, 0) => Instruction::Fload0,
            (VerificationType::Float, 1) => Instruction::Fload1,
            (VerificationType::Float, 2) => Instruction::Fload2,
            (VerificationType::Float, 3) => Instruction::Fload3,
            (VerificationType::Float, _) => Instruction::Fload(Self::slot(index)?),
            (VerificationType::Double, 0) => Instruction::Dload0,
            (VerificationType::Double, 1) => Instruction::Dload1,
            (VerificationType::Double, 2) => Instruction::Dload2,
            (VerificationType::Double, 3) => Instruction::Dload3,
            (VerificationType::Double, _) => Instruction::Dload(Self::slot(index)?),
            (other, _) if Self::is_reference(other) => match index {
                0 => Instruction::Aload0,
                1 => Instruction::Aload1,
                2 => Instruction::Aload2,
                3 => Instruction::Aload3,
                _ => Instruction::Aload(Self::slot(index)?),
            },
            _ => return Err(AsmError::TypeMismatch),
        };
        self.emit(instruction, &[], Some(ty))
    }

    /// Pop the top of the stack into local slot `index`.
    pub fn store(&mut self, index: u16) -> Result<()> {
        let ty = self.state.peek().ok_or(AsmError::StackUnderflow)?.clone();
        let instruction = match (&ty, index) {
            (VerificationType::Integer, 0) => Instruction::Istore0,
            (VerificationType::Integer, 1) => Instruction::Istore1,
            (VerificationType::Integer, 2) => Instruction::Istore2,
            (VerificationType::Integer, 3) => Instruction::Istore3,
            (VerificationType::Integer, _) => Instruction::Istore(Self::slot(index)?),
            (VerificationType::Long, 0) => Instruction::Lstore0,
            (VerificationType::Long, 1) => Instruction::Lstore1,
            (VerificationType::Long, 2) => Instruction::Lstore2,
            (VerificationType::Long, 3) => Instruction::Lstore3,
            (VerificationType::Long, _) => Instruction::Lstore(Self::slot(index)?),
            (VerificationType::Float, 0) => Instruction::Fstore0,
            (VerificationType::Float, 1) => Instruction::Fstore1,
            (VerificationType::Float, 2) => Instruction::Fstore2,
            (VerificationType::Float, 3) => Instruction::Fstore3,
            (VerificationType::Float, _) => Instruction::Fstore(Self::slot(index)?),
            (VerificationType::Double, 0) => Instruction::Dstore0,
            (VerificationType::Double, 1) => Instruction::Dstore1,
            (VerificationType::Double, 2) => Instruction::Dstore2,
            (VerificationType::Double, 3) => Instruction::Dstore3,
            (VerificationType::Double, _) => Instruction::Dstore(Self::slot(index)?),
            (other, _) if Self::is_reference(other) => match index {
                0 => Instruction::Astore0,
                1 => Instruction::Astore1,
                2 => Instruction::Astore2,
                3 => Instruction::Astore3,
                _ => Instruction::Astore(Self::slot(index)?),
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
        let receiver = Self::object_type(self.pool, owner)?;
        self.invoke(
            Instruction::InvokeSpecial(index),
            Some(receiver),
            descriptor,
        )?;
        // Running a constructor on `this` initialises every copy of it at once, wherever it is
        // held. Until this happens the verifier refuses to let the value be used for anything but
        // another `<init>` call, which is what makes a leaked half-built object impossible.
        if name == "<init>"
            && let Some(initialized) = self.initialized_this.clone()
        {
            self.state.initialize_this(&initialized);
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

    /// Duplicate the top stack value. Only defined for a one-word value; `long` / `double` need
    /// `dup2`, which arrives with the wider arithmetic surface.
    pub fn dup(&mut self) -> Result<()> {
        let ty = self.state.peek().ok_or(AsmError::StackUnderflow)?.clone();
        if State::words(&ty) != 1 {
            return Err(AsmError::TypeMismatch);
        }
        self.emit(Instruction::Dup, &[], Some(ty))
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

    /// Apply `op` to the two values on top, which must both be `ty`.
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
            _ => return Err(AsmError::TypeMismatch),
        };
        self.emit(instruction, &[ty.clone(), ty.clone()], Some(ty.clone()))
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
                exception_table: Vec::new(),
                attributes,
            }),
        })
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
    fn record_arrival(&mut self, target: Label) -> Result<()> {
        let arriving = self.state.clone();
        let object = self.object;
        let info = self.info_mut(target)?;
        let merged = match info.state.take() {
            Some(existing) => {
                let merged = existing
                    .join(&arriving, object)
                    .ok_or(AsmError::IncompatibleFrame)?;
                if info.bound.is_some() && merged != existing {
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

    fn slot(index: u16) -> Result<u8> {
        u8::try_from(index).map_err(|_| AsmError::TooLarge)
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

    /// The final instruction list, with every label replaced by a resolved offset.
    fn materialize(&self, wide: &[bool], offsets: &[usize]) -> Result<Vec<Instruction>> {
        let mut out = Vec::with_capacity(self.items.len());
        for (index, item) in self.items.iter().enumerate() {
            match item {
                Item::Mark => {}
                Item::Fixed(instruction) => out.push(instruction.clone()),
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
            frames.push(StackMapFrame::Full {
                offset_delta: u16::try_from(delta).map_err(|_| AsmError::TooLarge)?,
                locals: state.frame_locals(),
                stack: state.frame_stack(),
            });
            previous = Some(offset);
        }
        Ok(frames)
    }
}
