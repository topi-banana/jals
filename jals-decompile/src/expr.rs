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

/// Render a statement tree to indented Java lines. Top-level statements are at indent 0 (the caller
/// adds the method-body indentation); a nested block indents its contents four spaces further.
pub(crate) fn render_block(stmts: &[Stmt]) -> Vec<String> {
    let mut out = Vec::new();
    for stmt in stmts {
        render_into(stmt, 0, &mut out);
    }
    out
}

fn render_into(stmt: &Stmt, indent: usize, out: &mut Vec<String>) {
    let pad = " ".repeat(indent);
    match stmt {
        Stmt::If { cond, then, els } => {
            out.push(format!("{pad}if ({}) {{", render_expr(cond)));
            for s in then {
                render_into(s, indent + 4, out);
            }
            if !els.is_empty() {
                out.push(format!("{pad}}} else {{"));
                for s in els {
                    render_into(s, indent + 4, out);
                }
            }
            out.push(format!("{pad}}}"));
        }
        Stmt::While { cond, body } => {
            out.push(format!("{pad}while ({}) {{", render_expr(cond)));
            for s in body {
                render_into(s, indent + 4, out);
            }
            out.push(format!("{pad}}}"));
        }
        Stmt::DoWhile { body, cond } => {
            out.push(format!("{pad}do {{"));
            for s in body {
                render_into(s, indent + 4, out);
            }
            out.push(format!("{pad}}} while ({});", render_expr(cond)));
        }
        simple => out.push(format!("{pad}{}", render_simple(simple))),
    }
}

/// Render a non-`If` statement to a single line of Java (terminated with `;`).
fn render_simple(stmt: &Stmt) -> String {
    match stmt {
        Stmt::Expr(e) => format!("{};", render_expr(e)),
        Stmt::Declare { ty, name } => format!("{ty} {name};"),
        Stmt::Return(None) => "return;".to_string(),
        Stmt::Return(Some(e)) => format!("return {};", render_expr(e)),
        Stmt::Assign { target, value } => {
            format!("{} = {};", render_expr(target), render_expr(value))
        }
        Stmt::Throw(e) => format!("throw {};", render_expr(e)),
        Stmt::SuperCall(args) => format!("super({});", render_args(args)),
        Stmt::ThisCall(args) => format!("this({});", render_args(args)),
        Stmt::If { .. } | Stmt::While { .. } | Stmt::DoWhile { .. } => {
            unreachable!("block statements are rendered by render_into")
        }
    }
}

/// Render an expression to Java source.
pub(crate) fn render_expr(e: &Expr) -> String {
    match e {
        Expr::This => "this".into(),
        Expr::Local(name) | Expr::Type(name) => name.clone(),
        Expr::Literal(text) => text.clone(),
        Expr::Field { recv, name } => format!("{}.{name}", receiver(recv)),
        Expr::Call { recv, name, args } => recv.as_ref().map_or_else(
            || format!("{name}({})", render_args(args)),
            |r| format!("{}.{name}({})", receiver(r), render_args(args)),
        ),
        Expr::New { ty, args } => format!("new {ty}({})", render_args(args)),
        Expr::Binary { op, lhs, rhs } => format!("{} {op} {}", operand(lhs), operand(rhs)),
        Expr::Unary { op, expr } => format!("{op}{}", operand(expr)),
        Expr::Cast { ty, expr } => format!("({ty}) {}", operand(expr)),
        Expr::ArrayLength(a) => format!("{}.length", receiver(a)),
        Expr::Uninitialized(ty) => format!("new {ty}()"),
    }
}

/// Render an operand of a binary / unary / cast, wrapping a binary sub-expression in parentheses so
/// the grouping the bytecode evaluated is preserved (cast / unary bind tighter, so they need none).
fn operand(e: &Expr) -> String {
    match e {
        Expr::Binary { .. } => format!("({})", render_expr(e)),
        _ => render_expr(e),
    }
}

/// Render a receiver of a field access / call / `.length`, wrapping any non-primary expression so the
/// postfix access binds to the whole thing (`((Foo) x).bar()`, `(a + b).baz()`).
fn receiver(e: &Expr) -> String {
    match e {
        Expr::Binary { .. } | Expr::Unary { .. } | Expr::Cast { .. } => {
            format!("({})", render_expr(e))
        }
        _ => render_expr(e),
    }
}

/// Render a comma-separated argument list.
fn render_args(args: &[Expr]) -> String {
    args.iter().map(render_expr).collect::<Vec<_>>().join(", ")
}
