//! The crate's error type, hand-rolled to avoid a `thiserror` dependency.

use core::fmt;

use jals_storage::io::IoError;

/// The result of a fallible class-file codec operation.
pub type Result<T> = core::result::Result<T, ClassfileError>;

/// A structural problem encountered while reading a class file. The codec never panics; every
/// malformed input yields one of these instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassfileError {
    /// Reached the end of input while more bytes were expected.
    UnexpectedEof {
        /// The byte offset at which the read was attempted.
        offset: usize,
        /// How many bytes were needed.
        needed: usize,
    },
    /// The leading 4-byte magic was not `0xCAFEBABE`.
    BadMagic(u32),
    /// Bytes remained after a structurally complete class file. (A streaming source cannot
    /// know how many without draining, so no count is carried.)
    TrailingBytes,
    /// The byte source itself failed (host I/O, decompression, ...). Distinct from every other
    /// variant: the input was not malformed, it could not be read.
    Source(IoError),
    /// A constant-pool entry carried a tag byte that is not defined by the JVM spec.
    InvalidConstantTag(u8),
    /// A bytecode (or `wide`-prefixed) instruction used an opcode that is not defined by the JVM spec.
    InvalidOpcode(u8),
    /// A structural inconsistency that is not one of the more specific cases above.
    Malformed(&'static str),
}

impl fmt::Display for ClassfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof { offset, needed } => {
                write!(
                    f,
                    "unexpected end of input at byte {offset}: needed {needed} more byte(s)"
                )
            }
            Self::BadMagic(m) => {
                write!(f, "bad magic: expected 0xCAFEBABE, found {m:#010X}")
            }
            Self::TrailingBytes => {
                write!(f, "trailing byte(s) after a complete class file")
            }
            Self::Source(error) => {
                write!(f, "class-file source read failed: {error}")
            }
            Self::InvalidConstantTag(t) => {
                write!(f, "invalid constant-pool tag: {t}")
            }
            Self::InvalidOpcode(op) => {
                write!(f, "invalid bytecode opcode: {op:#04X}")
            }
            Self::Malformed(s) => write!(f, "malformed class file: {s}"),
        }
    }
}

impl core::error::Error for ClassfileError {}
