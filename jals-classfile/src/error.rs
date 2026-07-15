//! The crate's error type, hand-rolled to avoid a `thiserror` dependency.

use core::fmt;

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
    /// Bytes remained after a structurally complete class file.
    TrailingBytes {
        /// How many bytes were left over.
        remaining: usize,
    },
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
            Self::TrailingBytes { remaining } => {
                write!(
                    f,
                    "{remaining} trailing byte(s) after a complete class file"
                )
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn every_variant_renders_a_descriptive_message() {
        let cases = [
            (
                ClassfileError::UnexpectedEof {
                    offset: 12,
                    needed: 4,
                },
                "unexpected end of input at byte 12: needed 4 more byte(s)",
            ),
            (
                ClassfileError::BadMagic(0x1234_5678),
                "bad magic: expected 0xCAFEBABE, found 0x12345678",
            ),
            (
                ClassfileError::TrailingBytes { remaining: 3 },
                "3 trailing byte(s) after a complete class file",
            ),
            (
                ClassfileError::InvalidConstantTag(99),
                "invalid constant-pool tag: 99",
            ),
            (
                ClassfileError::InvalidOpcode(0xEF),
                "invalid bytecode opcode: 0xEF",
            ),
            (
                ClassfileError::Malformed("boom"),
                "malformed class file: boom",
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(err.to_string(), expected);
        }
    }
}
