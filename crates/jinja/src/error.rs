//! What went wrong, and where in the template it went wrong.
//!
//! The shape follows minijinja: [`ErrorKind`] is a small `Copy` classification and the sentence a
//! reader sees is the *detail* beside it, so a consumer that only prints the error needs to know
//! nothing about the kinds, and one that branches never has to parse a message.

use alloc::borrow::Cow;
use alloc::string::String;
use core::fmt;

/// Where in a template source something is, counted from 1 in characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Position {
    pub(crate) line: u32,
    pub(crate) column: u32,
}

impl Position {
    pub(crate) const START: Self = Self { line: 1, column: 1 };

    /// Move past one character of the source.
    pub(crate) const fn advance(&mut self, ch: char) {
        if ch == '\n' {
            self.line = self.line.saturating_add(1);
            self.column = 1;
        } else {
            self.column = self.column.saturating_add(1);
        }
    }
}

/// What class of thing went wrong.
///
/// Deliberately `Copy` and free of payload: everything a message needs is in [`Error::detail`], so
/// adding a better sentence never changes this type and a `match` written against it never breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The source could not be parsed: an unclosed tag, an unknown one, a malformed expression.
    SyntaxError,
    /// A value was used in a way its type does not allow — writing a map, iterating a string.
    InvalidOperation,
    /// A value is known and not set, and the [`UndefinedBehavior`] in force refuses it.
    ///
    /// [`UndefinedBehavior`]: crate::UndefinedBehavior
    UndefinedError,
    /// A name nothing defines, under [`Environment::set_strict_variables`].
    ///
    /// This is the one kind minijinja has no counterpart for. It exists because *unknown* and
    /// *undefined* are different mistakes: the first is a typo, the second is a value the author
    /// knows about and can spell a `| default(…)` for.
    ///
    /// [`Environment::set_strict_variables`]: crate::Environment::set_strict_variables
    UnknownVariable,
    /// `{{ x | nope }}` — no filter is registered under that name.
    UnknownFilter,
    /// `{% include "nope" %}`, or [`Environment::get_template`] for a name never added.
    ///
    /// [`Environment::get_template`]: crate::Environment::get_template
    TemplateNotFound,
    /// A filter was called with fewer arguments than it takes.
    MissingArgument,
    /// A filter was called with more arguments than it takes.
    TooManyArguments,
}

impl ErrorKind {
    /// The classification as a short phrase, which is what an error with no detail renders as.
    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::SyntaxError => "syntax error",
            Self::InvalidOperation => "invalid operation",
            Self::UndefinedError => "undefined value",
            Self::UnknownVariable => "unknown variable",
            Self::UnknownFilter => "unknown filter",
            Self::TemplateNotFound => "template not found",
            Self::MissingArgument => "missing argument",
            Self::TooManyArguments => "too many arguments",
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.description())
    }
}

/// A template that failed to parse or to render.
///
/// It carries the position it failed at whenever one is known, and the template's name whenever the
/// template has one — so a consumer renders the whole error rather than assembling a location of
/// its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    detail: Option<Cow<'static, str>>,
    name: Option<String>,
    at: Option<Position>,
}

impl Error {
    /// An error of this kind with the sentence a reader sees.
    pub fn new(kind: ErrorKind, detail: impl Into<Cow<'static, str>>) -> Self {
        Self {
            kind,
            detail: Some(detail.into()),
            name: None,
            at: None,
        }
    }

    /// An error of this kind with no sentence of its own, which renders as the kind itself.
    pub const fn of(kind: ErrorKind) -> Self {
        Self {
            kind,
            detail: None,
            name: None,
            at: None,
        }
    }

    /// What class of thing went wrong.
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The sentence describing this particular failure, if it has one.
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// The name of the template it happened in, if the template has one.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The 1-based line it happened on, if a position is known.
    pub const fn line(&self) -> Option<u32> {
        match self.at {
            Some(at) => Some(at.line),
            None => None,
        }
    }

    /// The 1-based column it happened at, if a position is known.
    pub const fn column(&self) -> Option<u32> {
        match self.at {
            Some(at) => Some(at.column),
            None => None,
        }
    }

    /// Attach a position, keeping the innermost one already attached.
    ///
    /// The evaluator hands positions down from the outside in, so the first one attached is the one
    /// closest to what actually failed.
    pub(crate) fn at(mut self, at: Position) -> Self {
        self.at.get_or_insert(at);
        self
    }

    /// Attach the template's name, keeping the innermost one — an `include` that fails names the
    /// included template rather than the one that included it.
    pub(crate) fn in_template(mut self, name: &str) -> Self {
        if self.name.is_none() && !name.is_empty() {
            self.name = Some(String::from(name));
        }
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(name) = &self.name {
            write!(f, "{name}: ")?;
        }
        if let Some(at) = self.at {
            write!(f, "line {}, column {}: ", at.line, at.column)?;
        }
        f.write_str(
            self.detail
                .as_deref()
                .unwrap_or_else(|| self.kind.description()),
        )
    }
}

impl core::error::Error for Error {}
