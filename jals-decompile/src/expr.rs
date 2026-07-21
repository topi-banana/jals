//! A small expression / statement IR for a reconstructed method body, plus its rendering to Java.
//!
//! The stack machine in [`crate::body`] folds bytecode into these trees; rendering parenthesizes
//! conservatively (any binary sub-expression used as an operand or receiver is wrapped), so the
//! emitted Java always groups the way the bytecode evaluated — never mis-associating an operator.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// A reconstructed Java expression.
#[derive(Clone)]
pub(crate) enum Expr {
    /// The receiver `this`.
    This,
    /// The non-virtual receiver `super`.
    Super,
    /// A qualified interface default receiver (`Interface.super`).
    QualifiedSuper(String),
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
    /// A string concatenation `p0 + p1 + …`, folded back from an `invokedynamic`
    /// `StringConcatFactory` call site or a `StringBuilder` append chain. Built only through
    /// [`Expr::concat`], which guarantees the leading `+` is a *string* concatenation (seeding a
    /// `""` when no `String`-typed operand anchors it), so the rendered chain always means what
    /// the bytecode computed.
    Concat(Vec<Self>),
    /// A prefix unary operation `op expr` (`-`, `~`).
    Unary { op: &'static str, expr: Box<Self> },
    /// A cast `(ty) expr`.
    Cast { ty: String, expr: Box<Self> },
    /// `expr.length` of an array.
    ArrayLength(Box<Self>),
    /// An array element access `array[index]`.
    Index { array: Box<Self>, index: Box<Self> },
    /// An array creation: `new elem`, the leading dimension(s) spelled by `form`, then
    /// `empty_dims` empty `[]` pairs (`new int[3]`, `new int[n][]`, `new int[a][b]`,
    /// `new int[]{e0, e1}`).
    NewArray {
        elem: String,
        empty_dims: usize,
        form: ArrayForm,
    },
    /// An uninitialized `new` reference on the operand stack (between `new` and its
    /// `invokespecial <init>`). Never rendered — it collapses into [`Expr::New`] once the
    /// constructor runs; if one somehow survives, it renders as a no-arg `new` as a safety net.
    Uninitialized(String),
    /// A constant-length `newarray`/`anewarray` whose element stores may still be folding into a
    /// `new T[]{…}` initializer (the `dup; <index>; <value>; Xastore` run). Never rendered —
    /// consuming it finalizes it into [`Expr::NewArray`] (complete collection → an initializer,
    /// untouched → a plain sized creation, partial → bail); if one somehow survives, it renders
    /// as the plain sized creation as a safety net.
    PendingArray {
        elem: String,
        empty_dims: usize,
        len: usize,
        elems: Vec<Self>,
    },
    /// The `dup`'d reference to the [`Expr::PendingArray`] directly beneath it on the stack (the
    /// array operand of an initializer element store). Never rendered — only the matching
    /// `Xastore` consumes it, and every other consumption bails; if one somehow survives, it
    /// renders as `null` as a safety net.
    PendingArrayDup,
    /// A fresh `new StringBuilder()` whose concat-safe `append` chain may still fold into a
    /// string concatenation (`a + "x" + …`) when a `toString()` consumes it. Never rendered —
    /// any other consumption finalizes it back into the original `new
    /// java.lang.StringBuilder().append(…)` call chain ([`Expr::builder_chain`]); if one somehow
    /// survives, it renders as that chain as a safety net.
    PendingBuilder(Vec<ConcatPart>),
}

/// One collected operand of a string concatenation: the expression plus whether its static type is
/// `java.lang.String` (a `String`-typed operand anchors the `+` chain in string context).
#[derive(Clone)]
pub(crate) struct ConcatPart {
    pub expr: Expr,
    pub stringy: bool,
}

/// How an [`Expr::NewArray`] spells its leading dimension(s) — the two forms are mutually
/// exclusive in Java source.
#[derive(Clone)]
pub(crate) enum ArrayForm {
    /// Sized dimensions `[dims[0]][dims[1]]…` (`new int[a][b]`).
    Sized(Vec<Expr>),
    /// A folded initializer: one unsized `[]` plus the brace list (`new int[]{e0, e1}`).
    Init(Vec<Expr>),
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
            Self::Return(None) => "return;".to_owned(),
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

    /// Fold collected concatenation operands into the `+` chain they came from. The chain
    /// associates left, so it is a *string* concatenation from the start iff one of the first two
    /// operands is `String`-typed; otherwise a `""` seed is prepended — recovering e.g.
    /// `a + "" + b`, whose empty constant vanishes from an `invokedynamic` recipe, and keeping a
    /// lone operand (`"" + n`) a concatenation at all.
    pub(crate) fn concat(parts: Vec<ConcatPart>) -> Self {
        let anchored = parts.len() >= 2 && parts.iter().take(2).any(|p| p.stringy);
        let mut exprs = Vec::with_capacity(parts.len() + 1);
        if !anchored {
            exprs.push(Self::lit("\"\""));
        }
        exprs.extend(parts.into_iter().map(|p| p.expr));
        if exprs.len() == 1 {
            return exprs.pop().unwrap_or_else(|| Self::lit("\"\""));
        }
        Self::Concat(exprs)
    }

    /// The value of an `int`-typed constant operand, or `None` for any other expression. The one
    /// place that ties the simulator's literal *text* back to its *value*: every `int` constant
    /// pushed on the stack (`iconst_*`, `bipush`/`sipush`, an `ldc` `Integer`) renders as bare
    /// decimal, so a plain parse is exact — and every other literal spelling (a suffixed `1L`, a
    /// quoted `"s"`, `null`) fails it.
    pub(crate) fn as_int_const(&self) -> Option<i64> {
        match self {
            Self::Literal(text) => text.parse().ok(),
            _ => None,
        }
    }

    /// Rebuild the original `new java.lang.StringBuilder().append(p0).append(p1)…` call chain a
    /// [`Expr::PendingBuilder`] collected — the finalized form when anything but a `toString()`
    /// consumes the builder, so an unfolded chain re-renders exactly as the calls it was.
    pub(crate) fn builder_chain(parts: Vec<ConcatPart>) -> Self {
        let mut chain = Self::New {
            ty: "java.lang.StringBuilder".to_owned(),
            args: Vec::new(),
        };
        for part in parts {
            chain = Self::Call {
                recv: Some(Box::new(chain)),
                name: "append".to_owned(),
                args: alloc::vec![part.expr],
            };
        }
        chain
    }

    /// Render an expression to Java source.
    pub(crate) fn render(&self) -> String {
        match self {
            Self::This => "this".into(),
            Self::Super => "super".into(),
            Self::QualifiedSuper(qualifier) => format!("{qualifier}.super"),
            Self::Local(name) | Self::Type(name) => name.clone(),
            Self::Literal(text) => text.clone(),
            Self::Field { recv, name } => format!("{}.{name}", recv.receiver()),
            Self::Call { recv, name, args } => recv.as_ref().map_or_else(
                || format!("{name}({})", Self::render_args(args)),
                |r| format!("{}.{name}({})", r.receiver(), Self::render_args(args)),
            ),
            Self::New { ty, args } => format!("new {ty}({})", Self::render_args(args)),
            Self::Binary { op, lhs, rhs } => format!("{} {op} {}", lhs.operand(), rhs.operand()),
            Self::Concat(parts) => parts
                .iter()
                .map(Self::operand)
                .collect::<Vec<_>>()
                .join(" + "),
            Self::Unary { op, expr } => {
                // A prefix operator and its operand render with no separating space, so an
                // operand that itself begins with the same operator character would merge into
                // a different Java token: `-` + `-x` renders as `--x` (pre-decrement) and `-` +
                // `-1` as `--1`. Parenthesize a nested unary or a negative literal so the token
                // boundary stays explicit. (Binary/concat operands are already wrapped by
                // `operand()`; casts begin with `(` so they cannot merge.)
                let needs_parens = match expr.as_ref() {
                    Self::Unary { .. } => true,
                    Self::Literal(text) => op.chars().next().is_some_and(|c| text.starts_with(c)),
                    _ => false,
                };
                if needs_parens {
                    format!("{op}({})", expr.render())
                } else {
                    format!("{op}{}", expr.operand())
                }
            }
            Self::Cast { ty, expr } => format!("({ty}) {}", expr.operand()),
            Self::ArrayLength(a) => format!("{}.length", a.receiver()),
            Self::Index { array, index } => format!("{}[{}]", array.receiver(), index.render()),
            Self::NewArray {
                elem,
                empty_dims,
                form,
            } => {
                let mut out = format!("new {elem}");
                match form {
                    ArrayForm::Sized(dims) => {
                        for d in dims {
                            out.push('[');
                            out.push_str(&d.render());
                            out.push(']');
                        }
                        out.push_str(&"[]".repeat(*empty_dims));
                    }
                    ArrayForm::Init(elems) => {
                        out.push_str(&"[]".repeat(*empty_dims + 1));
                        out.push('{');
                        out.push_str(&Self::render_args(elems));
                        out.push('}');
                    }
                }
                out
            }
            Self::Uninitialized(ty) => format!("new {ty}()"),
            Self::PendingArray {
                elem,
                empty_dims,
                len,
                ..
            } => format!("new {elem}[{len}]{}", "[]".repeat(*empty_dims)),
            Self::PendingArrayDup => "null".into(),
            Self::PendingBuilder(parts) => Self::builder_chain(parts.clone()).render(),
        }
    }

    /// Render an operand of a binary / unary / cast, wrapping a binary sub-expression in parentheses
    /// so the grouping the bytecode evaluated is preserved (cast / unary bind tighter, so they need
    /// none).
    fn operand(&self) -> String {
        match self {
            Self::Binary { .. } | Self::Concat(_) => format!("({})", self.render()),
            _ => self.render(),
        }
    }

    /// Render a receiver of a field access / call / `.length` / indexing, wrapping any non-primary
    /// expression so the postfix access binds to the whole thing (`((Foo) x).bar()`,
    /// `(a + b).baz()`). An array creation is wrapped too: indexing one is not grammatical bare
    /// (`new int[2][0]` parses as a two-dimensional creation), so `(new int[]{7}).length` /
    /// `(new int[2])[0]`.
    fn receiver(&self) -> String {
        match self {
            Self::Binary { .. }
            | Self::Concat(_)
            | Self::Unary { .. }
            | Self::Cast { .. }
            | Self::NewArray { .. } => {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn neg(expr: Expr) -> Expr {
        Expr::Unary {
            op: "-",
            expr: Box::new(expr),
        }
    }

    #[test]
    fn nested_unary_minus_is_parenthesized() {
        let expr = neg(neg(Expr::Local("x".into())));
        assert_eq!(expr.render(), "-(-x)");
    }

    #[test]
    fn negated_negative_literal_is_parenthesized() {
        let expr = neg(Expr::lit("-1"));
        assert_eq!(expr.render(), "-(-1)");
    }

    #[test]
    fn simple_unary_minus_stays_bare() {
        let expr = neg(Expr::Local("x".into()));
        assert_eq!(expr.render(), "-x");
    }

    #[test]
    fn negated_positive_literal_stays_bare() {
        let expr = neg(Expr::lit("1"));
        assert_eq!(expr.render(), "-1");
    }

    #[test]
    fn boolean_negation_stays_bare() {
        let expr = Expr::Unary {
            op: "!",
            expr: Box::new(Expr::Local("value".into())),
        };
        assert_eq!(expr.render(), "!value");
    }

    #[test]
    fn negated_binary_keeps_operand_parens() {
        let expr = neg(Expr::Binary {
            op: "+",
            lhs: Box::new(Expr::Local("a".into())),
            rhs: Box::new(Expr::Local("b".into())),
        });
        assert_eq!(expr.render(), "-(a + b)");
    }
}
