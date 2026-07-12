//! A small expression / statement IR for a reconstructed method body, plus its rendering to Java.
//!
//! The stack machine in [`crate::body`] folds bytecode into these trees; rendering parenthesizes
//! conservatively (any binary sub-expression used as an operand or receiver is wrapped), so the
//! emitted Java always groups the way the bytecode evaluated — never mis-associating an operator.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// A reconstructed Java expression.
pub(crate) enum Expr {
    /// The receiver `this`.
    This,
    /// A local variable / parameter, by source name.
    Local(String),
    /// A literal already rendered as Java source (`42`, `"s"`, `null`, `Foo.class`).
    Literal(String),
    /// A bare type name, the receiver of a static field access or static call (`java.lang.System`).
    Type(String),
    /// Field access `recv.name`.
    Field { recv: Box<Self>, name: String },
    /// Method call `recv.name(args)`, or `name(args)` when `recv` is `None`.
    Call {
        recv: Option<Box<Self>>,
        name: String,
        args: Vec<Self>,
    },
    /// Object creation `new ty(args)`.
    New { ty: String, args: Vec<Self> },
    /// A binary operation `lhs op rhs`.
    Binary {
        op: &'static str,
        lhs: Box<Self>,
        rhs: Box<Self>,
    },
    /// A prefix unary operation `op expr` (`-`, `~`).
    Unary { op: &'static str, expr: Box<Self> },
    /// A cast `(ty) expr`.
    Cast { ty: String, expr: Box<Self> },
    /// `expr.length` of an array.
    ArrayLength(Box<Self>),
    /// An uninitialized `new` reference on the operand stack (between `new` and its
    /// `invokespecial <init>`). Never rendered — it collapses into [`Expr::New`] once the
    /// constructor runs; if one somehow survives, it renders as a no-arg `new` as a safety net.
    Uninitialized(String),
}

/// A reconstructed Java statement.
pub(crate) enum Stmt {
    /// A bare expression statement `expr;` (a discarded call).
    Expr(Expr),
    /// A hoisted, uninitialized local declaration `ty name;`. Every local a method stores into is
    /// declared once at the method-body top (so a local written inside a branch and read after the
    /// join stays in scope), and each store becomes a plain [`Stmt::Assign`].
    Declare { ty: String, name: String },
    /// `return;` or `return expr;`.
    Return(Option<Expr>),
    /// `target = value;` (an assignment to a field or local).
    Assign { target: Expr, value: Expr },
    /// `throw expr;`.
    Throw(Expr),
    /// A constructor's `super(args);`.
    SuperCall(Vec<Expr>),
    /// A constructor's `this(args);`.
    ThisCall(Vec<Expr>),
    /// An `if (cond) { then } else { els }` (an empty `els` renders as a plain `if`).
    If {
        cond: Expr,
        then: Vec<Self>,
        els: Vec<Self>,
    },
    /// A `while (cond) { body }` (a top-test loop).
    While { cond: Expr, body: Vec<Self> },
    /// A `do { body } while (cond);` (a bottom-test loop).
    DoWhile { body: Vec<Self>, cond: Expr },
}

impl Stmt {
    /// Render a statement tree to indented Java lines. Top-level statements are at indent 0 (the
    /// caller adds the method-body indentation); a nested block indents its contents four spaces
    /// further.
    pub(crate) fn render_block(stmts: &[Self]) -> Vec<String> {
        let mut out = Vec::new();
        for stmt in stmts {
            stmt.render_into(0, &mut out);
        }
        out
    }

    fn render_into(&self, indent: usize, out: &mut Vec<String>) {
        let pad = " ".repeat(indent);
        match self {
            Self::If { cond, then, els } => {
                out.push(format!("{pad}if ({}) {{", cond.render()));
                for s in then {
                    s.render_into(indent + 4, out);
                }
                if !els.is_empty() {
                    out.push(format!("{pad}}} else {{"));
                    for s in els {
                        s.render_into(indent + 4, out);
                    }
                }
                out.push(format!("{pad}}}"));
            }
            Self::While { cond, body } => {
                out.push(format!("{pad}while ({}) {{", cond.render()));
                for s in body {
                    s.render_into(indent + 4, out);
                }
                out.push(format!("{pad}}}"));
            }
            Self::DoWhile { body, cond } => {
                out.push(format!("{pad}do {{"));
                for s in body {
                    s.render_into(indent + 4, out);
                }
                out.push(format!("{pad}}} while ({});", cond.render()));
            }
            simple => out.push(format!("{pad}{}", simple.render_simple())),
        }
    }

    /// Render a non-`If` statement to a single line of Java (terminated with `;`).
    fn render_simple(&self) -> String {
        match self {
            Self::Expr(e) => format!("{};", e.render()),
            Self::Declare { ty, name } => format!("{ty} {name};"),
            Self::Return(None) => "return;".to_string(),
            Self::Return(Some(e)) => format!("return {};", e.render()),
            Self::Assign { target, value } => {
                format!("{} = {};", target.render(), value.render())
            }
            Self::Throw(e) => format!("throw {};", e.render()),
            Self::SuperCall(args) => format!("super({});", Expr::render_args(args)),
            Self::ThisCall(args) => format!("this({});", Expr::render_args(args)),
            Self::If { .. } | Self::While { .. } | Self::DoWhile { .. } => {
                unreachable!("block statements are rendered by render_into")
            }
        }
    }
}

impl Expr {
    /// A literal expression from already-rendered Java source text.
    pub(crate) fn lit(text: impl Into<String>) -> Self {
        Self::Literal(text.into())
    }

    /// Render an expression to Java source.
    pub(crate) fn render(&self) -> String {
        match self {
            Self::This => "this".into(),
            Self::Local(name) | Self::Type(name) => name.clone(),
            Self::Literal(text) => text.clone(),
            Self::Field { recv, name } => format!("{}.{name}", recv.receiver()),
            Self::Call { recv, name, args } => recv.as_ref().map_or_else(
                || format!("{name}({})", Self::render_args(args)),
                |r| format!("{}.{name}({})", r.receiver(), Self::render_args(args)),
            ),
            Self::New { ty, args } => format!("new {ty}({})", Self::render_args(args)),
            Self::Binary { op, lhs, rhs } => format!("{} {op} {}", lhs.operand(), rhs.operand()),
            Self::Unary { op, expr } => format!("{op}{}", expr.operand()),
            Self::Cast { ty, expr } => format!("({ty}) {}", expr.operand()),
            Self::ArrayLength(a) => format!("{}.length", a.receiver()),
            Self::Uninitialized(ty) => format!("new {ty}()"),
        }
    }

    /// Render an operand of a binary / unary / cast, wrapping a binary sub-expression in parentheses
    /// so the grouping the bytecode evaluated is preserved (cast / unary bind tighter, so they need
    /// none).
    fn operand(&self) -> String {
        match self {
            Self::Binary { .. } => format!("({})", self.render()),
            _ => self.render(),
        }
    }

    /// Render a receiver of a field access / call / `.length`, wrapping any non-primary expression so
    /// the postfix access binds to the whole thing (`((Foo) x).bar()`, `(a + b).baz()`).
    fn receiver(&self) -> String {
        match self {
            Self::Binary { .. } | Self::Unary { .. } | Self::Cast { .. } => {
                format!("({})", self.render())
            }
            _ => self.render(),
        }
    }

    /// Render a comma-separated argument list.
    fn render_args(args: &[Self]) -> String {
        args.iter().map(Self::render).collect::<Vec<_>>().join(", ")
    }
}
