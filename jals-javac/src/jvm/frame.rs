//! The verifier-visible state at one point in a method body: the operand stack and the local
//! variable slots.
//!
//! This exists so the assembler can derive `max_stack`, `max_locals`, and the `StackMapTable`
//! without a dataflow pass. A generator knows the abstract state at every instruction it emits, so
//! it simply carries that state forward and snapshots it wherever a frame is required.
//!
//! Two JVMS §4.7.4 subtleties are modelled here rather than left to callers:
//!
//! - A `long` / `double` occupies **two** local slots and **two** stack words, but exactly **one**
//!   entry in a stack-map frame's `locals` / `stack` list.
//! - Writing to either half of a wide value destroys the whole value. Slot `n+1` of a `long` at
//!   `n` is [`Slot::Upper`]; overwriting it leaves slot `n` unusable, which a frame must report as
//!   `Top` rather than as a `Long` the verifier would then trust.

use alloc::vec::Vec;

use jals_classfile::VerificationType;

/// One local-variable slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Slot {
    /// Never written, or invalidated by a write to the other half of a wide value.
    Empty,
    /// A value starts here.
    Value(VerificationType),
    /// The second slot of a `long` / `double` that starts in the slot below.
    Upper,
}

/// The abstract state the verifier would compute at one point in the code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct State {
    stack: Vec<VerificationType>,
    slots: Vec<Slot>,
}

impl State {
    /// An empty stack over `slots`, which the caller has already filled with the receiver and
    /// parameters.
    pub(crate) const fn new(slots: Vec<Slot>) -> Self {
        Self {
            stack: Vec::new(),
            slots,
        }
    }

    /// How many stack *words* a value of this type occupies: two for `long` / `double`, one for
    /// everything else.
    pub(crate) const fn words(ty: &VerificationType) -> u16 {
        match ty {
            VerificationType::Long | VerificationType::Double => 2,
            _ => 1,
        }
    }

    /// Whether this type is a reference, and so a candidate for widening at a merge point.
    const fn is_reference(ty: &VerificationType) -> bool {
        matches!(
            ty,
            VerificationType::Object { .. }
                | VerificationType::Null
                | VerificationType::UninitializedThis
                | VerificationType::Uninitialized { .. }
        )
    }

    /// How many values are on the stack. Distinct from [`stack_words`](Self::stack_words): a
    /// `long` is one value occupying two words.
    pub(crate) const fn stack_len(&self) -> usize {
        self.stack.len()
    }

    /// The current operand-stack depth in words.
    pub(crate) fn stack_words(&self) -> u16 {
        self.stack.iter().map(Self::words).sum()
    }

    /// The number of local slots in use — the `max_locals` this state alone would need.
    pub(crate) fn slot_count(&self) -> u16 {
        u16::try_from(self.slots.len()).unwrap_or(u16::MAX)
    }

    pub(crate) fn push(&mut self, ty: VerificationType) {
        self.stack.push(ty);
    }

    pub(crate) fn pop(&mut self) -> Option<VerificationType> {
        self.stack.pop()
    }

    /// The top of the stack without removing it.
    pub(crate) fn peek(&self) -> Option<&VerificationType> {
        self.stack.last()
    }

    /// The type of the value starting at `index`, or `None` when the slot is unwritten or holds
    /// the orphaned upper half of a clobbered wide value.
    pub(crate) fn local(&self, index: u16) -> Option<&VerificationType> {
        match self.slots.get(usize::from(index)) {
            Some(Slot::Value(ty)) => Some(ty),
            _ => None,
        }
    }

    /// Write `ty` into slot `index`, invalidating whatever wide value the write straddles.
    pub(crate) fn set_local(&mut self, index: u16, ty: VerificationType) {
        let index = usize::from(index);
        let width = usize::from(Self::words(&ty));
        if self.slots.len() < index + width {
            self.slots.resize(index + width, Slot::Empty);
        }

        // Overwriting the upper half of a wide value below leaves that value half-written.
        if self.slots[index] == Slot::Upper && index > 0 {
            self.slots[index - 1] = Slot::Empty;
        }
        // Overwriting the lower half of a wide value orphans its upper half. Only the slot just
        // past our own footprint can survive as an orphan; anything inside it is overwritten.
        let last = index + width - 1;
        if let Some(Slot::Value(existing)) = self.slots.get(last)
            && Self::words(existing) == 2
            && let Some(above) = self.slots.get_mut(last + 1)
        {
            *above = Slot::Empty;
        }

        self.slots[index] = Slot::Value(ty);
        if width == 2 {
            self.slots[index + 1] = Slot::Upper;
        }
    }

    /// Replace every `uninitializedThis` with `ty`, which is what running a constructor's
    /// `invokespecial <init>` does to *every* copy of the receiver at once (JVMS §4.10.1.9).
    pub(crate) fn initialize_this(&mut self, ty: &VerificationType) {
        for entry in &mut self.stack {
            if *entry == VerificationType::UninitializedThis {
                *entry = ty.clone();
            }
        }
        for slot in &mut self.slots {
            if *slot == Slot::Value(VerificationType::UninitializedThis) {
                *slot = Slot::Value(ty.clone());
            }
        }
    }

    /// This state's `stack` list in stack-map form: one entry per value, bottom first.
    pub(crate) fn frame_stack(&self) -> Vec<VerificationType> {
        self.stack.clone()
    }

    /// This state's `locals` list in stack-map form: one entry per *value*, with unwritten and
    /// orphaned slots reported as `Top`.
    ///
    /// Trailing `Top`s are dropped. A frame describes a prefix of the local array and the verifier
    /// treats the rest as `Top`, so describing slots nothing has written to is pure noise.
    pub(crate) fn frame_locals(&self) -> Vec<VerificationType> {
        let mut out = Vec::new();
        let mut index = 0;
        while index < self.slots.len() {
            match &self.slots[index] {
                Slot::Value(ty) => {
                    index += usize::from(Self::words(ty));
                    out.push(ty.clone());
                }
                Slot::Empty | Slot::Upper => {
                    index += 1;
                    out.push(VerificationType::Top);
                }
            }
        }
        while out.last() == Some(&VerificationType::Top) {
            out.pop();
        }
        out
    }

    /// The state both `self` and `other` are compatible with, or `None` when no such state exists.
    ///
    /// Two paths reaching one label must agree. They agree exactly in the code this crate emits
    /// today; where they differ only in *which* reference a slot holds, the merge widens to
    /// `object` (the pool index of `java/lang/Object`), which every reference is assignable to.
    /// Widening loses precision, so a later use of the value needs a `checkcast` the emitter is
    /// responsible for — a lowering that knows the source-level static type, which this layer does
    /// not. Any other disagreement (an `int` against a reference, a differing stack depth, a value
    /// still awaiting its constructor) is a generator bug and returns `None` rather than a frame
    /// the verifier would reject in a harder-to-read way.
    pub(crate) fn join(&self, other: &Self, object: u16) -> Option<Self> {
        if self.stack.len() != other.stack.len() {
            return None;
        }
        let stack = self
            .stack
            .iter()
            .zip(&other.stack)
            .map(|(left, right)| Self::join_type(left, right, object))
            .collect::<Option<Vec<_>>>()?;

        let width = self.slots.len().max(other.slots.len());
        let mut slots = Vec::with_capacity(width);
        for index in 0..width {
            let left = self.slots.get(index).unwrap_or(&Slot::Empty);
            let right = other.slots.get(index).unwrap_or(&Slot::Empty);
            slots.push(match (left, right) {
                // A slot the two paths disagree about is simply unusable past the merge, which is
                // what the verifier already assumes of an undescribed slot.
                (Slot::Value(left), Slot::Value(right)) => {
                    Self::join_type(left, right, object).map_or(Slot::Empty, Slot::Value)
                }
                (Slot::Upper, Slot::Upper) => Slot::Upper,
                _ => Slot::Empty,
            });
        }
        Some(Self { stack, slots })
    }

    fn join_type(
        left: &VerificationType,
        right: &VerificationType,
        object: u16,
    ) -> Option<VerificationType> {
        if left == right {
            return Some(left.clone());
        }
        match (left, right) {
            // `null` is assignable to every reference, so the other side survives intact.
            (VerificationType::Null, other) | (other, VerificationType::Null)
                if Self::is_reference(other) =>
            {
                Some(other.clone())
            }
            // Two different initialised references widen to their guaranteed common supertype.
            // `UninitializedThis` / `Uninitialized` are excluded by `is_reference`'s callers here:
            // an object awaiting its constructor cannot be widened, only tracked exactly.
            (VerificationType::Object { .. }, VerificationType::Object { .. }) => {
                Some(VerificationType::Object {
                    cpool_index: object,
                })
            }
            _ => None,
        }
    }
}
