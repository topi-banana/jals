//! Field and method descriptors (JVMS §4.3): the non-generic type grammar used by
//! `descriptor_index` entries. [`core::fmt::Display`] renders a parsed value back to its descriptor
//! text, so `parse → render` round-trips.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{ClassfileError, Result};

/// A primitive (`BaseType`) descriptor (JVMS Table 4.3-A).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BaseType {
    /// `B` — `byte`.
    Byte,
    /// `C` — `char`.
    Char,
    /// `D` — `double`.
    Double,
    /// `F` — `float`.
    Float,
    /// `I` — `int`.
    Int,
    /// `J` — `long`.
    Long,
    /// `S` — `short`.
    Short,
    /// `Z` — `boolean`.
    Boolean,
}

impl BaseType {
    pub(crate) const fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            b'B' => Self::Byte,
            b'C' => Self::Char,
            b'D' => Self::Double,
            b'F' => Self::Float,
            b'I' => Self::Int,
            b'J' => Self::Long,
            b'S' => Self::Short,
            b'Z' => Self::Boolean,
            _ => return None,
        })
    }

    /// The primitive a `newarray` instruction's `atype` operand denotes (JVMS Table
    /// 6.5.newarray-A), or `None` for an invalid code.
    pub const fn from_atype(atype: u8) -> Option<Self> {
        Some(match atype {
            4 => Self::Boolean,
            5 => Self::Char,
            6 => Self::Float,
            7 => Self::Double,
            8 => Self::Byte,
            9 => Self::Short,
            10 => Self::Int,
            11 => Self::Long,
            _ => return None,
        })
    }

    /// The single descriptor character (`Int` → `'I'`).
    pub const fn as_char(self) -> char {
        match self {
            Self::Byte => 'B',
            Self::Char => 'C',
            Self::Double => 'D',
            Self::Float => 'F',
            Self::Int => 'I',
            Self::Long => 'J',
            Self::Short => 'S',
            Self::Boolean => 'Z',
        }
    }

    /// The Java keyword this primitive is spelled with (`Int` → `"int"`).
    pub const fn keyword(self) -> &'static str {
        match self {
            Self::Byte => "byte",
            Self::Char => "char",
            Self::Double => "double",
            Self::Float => "float",
            Self::Int => "int",
            Self::Long => "long",
            Self::Short => "short",
            Self::Boolean => "boolean",
        }
    }
}

/// A `FieldType` descriptor (JVMS §4.3.2): a primitive, a class reference, or an array of one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldType {
    /// A primitive type.
    Base(BaseType),
    /// `L ClassName ;` — a class reference, by internal binary name (`java/lang/Object`).
    Object(String),
    /// `[ ComponentType` — an array; nest for multiple dimensions.
    Array(Box<Self>),
}

/// A method's return descriptor (JVMS §4.3.3): a [`FieldType`] or `void`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReturnType {
    /// `V` — `void`.
    Void,
    /// A value-returning method's type.
    Type(FieldType),
}

/// A parsed method descriptor (JVMS §4.3.3): `( {ParameterDescriptor} ) ReturnDescriptor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MethodDescriptor {
    /// The parameter types, in order.
    pub params: Vec<FieldType>,
    /// The return type.
    pub return_type: ReturnType,
}

impl FieldType {
    /// Parse a field descriptor such as `Ljava/lang/Object;` or `[[I`.
    pub fn parse(s: &str) -> Result<Self> {
        let mut p = Parser::new(s);
        let ty = p.field_type()?;
        p.expect_eof()?;
        Ok(ty)
    }
}

impl MethodDescriptor {
    /// Parse a method descriptor such as `(Ljava/lang/Object;I)V`.
    pub fn parse(s: &str) -> Result<Self> {
        let mut p = Parser::new(s);
        p.expect(b'(')?;
        let mut params = Vec::new();
        while p.peek() != Some(b')') {
            params.push(p.field_type()?);
        }
        p.expect(b')')?;
        let return_type = p.return_type()?;
        p.expect_eof()?;
        Ok(Self {
            params,
            return_type,
        })
    }
}

/// A byte cursor over a descriptor string. Descriptor punctuation is ASCII, so byte indexing never
/// splits a multi-byte class-name character.
struct Parser<'a> {
    s: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    const fn new(s: &'a str) -> Self {
        Parser { s, pos: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.s.as_bytes().get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    fn expect(&mut self, b: u8) -> Result<()> {
        if self.bump() == Some(b) {
            Ok(())
        } else {
            Err(ClassfileError::Malformed(
                "descriptor: unexpected character",
            ))
        }
    }

    const fn expect_eof(&self) -> Result<()> {
        if self.pos == self.s.len() {
            Ok(())
        } else {
            Err(ClassfileError::Malformed("descriptor: trailing characters"))
        }
    }

    fn field_type(&mut self) -> Result<FieldType> {
        match self.peek() {
            Some(b'[') => {
                self.pos += 1;
                Ok(FieldType::Array(Box::new(self.field_type()?)))
            }
            Some(b'L') => {
                self.pos += 1;
                Ok(FieldType::Object(self.class_name()?))
            }
            Some(b) if BaseType::from_byte(b).is_some() => {
                self.pos += 1;
                Ok(FieldType::Base(BaseType::from_byte(b).unwrap()))
            }
            _ => Err(ClassfileError::Malformed(
                "descriptor: expected a field type",
            )),
        }
    }

    fn class_name(&mut self) -> Result<String> {
        let rest = &self.s[self.pos..];
        let end = rest
            .find(';')
            .ok_or(ClassfileError::Malformed("descriptor: unterminated L..;"))?;
        let name = rest[..end].to_owned();
        self.pos += end + 1;
        Ok(name)
    }

    fn return_type(&mut self) -> Result<ReturnType> {
        if self.peek() == Some(b'V') {
            self.pos += 1;
            Ok(ReturnType::Void)
        } else {
            Ok(ReturnType::Type(self.field_type()?))
        }
    }
}

impl fmt::Display for FieldType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Base(b) => write!(f, "{}", b.as_char()),
            Self::Object(name) => write!(f, "L{name};"),
            Self::Array(inner) => write!(f, "[{inner}"),
        }
    }
}

impl fmt::Display for ReturnType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Void => f.write_str("V"),
            Self::Type(ty) => write!(f, "{ty}"),
        }
    }
}

impl fmt::Display for MethodDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("(")?;
        for p in &self.params {
            write!(f, "{p}")?;
        }
        write!(f, "){}", self.return_type)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(s: &str) -> FieldType {
        FieldType::parse(s).unwrap()
    }

    #[test]
    fn primitives_and_objects() {
        assert_eq!(field("I"), FieldType::Base(BaseType::Int));
        assert_eq!(
            field("Ljava/lang/Object;"),
            FieldType::Object("java/lang/Object".to_owned())
        );
    }

    #[test]
    fn nested_arrays() {
        assert_eq!(
            field("[[I"),
            FieldType::Array(Box::new(FieldType::Array(Box::new(FieldType::Base(
                BaseType::Int
            )))))
        );
    }

    #[test]
    fn method_descriptor() {
        let m = MethodDescriptor::parse("(Ljava/lang/Object;I)V").unwrap();
        assert_eq!(m.params.len(), 2);
        assert_eq!(m.return_type, ReturnType::Void);
        assert_eq!(
            MethodDescriptor::parse("()[Ljava/lang/String;")
                .unwrap()
                .return_type,
            ReturnType::Type(FieldType::Array(Box::new(FieldType::Object(
                "java/lang/String".to_owned()
            ))))
        );
    }

    #[test]
    fn rejects_malformed() {
        assert!(FieldType::parse("Ljava/lang/Object").is_err());
        assert!(FieldType::parse("X").is_err());
        assert!(FieldType::parse("II").is_err());
        assert!(MethodDescriptor::parse("Ljava/lang/Object;").is_err());
    }

    #[test]
    fn render_round_trips() {
        for s in ["I", "[[J", "Ljava/util/Map;", "[Ljava/lang/String;", "Z"] {
            assert_eq!(FieldType::parse(s).unwrap().to_string(), s);
        }
        for s in [
            "()V",
            "(Ljava/lang/Object;I)Z",
            "([IJ)Ljava/lang/String;",
            "()[Ljava/lang/Object;",
        ] {
            assert_eq!(MethodDescriptor::parse(s).unwrap().to_string(), s);
        }
    }

    /// The eight primitive descriptor letters, their `newarray` atype codes, and their Java keywords,
    /// paired so each `BaseType` mapping is checked in every direction.
    const PRIMITIVES: &[(BaseType, u8, u8, &str)] = &[
        (BaseType::Byte, b'B', 8, "byte"),
        (BaseType::Char, b'C', 5, "char"),
        (BaseType::Double, b'D', 7, "double"),
        (BaseType::Float, b'F', 6, "float"),
        (BaseType::Int, b'I', 10, "int"),
        (BaseType::Long, b'J', 11, "long"),
        (BaseType::Short, b'S', 9, "short"),
        (BaseType::Boolean, b'Z', 4, "boolean"),
    ];

    #[test]
    fn from_byte_maps_every_primitive_letter() {
        for &(base, letter, _, _) in PRIMITIVES {
            assert_eq!(
                BaseType::from_byte(letter),
                Some(base),
                "letter {}",
                letter as char
            );
            assert_eq!(base.as_char() as u8, letter);
        }
        assert_eq!(BaseType::from_byte(b'V'), None);
        assert_eq!(BaseType::from_byte(b'L'), None);
    }

    #[test]
    fn from_atype_maps_every_newarray_code() {
        for &(base, _, atype, _) in PRIMITIVES {
            assert_eq!(BaseType::from_atype(atype), Some(base), "atype {atype}");
        }
        // The valid codes are 4..=11; everything else is rejected.
        assert_eq!(BaseType::from_atype(3), None);
        assert_eq!(BaseType::from_atype(12), None);
        assert_eq!(BaseType::from_atype(0), None);
    }

    #[test]
    fn keyword_names_every_primitive() {
        for &(base, _, _, keyword) in PRIMITIVES {
            assert_eq!(base.keyword(), keyword);
        }
    }
}
