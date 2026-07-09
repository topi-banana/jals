//! Generic type signatures (JVMS §4.7.9.1): the grammar carried by the `Signature` attribute, which
//! descriptors cannot express (type variables, type arguments, bounds, wildcards).
//!
//! [`core::fmt::Display`] renders a parsed value back to its signature text, so `parse → render`
//! round-trips. The internal binary names keep the `/` form (`java/util/Map`).

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use serde::{Deserialize, Serialize};

use crate::descriptor::BaseType;
use crate::error::{ClassfileError, Result};

/// A `JavaTypeSignature`: a primitive, a class type, a type variable, or an array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeSignature {
    /// A primitive type.
    Base(BaseType),
    /// A (possibly generic) class or interface type.
    Class(ClassTypeSignature),
    /// `T Identifier ;` — a reference to a type variable.
    TypeVariable(String),
    /// `[ ...` — an array of the component signature.
    Array(Box<Self>),
}

impl TypeSignature {
    /// Whether this signature is exactly `java.lang.Object` (a raw, non-restrictive class bound —
    /// the implicit bound that renderers omit).
    pub fn is_java_lang_object(&self) -> bool {
        matches!(self, Self::Class(c)
            if c.name == "java/lang/Object" && c.suffixes.is_empty() && c.type_arguments.is_empty())
    }
}

/// A `ClassTypeSignature`: an outer class (with optional type arguments) plus any `.`-separated inner
/// classes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassTypeSignature {
    /// The outermost class's internal binary name, including its package (`java/util/Map`).
    pub name: String,
    /// The outer class's type arguments (empty for a raw use).
    pub type_arguments: Vec<TypeArgument>,
    /// Nested-class suffixes (`.Entry<...>`), in order.
    pub suffixes: Vec<SimpleClassTypeSignature>,
}

/// One `.`-separated inner-class step of a [`ClassTypeSignature`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleClassTypeSignature {
    /// The inner class's simple name.
    pub name: String,
    /// Its type arguments.
    pub type_arguments: Vec<TypeArgument>,
}

/// A `TypeArgument`: an unbounded wildcard, an exact type, or a bounded wildcard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeArgument {
    /// `*` — an unbounded wildcard (`?`).
    Any,
    /// An invariant type argument.
    Exact(TypeSignature),
    /// `+ T` — `? extends T`.
    Extends(TypeSignature),
    /// `- T` — `? super T`.
    Super(TypeSignature),
}

/// A `TypeParameter`: its name and its bounds (an optional class bound and any interface bounds).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeParameter {
    /// The parameter's name (`T`, `K`, …).
    pub name: String,
    /// The class bound after the first `:`, if present (absent ⇒ implicitly `Object`).
    pub class_bound: Option<TypeSignature>,
    /// The interface bounds, each introduced by a further `:`.
    pub interface_bounds: Vec<TypeSignature>,
}

/// A `ClassSignature` (JVMS §4.7.9.1): a generic class declaration's parameters and supertypes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassSignature {
    /// The class's own type parameters.
    pub type_parameters: Vec<TypeParameter>,
    /// The (generic) superclass.
    pub superclass: ClassTypeSignature,
    /// The (generic) superinterfaces.
    pub superinterfaces: Vec<ClassTypeSignature>,
}

/// A `MethodSignature`: type parameters, parameter types, a result, and declared throws.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MethodSignature {
    /// The method's own type parameters.
    pub type_parameters: Vec<TypeParameter>,
    /// The parameter types, in order.
    pub parameters: Vec<TypeSignature>,
    /// The return type.
    pub result: ResultSignature,
    /// The declared checked exceptions (only present when one is a type variable or generic).
    pub throws: Vec<ThrowsSignature>,
}

/// A method signature's result: a type or `void`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResultSignature {
    /// `V` — `void`.
    Void,
    /// A value-returning result type.
    Type(TypeSignature),
}

/// One `^`-introduced `throws` clause of a [`MethodSignature`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThrowsSignature {
    /// A thrown class type.
    Class(ClassTypeSignature),
    /// A thrown type variable.
    TypeVariable(String),
}

/// Parse a class signature, e.g. `<T:Ljava/lang/Object;>Ljava/lang/Object;Ljava/util/List<TT;>;`.
pub fn parse_class_signature(s: &str) -> Result<ClassSignature> {
    let mut p = Parser::new(s);
    let sig = p.class_signature()?;
    p.expect_eof()?;
    Ok(sig)
}

/// Parse a method signature, e.g. `(Ljava/lang/Object;I)TV;`.
pub fn parse_method_signature(s: &str) -> Result<MethodSignature> {
    let mut p = Parser::new(s);
    let sig = p.method_signature()?;
    p.expect_eof()?;
    Ok(sig)
}

/// Parse a field signature (a `ReferenceTypeSignature`), e.g. `Ljava/util/List<Ljava/lang/String;>;`.
pub fn parse_field_signature(s: &str) -> Result<TypeSignature> {
    let mut p = Parser::new(s);
    let sig = p.reference_type_signature()?;
    p.expect_eof()?;
    Ok(sig)
}

/// A byte cursor over a signature string. All grammar punctuation is ASCII, so byte indexing never
/// splits a multi-byte identifier character.
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
            Err(ClassfileError::Malformed("signature: unexpected character"))
        }
    }

    const fn expect_eof(&self) -> Result<()> {
        if self.pos == self.s.len() {
            Ok(())
        } else {
            Err(ClassfileError::Malformed("signature: trailing characters"))
        }
    }

    /// Read characters up to (not including) the first byte in `terminators`.
    fn read_until(&mut self, terminators: &[u8]) -> &'a str {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if terminators.contains(&b) {
                break;
            }
            self.pos += 1;
        }
        &self.s[start..self.pos]
    }

    fn class_signature(&mut self) -> Result<ClassSignature> {
        let type_parameters = self.type_parameters_opt()?;
        let superclass = self.class_type_signature()?;
        let mut superinterfaces = Vec::new();
        while self.peek().is_some() {
            superinterfaces.push(self.class_type_signature()?);
        }
        Ok(ClassSignature {
            type_parameters,
            superclass,
            superinterfaces,
        })
    }

    fn method_signature(&mut self) -> Result<MethodSignature> {
        let type_parameters = self.type_parameters_opt()?;
        self.expect(b'(')?;
        let mut parameters = Vec::new();
        while self.peek() != Some(b')') {
            if self.peek().is_none() {
                return Err(ClassfileError::Malformed("signature: unterminated ("));
            }
            parameters.push(self.java_type_signature()?);
        }
        self.expect(b')')?;
        let result = if self.peek() == Some(b'V') {
            self.bump();
            ResultSignature::Void
        } else {
            ResultSignature::Type(self.java_type_signature()?)
        };
        let mut throws = Vec::new();
        while self.peek() == Some(b'^') {
            self.bump();
            if self.peek() == Some(b'T') {
                self.bump();
                let name = self.read_until(b";").to_owned();
                self.expect(b';')?;
                throws.push(ThrowsSignature::TypeVariable(name));
            } else {
                throws.push(ThrowsSignature::Class(self.class_type_signature()?));
            }
        }
        Ok(MethodSignature {
            type_parameters,
            parameters,
            result,
            throws,
        })
    }

    fn type_parameters_opt(&mut self) -> Result<Vec<TypeParameter>> {
        if self.peek() != Some(b'<') {
            return Ok(Vec::new());
        }
        self.bump();
        let mut params = Vec::new();
        while self.peek() != Some(b'>') {
            if self.peek().is_none() {
                return Err(ClassfileError::Malformed("signature: unterminated <"));
            }
            params.push(self.type_parameter()?);
        }
        self.bump();
        Ok(params)
    }

    fn type_parameter(&mut self) -> Result<TypeParameter> {
        let name = self.read_until(b":").to_owned();
        self.expect(b':')?;
        let class_bound = if self.starts_reference_type() {
            Some(self.reference_type_signature()?)
        } else {
            None
        };
        let mut interface_bounds = Vec::new();
        while self.peek() == Some(b':') {
            self.bump();
            interface_bounds.push(self.reference_type_signature()?);
        }
        Ok(TypeParameter {
            name,
            class_bound,
            interface_bounds,
        })
    }

    fn starts_reference_type(&self) -> bool {
        matches!(self.peek(), Some(b'L' | b'T' | b'['))
    }

    fn java_type_signature(&mut self) -> Result<TypeSignature> {
        match self.peek().and_then(BaseType::from_byte) {
            Some(base) => {
                self.bump();
                Ok(TypeSignature::Base(base))
            }
            None => self.reference_type_signature(),
        }
    }

    fn reference_type_signature(&mut self) -> Result<TypeSignature> {
        match self.peek() {
            Some(b'L') => Ok(TypeSignature::Class(self.class_type_signature()?)),
            Some(b'T') => {
                self.bump();
                let name = self.read_until(b";").to_owned();
                self.expect(b';')?;
                Ok(TypeSignature::TypeVariable(name))
            }
            Some(b'[') => {
                self.bump();
                Ok(TypeSignature::Array(Box::new(self.java_type_signature()?)))
            }
            _ => Err(ClassfileError::Malformed(
                "signature: expected a reference type",
            )),
        }
    }

    fn class_type_signature(&mut self) -> Result<ClassTypeSignature> {
        self.expect(b'L')?;
        let name = self.read_until(b"<.;").to_owned();
        let type_arguments = self.type_arguments_opt()?;
        let mut suffixes = Vec::new();
        while self.peek() == Some(b'.') {
            self.bump();
            let name = self.read_until(b"<.;").to_owned();
            let type_arguments = self.type_arguments_opt()?;
            suffixes.push(SimpleClassTypeSignature {
                name,
                type_arguments,
            });
        }
        self.expect(b';')?;
        Ok(ClassTypeSignature {
            name,
            type_arguments,
            suffixes,
        })
    }

    fn type_arguments_opt(&mut self) -> Result<Vec<TypeArgument>> {
        if self.peek() != Some(b'<') {
            return Ok(Vec::new());
        }
        self.bump();
        let mut args = Vec::new();
        while self.peek() != Some(b'>') {
            if self.peek().is_none() {
                return Err(ClassfileError::Malformed("signature: unterminated <"));
            }
            args.push(self.type_argument()?);
        }
        self.bump();
        Ok(args)
    }

    fn type_argument(&mut self) -> Result<TypeArgument> {
        match self.peek() {
            Some(b'*') => {
                self.bump();
                Ok(TypeArgument::Any)
            }
            Some(b'+') => {
                self.bump();
                Ok(TypeArgument::Extends(self.reference_type_signature()?))
            }
            Some(b'-') => {
                self.bump();
                Ok(TypeArgument::Super(self.reference_type_signature()?))
            }
            _ => Ok(TypeArgument::Exact(self.reference_type_signature()?)),
        }
    }
}

impl fmt::Display for TypeSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Base(b) => f.write_str(&b.as_char().to_string()),
            Self::Class(c) => write!(f, "{c}"),
            Self::TypeVariable(name) => write!(f, "T{name};"),
            Self::Array(inner) => write!(f, "[{inner}"),
        }
    }
}

fn write_type_arguments(f: &mut fmt::Formatter<'_>, args: &[TypeArgument]) -> fmt::Result {
    if args.is_empty() {
        return Ok(());
    }
    f.write_str("<")?;
    for arg in args {
        write!(f, "{arg}")?;
    }
    f.write_str(">")
}

impl fmt::Display for ClassTypeSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "L{}", self.name)?;
        write_type_arguments(f, &self.type_arguments)?;
        for suffix in &self.suffixes {
            write!(f, ".{}", suffix.name)?;
            write_type_arguments(f, &suffix.type_arguments)?;
        }
        f.write_str(";")
    }
}

impl fmt::Display for TypeArgument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Any => f.write_str("*"),
            Self::Exact(t) => write!(f, "{t}"),
            Self::Extends(t) => write!(f, "+{t}"),
            Self::Super(t) => write!(f, "-{t}"),
        }
    }
}

fn write_type_parameters(f: &mut fmt::Formatter<'_>, params: &[TypeParameter]) -> fmt::Result {
    if params.is_empty() {
        return Ok(());
    }
    f.write_str("<")?;
    for p in params {
        write!(f, "{}:", p.name)?;
        if let Some(bound) = &p.class_bound {
            write!(f, "{bound}")?;
        }
        for bound in &p.interface_bounds {
            write!(f, ":{bound}")?;
        }
    }
    f.write_str(">")
}

impl fmt::Display for ClassSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_type_parameters(f, &self.type_parameters)?;
        write!(f, "{}", self.superclass)?;
        for i in &self.superinterfaces {
            write!(f, "{i}")?;
        }
        Ok(())
    }
}

impl fmt::Display for MethodSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_type_parameters(f, &self.type_parameters)?;
        f.write_str("(")?;
        for p in &self.parameters {
            write!(f, "{p}")?;
        }
        f.write_str(")")?;
        match &self.result {
            ResultSignature::Void => f.write_str("V")?,
            ResultSignature::Type(t) => write!(f, "{t}")?,
        }
        for t in &self.throws {
            match t {
                ThrowsSignature::Class(c) => write!(f, "^{c}")?,
                ThrowsSignature::TypeVariable(name) => write!(f, "^T{name};")?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_signature_with_type_variable_result() {
        let m = parse_method_signature("(Ljava/lang/Object;I)TV;").unwrap();
        assert_eq!(m.parameters.len(), 2);
        assert_eq!(
            m.result,
            ResultSignature::Type(TypeSignature::TypeVariable("V".to_owned()))
        );
        assert_eq!(m.to_string(), "(Ljava/lang/Object;I)TV;");
    }

    #[test]
    fn generic_map_class_signature() {
        let s = "<K:Ljava/lang/Object;V:Ljava/lang/Object;>Ljava/util/AbstractMap<TK;TV;>;Ljava/util/Map<TK;TV;>;";
        let sig = parse_class_signature(s).unwrap();
        assert_eq!(sig.type_parameters.len(), 2);
        assert_eq!(sig.type_parameters[0].name, "K");
        assert_eq!(sig.superclass.name, "java/util/AbstractMap");
        assert_eq!(sig.superinterfaces.len(), 1);
        assert_eq!(sig.superinterfaces[0].name, "java/util/Map");
        assert_eq!(sig.to_string(), s);
    }

    #[test]
    fn wildcards_and_inner_classes() {
        for s in [
            "Ljava/util/List<*>;",
            "Ljava/util/List<+Ljava/lang/Number;>;",
            "Ljava/util/Map<-TK;TV;>;",
            "Ljava/util/Map<TK;TV;>.Entry<TK;TV;>;",
            "[Ljava/util/List<Ljava/lang/String;>;",
        ] {
            assert_eq!(parse_field_signature(s).unwrap().to_string(), s, "{s}");
        }
    }

    #[test]
    fn bounded_type_parameter() {
        let s = "<T:Ljava/lang/Object;:Ljava/lang/Comparable<TT;>;>Ljava/lang/Object;";
        let sig = parse_class_signature(s).unwrap();
        assert_eq!(sig.type_parameters[0].interface_bounds.len(), 1);
        assert_eq!(sig.to_string(), s);
    }

    #[test]
    fn method_with_throws() {
        let s = "()V^Ljava/io/IOException;^TE;";
        let sig = parse_method_signature(s).unwrap();
        assert_eq!(sig.throws.len(), 2);
        assert_eq!(sig.to_string(), s);
    }

    #[test]
    fn rejects_malformed() {
        assert!(parse_field_signature("Ljava/lang/Object").is_err());
        assert!(parse_field_signature("Q").is_err());
        assert!(parse_method_signature("Ljava/lang/Object;").is_err());
    }
}
