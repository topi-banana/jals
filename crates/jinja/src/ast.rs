//! What a parsed template is, before anything is rendered.
//!
//! Crate-internal on purpose. A consumer that wants to know what a template says asks the
//! [`Template`](crate::Template) it got back, and publishing the tree would publish a second,
//! unrendered way to ask the same questions.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use crate::error::Position;
use crate::value::Value;

/// One piece of a template body.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Node {
    /// Literal source, copied through untouched.
    Text(String),
    /// `{{ … }}`.
    Emit { expr: Expr, at: Position },
    /// `{% if %}` … `{% elif %}` … `{% else %}` … `{% endif %}`.
    If {
        arms: Vec<Arm>,
        otherwise: Vec<Self>,
    },
    /// `{% for <binding> in <source> %}` … `{% endfor %}`.
    For {
        binding: String,
        source: Expr,
        at: Position,
        body: Vec<Self>,
    },
    /// `{% include <name> %}`.
    Include { name: Expr, at: Position },
}

/// One `if`/`elif` arm: the condition, where it is, and what it renders.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Arm {
    pub(crate) condition: Expr,
    pub(crate) at: Position,
    pub(crate) body: Vec<Node>,
}

/// How two values are compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl CompareOp {
    /// The operator as it is written, which is what an error naming it says.
    pub(crate) const fn spelling(self) -> &'static str {
        match self {
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
        }
    }
}

/// An expression inside a tag.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Expr {
    /// A name resolved against the loop scope, then the context, then the globals.
    Var {
        name: String,
    },
    /// A literal: a string, a number, `true`, `false`, or `none`.
    Const(Value),
    /// `base.key` and `base[key]`, which are the same access written twice.
    ///
    /// `path` is how the *base* was spelled, so an error can name it; it is [`None`] for a base
    /// with no spelling of its own, such as a parenthesised expression.
    Get {
        base: Box<Self>,
        key: Box<Self>,
        path: Option<String>,
    },
    Not(Box<Self>),
    And(Box<Self>, Box<Self>),
    Or(Box<Self>, Box<Self>),
    Compare {
        op: CompareOp,
        left: Box<Self>,
        right: Box<Self>,
    },
    /// `value | name(args…)`.
    Filter {
        name: String,
        value: Box<Self>,
        args: Vec<Self>,
    },
}
