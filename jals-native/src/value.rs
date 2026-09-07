//! The values that cross the boundary, and what a native function reports when one is wrong.
//!
//! Deliberately not the engine's value type. A package author's crate depends on this one and on
//! nothing else, so the vocabulary here is the *contract* — four numeric shapes plus a reference —
//! and the engine that fills it in lives behind [`NativeHost`](crate::NativeHost).
//!
//! A reference is a [`RefSlot`] and not a pointer: the host owns the references live for the
//! duration of one call and hands out indices into that table. That is what keeps a Java object
//! reachable from Rust without this crate naming a garbage collector, and what makes a slot from
//! one call structurally useless in another.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// A reference argument, as an index into the references the host made live for this call.
///
/// Opaque on purpose. What a slot *points at* is the engine's business; what a package may do
/// with one is exactly the [`NativeHost`](crate::NativeHost) methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RefSlot(u32);

impl RefSlot {
    /// The slot a host hands out for the `index`-th reference it made live.
    ///
    /// Public because the *host* constructs these — `jals-build`'s tinywasm adapter is the only
    /// caller in this workspace, and it is in another crate.
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// The index this slot names, for the host that issued it.
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// One argument or result.
///
/// `Null` is a reference that is null, kept apart from [`Ref`](Self::Ref) rather than folded into
/// an `Option` inside it: a native method reading a null array has to report *that*, and a slot
/// that might not name anything would push the check into every accessor instead of into the
/// match a package already writes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NativeValue {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    /// A non-null reference the host made live for this call.
    Ref(RefSlot),
    /// A null reference.
    Null,
}

impl NativeValue {
    /// The `i32` this value holds, which is also every Java `boolean`, `byte`, `short`, `char`
    /// and `int` — the wasm type they all lower to.
    pub const fn as_i32(self) -> Option<i32> {
        match self {
            Self::I32(value) => Some(value),
            _ => None,
        }
    }

    pub const fn as_i64(self) -> Option<i64> {
        match self {
            Self::I64(value) => Some(value),
            _ => None,
        }
    }

    pub const fn as_f32(self) -> Option<f32> {
        match self {
            Self::F32(value) => Some(value),
            _ => None,
        }
    }

    pub const fn as_f64(self) -> Option<f64> {
        match self {
            Self::F64(value) => Some(value),
            _ => None,
        }
    }

    /// The slot a non-null reference names. `None` for a null one *and* for a number, because a
    /// package that reached for a reference and got a number has the same problem either way.
    pub const fn as_ref_slot(self) -> Option<RefSlot> {
        match self {
            Self::Ref(slot) => Some(slot),
            _ => None,
        }
    }

    /// What this value is, as a name a diagnostic can use.
    pub const fn kind(self) -> &'static str {
        match self {
            Self::I32(_) => "i32",
            Self::I64(_) => "i64",
            Self::F32(_) => "f32",
            Self::F64(_) => "f64",
            Self::Ref(_) => "a reference",
            Self::Null => "null",
        }
    }
}

/// Why a native call could not answer.
///
/// Every variant is a *refusal*, never a Java exception: a wasm host cannot throw one, because a
/// `throw` needs an exception class the module declares. The engine turns one of these into a
/// trap, which is what a JVM would do with a `native` method that failed to link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeError {
    /// The argument in this position was not the shape the binding reads.
    Argument {
        position: usize,
        expected: &'static str,
        found: &'static str,
    },
    /// A reference argument was null where the binding needs an object.
    Null { position: usize },
    /// There is no argument in this position at all. Unreachable through the engine, which matches
    /// the call against the signature first, and reported rather than panicking because a binding
    /// may also be driven directly by a package's own tests.
    MissingArgument { position: usize },
    /// The slot named something that is not a wasm array.
    NotAnArray,
    /// An element index outside the array.
    OutOfBounds { index: u32, len: u32 },
    /// A call back into the module did not complete. Carries whatever the engine said.
    Call(String),
    /// Anything a package itself wants to report.
    Message(String),
}

impl NativeError {
    /// The refusal a binding reports when its `position`-th argument is not what it reads.
    ///
    /// Written as a constructor rather than left to the struct literal so the `found` half is
    /// always the value's own [`kind`](NativeValue::kind) — a hand-written `found` is how the two
    /// halves of one message stop describing the same value.
    pub const fn argument(position: usize, expected: &'static str, found: NativeValue) -> Self {
        Self::Argument {
            position,
            expected,
            found: found.kind(),
        }
    }
}

impl fmt::Display for NativeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Argument {
                position,
                expected,
                found,
            } => write!(
                f,
                "argument {position} is {found}, and {expected} was needed"
            ),
            Self::Null { position } => write!(f, "argument {position} is null"),
            Self::MissingArgument { position } => write!(f, "there is no argument {position}"),
            Self::NotAnArray => f.write_str("the reference is not an array"),
            Self::OutOfBounds { index, len } => {
                write!(f, "index {index} is outside an array of {len}")
            }
            Self::Call(message) => write!(f, "the call back into the module failed: {message}"),
            Self::Message(message) => f.write_str(message),
        }
    }
}

impl core::error::Error for NativeError {}

/// The result buffer a binding writes into.
///
/// A wasm function's results are positional and their count is fixed by the signature, so the
/// engine hands a slice that is already the right length and a binding fills it. `Results` exists
/// so a binding that returns one value writes `results.set(0, …)` rather than indexing a slice it
/// would have to bounds-check itself.
pub struct Results<'a> {
    slots: &'a mut [NativeValue],
}

impl<'a> Results<'a> {
    /// Wrap the slice the engine allocated for one call's results.
    pub const fn new(slots: &'a mut [NativeValue]) -> Self {
        Self { slots }
    }

    /// How many results the signature declares.
    pub const fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the signature declares none — a `void` native method.
    pub const fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Write one result. A position past the declared count is ignored rather than a panic: the
    /// engine re-checks every result against the signature before it hands one back, so a binding
    /// that writes too many is caught there with both types in hand.
    pub fn set(&mut self, position: usize, value: NativeValue) {
        if let Some(slot) = self.slots.get_mut(position) {
            *slot = value;
        }
    }
}

/// The arguments one call received.
///
/// A thin reader over the slice so a binding says what it wants (`args.i32(0)?`) instead of
/// matching a `NativeValue` and inventing its own error text — the refusal and the position it
/// names then read the same for every package.
pub struct Args<'a> {
    values: &'a [NativeValue],
}

impl<'a> Args<'a> {
    /// Wrap the arguments the engine decoded for one call.
    pub const fn new(values: &'a [NativeValue]) -> Self {
        Self { values }
    }

    /// How many arguments the signature declares.
    pub const fn len(&self) -> usize {
        self.values.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The raw value at `position`, or a refusal naming the position.
    pub fn value(&self, position: usize) -> Result<NativeValue, NativeError> {
        self.values
            .get(position)
            .copied()
            .ok_or(NativeError::MissingArgument { position })
    }

    /// The `i32` at `position` — every Java `boolean`, `byte`, `short`, `char` and `int`.
    pub fn i32(&self, position: usize) -> Result<i32, NativeError> {
        let value = self.value(position)?;
        value
            .as_i32()
            .ok_or_else(|| NativeError::argument(position, "an i32", value))
    }

    pub fn i64(&self, position: usize) -> Result<i64, NativeError> {
        let value = self.value(position)?;
        value
            .as_i64()
            .ok_or_else(|| NativeError::argument(position, "an i64", value))
    }

    pub fn f32(&self, position: usize) -> Result<f32, NativeError> {
        let value = self.value(position)?;
        value
            .as_f32()
            .ok_or_else(|| NativeError::argument(position, "an f32", value))
    }

    pub fn f64(&self, position: usize) -> Result<f64, NativeError> {
        let value = self.value(position)?;
        value
            .as_f64()
            .ok_or_else(|| NativeError::argument(position, "an f64", value))
    }

    /// The non-null reference at `position`. A null one is its own refusal, because "you were
    /// handed `null`" and "you were handed a number" are different mistakes with different fixes.
    pub fn reference(&self, position: usize) -> Result<RefSlot, NativeError> {
        match self.value(position)? {
            NativeValue::Ref(slot) => Ok(slot),
            NativeValue::Null => Err(NativeError::Null { position }),
            other => Err(NativeError::argument(position, "a reference", other)),
        }
    }

    /// Every argument, for a binding that would rather match the slice itself.
    pub const fn values(&self) -> &'a [NativeValue] {
        self.values
    }
}

/// A canonical byte string, appended to by whatever describes itself into a provenance fold.
///
/// This crate computes no digest — it has no dependency to compute one with, and the consumer
/// that memoizes a compile already owns the fold its cache keys are built from. What it owes that
/// consumer is a byte string that changes whenever anything a compile observed changed, and that
/// is what [`Provenance`] accumulates.
#[derive(Debug, Default)]
pub struct Provenance {
    bytes: Vec<u8>,
}

impl Provenance {
    pub const fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    /// Append one length-prefixed field. Length-prefixed rather than separated, because a
    /// separator has to be a byte no field contains and Java source contains every byte.
    pub fn field(&mut self, bytes: &[u8]) -> &mut Self {
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        self.bytes.extend_from_slice(&len.to_be_bytes());
        self.bytes.extend_from_slice(bytes);
        self
    }

    /// Append a `u32`, big-endian.
    pub fn number(&mut self, value: u32) -> &mut Self {
        self.bytes.extend_from_slice(&value.to_be_bytes());
        self
    }

    /// The accumulated bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}
