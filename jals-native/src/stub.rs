//! Java stubs generated from declarations, for the compiler.
//!
//! A declaration states an API with no bodies, and the wasm target still has to compile *something*
//! for a package that ships no Java: the class's struct type, its `static` state, and one host
//! import per `native` method. This module renders the declarations back into the smallest Java
//! that produces exactly that — every method `native`, no fields, no bodies — so the existing
//! front end lowers it with no second path in the compiler.
//!
//! # What a stub can and cannot carry
//!
//! A body is the one thing a declaration does not have, so every method a stub carries must be
//! [`native`](crate::Modifiers::is_native): the package's Rust half supplies it, and the stub's
//! only job is to spell the signature the import is keyed by. A method without that flag is
//! refused rather than given an empty body, because an empty body is a wrong answer where a
//! refusal is a true one. For the same reason only **top-level classes** are rendered: a nested
//! type is a second file with its own name, and a stub that guessed where the package ends and the
//! enclosing type begins would be guessing at the one thing the model states.
//!
//! The text is an *artifact*. It is never a checked-in file and never read by the index — the
//! declarations are — so nothing but the compiler and its cache key ever sees it.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use core::fmt::Write as _;

use crate::declaration::{DeclaredType, Member, MemberKind, TypeKind, TypeRef};

/// Why a declaration could not be rendered as a Java stub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StubError {
    /// Only a class can be a stub: an interface's methods have no host import to become.
    NotAClass {
        /// The declaration's fully-qualified name.
        fqn: String,
    },
    /// A nested type cannot be a top-level compilation unit.
    Nested {
        /// The declaration's fully-qualified name.
        fqn: String,
    },
    /// A member kind a stub has no syntax for — a field, a constructor, an enum constant.
    UnsupportedMember {
        /// The declaring type.
        owner: String,
        /// The member's name.
        member: String,
    },
    /// A method with no body and no `native`: the stub has nothing to write.
    MissingBody {
        /// The declaring type.
        owner: String,
        /// The method's name.
        member: String,
    },
    /// A declared type a stub cannot spell — a constructor's `Unknown`, or a gap in the model.
    Unrepresentable {
        /// The declaring type.
        owner: String,
        /// The member the type belongs to.
        member: String,
    },
}

impl fmt::Display for StubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAClass { fqn } => {
                write!(
                    f,
                    "`{fqn}` is not a class, and only a class can carry host imports"
                )
            }
            Self::Nested { fqn } => write!(
                f,
                "`{fqn}` is a nested type, which cannot be a top-level compilation unit"
            ),
            Self::UnsupportedMember { owner, member } => write!(
                f,
                "`{owner}.{member}` is a member a declaration stub cannot express"
            ),
            Self::MissingBody { owner, member } => {
                write!(f, "`{owner}.{member}` declares no body and is not `native`")
            }
            Self::Unrepresentable { owner, member } => write!(
                f,
                "`{owner}.{member}` has a type with no spelling in a stub"
            ),
        }
    }
}

impl core::error::Error for StubError {}

impl DeclaredType {
    /// Render each declaration as one Java compilation unit, as `(path, text)`.
    ///
    /// One unit per declaration, because a nested type has no compilation unit of its own — the
    /// caller numbers the paths and parses them exactly as it parses a package's Java.
    pub fn java_stubs(declarations: &[Self]) -> Result<Vec<(String, String)>, StubError> {
        declarations.iter().map(Self::stub).collect()
    }

    /// Render this declaration as a Java compilation unit.
    fn stub(&self) -> Result<(String, String), StubError> {
        if self.kind != TypeKind::Class {
            return Err(StubError::NotAClass {
                fqn: self.fqn.to_string(),
            });
        }
        let simple = self.fqn.rsplit('.').next().unwrap_or(&self.fqn);
        let expected = if self.package.is_empty() {
            simple.to_owned()
        } else {
            format!("{}.{simple}", self.package)
        };
        if self.fqn != expected {
            return Err(StubError::Nested {
                fqn: self.fqn.to_string(),
            });
        }

        let mut out = String::new();
        if !self.package.is_empty() {
            let _ = writeln!(out, "package {};", self.package);
        }
        let _ = writeln!(out, "public class {simple} {{");
        for member in &self.members {
            if member.kind != MemberKind::Method {
                return Err(StubError::UnsupportedMember {
                    owner: self.fqn.to_string(),
                    member: member.name.to_string(),
                });
            }
            self.render_method(member, &mut out)?;
        }
        out.push_str("}\n");
        Ok((format!("{}.java", self.fqn.replace('.', "/")), out))
    }

    /// One `native` method declaration.
    fn render_method(&self, member: &Member, out: &mut String) -> Result<(), StubError> {
        let owner = self.fqn.to_string();
        let member_name = member.name.to_string();
        if !member.modifiers.is_native {
            return Err(StubError::MissingBody {
                owner,
                member: member_name,
            });
        }
        let unrepresentable = || StubError::Unrepresentable {
            owner: self.fqn.to_string(),
            member: member.name.to_string(),
        };

        out.push_str("    ");
        if member.modifiers.is_private {
            out.push_str("private ");
        } else if member.modifiers.is_public {
            out.push_str("public ");
        }
        if member.modifiers.is_static {
            out.push_str("static ");
        }
        out.push_str("native ");
        if !member.type_params.is_empty() {
            out.push('<');
            for (position, param) in member.type_params.iter().enumerate() {
                if position > 0 {
                    out.push_str(", ");
                }
                out.push_str(&param.name);
                if let Some(first) = param.bounds.first() {
                    out.push_str(" extends ");
                    out.push_str(&Self::render_type(first).ok_or_else(unrepresentable)?);
                }
            }
            out.push('>');
            out.push(' ');
        }
        out.push_str(&Self::render_type(&member.ty).ok_or_else(unrepresentable)?);
        out.push(' ');
        out.push_str(&member.name);
        out.push('(');
        for (position, param) in member.params.iter().enumerate() {
            if position > 0 {
                out.push_str(", ");
            }
            let last = position + 1 == member.params.len();
            let rendered = Self::render_type(&param.ty).ok_or_else(unrepresentable)?;
            if member.varargs && last {
                // `T...` is the declaration of a `T[]`, which is what the descriptor says; a stub
                // that wrote `T[]` would be a different method.
                let array = rendered.strip_suffix("[]").ok_or_else(unrepresentable)?;
                out.push_str(array);
                out.push_str("...");
            } else {
                out.push_str(&rendered);
            }
            out.push(' ');
            match &param.name {
                Some(name) => out.push_str(name),
                None => {
                    let _ = write!(out, "arg{position}");
                }
            }
        }
        out.push(')');
        if !member.throws.is_empty() {
            out.push_str(" throws ");
            for (position, thrown) in member.throws.iter().enumerate() {
                if position > 0 {
                    out.push_str(", ");
                }
                out.push_str(&Self::render_type(thrown).ok_or_else(unrepresentable)?);
            }
        }
        out.push_str(";\n");
        Ok(())
    }

    /// A type's Java spelling, or `None` for a type a stub cannot write.
    fn render_type(ty: &TypeRef) -> Option<String> {
        let (base, dims, args) = match ty {
            TypeRef::Primitive { keyword, dims } => (keyword.as_ref().to_owned(), *dims, &[][..]),
            TypeRef::Void => ("void".to_owned(), 0, &[][..]),
            TypeRef::Named { fqn, dims, args } => (fqn.as_ref().to_owned(), *dims, args.as_slice()),
            TypeRef::Unknown => return None,
        };
        let mut out = String::with_capacity(base.len() + 2);
        out.push_str(&base);
        if !args.is_empty() {
            out.push('<');
            for (position, arg) in args.iter().enumerate() {
                if position > 0 {
                    out.push_str(", ");
                }
                out.push_str(&Self::render_type(arg)?);
            }
            out.push('>');
        }
        for _ in 0..dims {
            out.push_str("[]");
        }
        Some(out)
    }
}
