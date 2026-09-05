//! Nodes and a context to text.
//!
//! The whole evaluator is one struct with a scope stack, because the only thing a render *has* is
//! what the enclosing loops bound. Everything else — filters, globals, how strict to be — is the
//! [`Environment`]'s, read rather than carried.

use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use crate::ast::{CompareOp, Expr, Node};
use crate::environment::{Environment, UndefinedBehavior};
use crate::error::{Error, ErrorKind, Position};
use crate::parser::Ast;
use crate::value::{Value, ValueKind};

/// One render in progress.
pub(crate) struct Renderer<'env> {
    env: &'env Environment,
    context: Value,
    scope: Vec<(String, Value)>,
    includes: u32,
}

impl<'env> Renderer<'env> {
    /// How deep `{% include %}` may go before the renderer refuses rather than recurses.
    ///
    /// A template that includes itself is a stack overflow, and a template registry is data a
    /// consumer may well have read from somewhere it does not control.
    const MAX_INCLUDES: u32 = 16;

    /// Render `ast` against `context`, appending to `out`.
    pub(crate) fn render(
        env: &'env Environment,
        ast: &Ast,
        context: Value,
        out: &mut String,
    ) -> Result<(), Error> {
        let mut renderer = Self {
            env,
            context,
            scope: Vec::new(),
            includes: 0,
        };
        renderer.nodes(&ast.nodes, out)
    }

    fn nodes(&mut self, nodes: &[Node], out: &mut String) -> Result<(), Error> {
        for node in nodes {
            match node {
                Node::Text(text) => out.push_str(text),
                Node::Emit { expr, at } => {
                    let value = self.eval(expr, *at)?;
                    self.write(&value, *at, out)?;
                }
                Node::If { arms, otherwise } => {
                    let mut taken = false;
                    for arm in arms {
                        if self.eval(&arm.condition, arm.at)?.is_true() {
                            self.nodes(&arm.body, out)?;
                            taken = true;
                            break;
                        }
                    }
                    if !taken {
                        self.nodes(otherwise, out)?;
                    }
                }
                Node::For {
                    binding,
                    source,
                    at,
                    body,
                } => self.walk(binding, source, *at, body, out)?,
                Node::Include { name, at } => self.include(name, *at, out)?,
            }
        }
        Ok(())
    }

    fn walk(
        &mut self,
        binding: &str,
        source: &Expr,
        at: Position,
        body: &[Node],
        out: &mut String,
    ) -> Result<(), Error> {
        let value = self.eval(source, at)?;
        let items = value.try_iter().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidOperation,
                format!("{} cannot be iterated", value.kind().phrase()),
            )
            .at(at)
        })?;
        let length = items.len();
        for (index, item) in items.into_iter().enumerate() {
            self.scope.push((String::from(binding), item));
            self.scope
                .push((String::from("loop"), Self::loop_value(index, length)));
            let rendered = self.nodes(body, out);
            self.scope.truncate(self.scope.len().saturating_sub(2));
            rendered?;
        }
        Ok(())
    }

    /// The `loop` namespace a body reads. Every field is a fact about position, so a template can
    /// write a separator without counting anything itself.
    fn loop_value(index: usize, length: usize) -> Value {
        let remaining = length.saturating_sub(index);
        Value::from_entries([
            ("index", Value::from(index + 1)),
            ("index0", Value::from(index)),
            ("revindex", Value::from(remaining)),
            ("revindex0", Value::from(remaining.saturating_sub(1))),
            ("first", Value::from(index == 0)),
            ("last", Value::from(index + 1 == length)),
            ("length", Value::from(length)),
        ])
    }

    fn include(&mut self, name: &Expr, at: Position, out: &mut String) -> Result<(), Error> {
        let value = self.eval(name, at)?;
        let name = value.as_str().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidOperation,
                "`include` names a template with a string",
            )
            .at(at)
        })?;
        let ast = self.env.ast(name).ok_or_else(|| {
            Error::new(
                ErrorKind::TemplateNotFound,
                format!("no template named `{name}`"),
            )
            .at(at)
        })?;
        if self.includes >= Self::MAX_INCLUDES {
            return Err(Error::new(
                ErrorKind::InvalidOperation,
                "templates are included too deeply",
            )
            .at(at));
        }
        self.includes += 1;
        let included = String::from(name);
        let rendered = self
            .nodes(&ast.nodes, out)
            .map_err(|error| error.in_template(&included));
        self.includes -= 1;
        rendered
    }

    /// Append a value's text form, or say why it has none.
    fn write(&self, value: &Value, at: Position, out: &mut String) -> Result<(), Error> {
        match value.write(out) {
            Ok(true) => Ok(()),
            Ok(false) => match self.env.undefined_behavior() {
                UndefinedBehavior::Lenient | UndefinedBehavior::Chainable => Ok(()),
                UndefinedBehavior::SemiStrict | UndefinedBehavior::Strict => Err(Error::new(
                    ErrorKind::UndefinedError,
                    "this value is not set; write `| default(\"…\")` to say what to use instead",
                )
                .at(at)),
            },
            Err(kind) => Err(Error::new(
                ErrorKind::InvalidOperation,
                format!(
                    "{} has no text form, so it cannot be written",
                    kind.phrase()
                ),
            )
            .at(at)),
        }
    }

    fn eval(&self, expr: &Expr, at: Position) -> Result<Value, Error> {
        match expr {
            Expr::Const(value) => Ok(value.clone()),
            Expr::Var { name } => self.variable(name, at),
            Expr::Get { base, key, path } => self.get(base, key, path.as_deref(), at),
            Expr::Not(inner) => Ok(Value::from(!self.eval(inner, at)?.is_true())),
            Expr::And(left, right) => Ok(Value::from(
                self.eval(left, at)?.is_true() && self.eval(right, at)?.is_true(),
            )),
            Expr::Or(left, right) => Ok(Value::from(
                self.eval(left, at)?.is_true() || self.eval(right, at)?.is_true(),
            )),
            Expr::Compare { op, left, right } => self.compare(*op, left, right, at),
            Expr::Filter { name, value, args } => self.apply(name, value, args, at),
        }
    }

    /// Resolve a bare name: the enclosing loops first, then the context, then the globals.
    fn variable(&self, name: &str, at: Position) -> Result<Value, Error> {
        if let Some((_, value)) = self.scope.iter().rev().find(|(bound, _)| bound == name) {
            return Ok(value.clone());
        }
        if let Some(value) = self.context.get_attr(name) {
            return Ok(value);
        }
        if let Some(value) = self.env.global(name) {
            return Ok(value.clone());
        }
        if !self.env.strict_variables() {
            return Ok(Value::UNDEFINED);
        }
        Err(Error::new(ErrorKind::UnknownVariable, self.unknown(name)).at(at))
    }

    /// The sentence an unknown name gets, with what a template *can* read spelled out: the fix for
    /// a typo is a name, and listing them is the only way an error can hand one over.
    fn unknown(&self, name: &str) -> String {
        let mut names: Vec<&str> = self.context.keys().unwrap_or_default();
        names.extend(self.env.global_names());
        names.sort_unstable();
        names.dedup();
        let mut message = format!("unknown name `{name}`");
        Self::list_names(&names, &mut message, "; a template can read ");
        message
    }

    /// Append `lead` and then every name, quoted, as `` `a`, `b` and `c` `` — or nothing at all
    /// when there are no names, so a caller never has to check first.
    fn list_names(names: &[&str], message: &mut String, lead: &str) {
        let Some((last, rest)) = names.split_last() else {
            return;
        };
        message.push_str(lead);
        let quoted: Vec<String> = rest.iter().map(|name| format!("`{name}`")).collect();
        message.push_str(&quoted.join(", "));
        if !rest.is_empty() {
            message.push_str(" and ");
        }
        let _ = write!(message, "`{last}`");
    }

    /// `base.key` and `base[key]`, which are the same access.
    fn get(
        &self,
        base: &Expr,
        key: &Expr,
        path: Option<&str>,
        at: Position,
    ) -> Result<Value, Error> {
        let subject = self.eval(base, at)?;
        let key = self.eval(key, at)?;
        // A `.field` and a `["field"]` both arrive as a string already, which is every key a
        // template writes but the numeric index; only the rest are rendered into one.
        let key = key
            .as_str()
            .map(Cow::Borrowed)
            .or_else(|| key.to_text().map(Cow::Owned))
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidOperation,
                    format!("{} cannot name a field", key.kind().phrase()),
                )
                .at(at)
            })?;
        if subject.is_undefined() {
            return match self.env.undefined_behavior() {
                UndefinedBehavior::Chainable => Ok(Value::UNDEFINED),
                _ => Err(Error::new(
                    ErrorKind::UndefinedError,
                    format!("`{key}` cannot be read from a value that is not set"),
                )
                .at(at)),
            };
        }
        if let Some(value) = subject.get_attr(&key) {
            return Ok(value);
        }
        if !matches!(subject.kind(), ValueKind::Seq | ValueKind::Map) {
            return Err(Error::new(
                ErrorKind::InvalidOperation,
                format!("this value has no fields, so `{key}` cannot be read"),
            )
            .at(at));
        }
        if !self.env.strict_variables() {
            return Ok(Value::UNDEFINED);
        }
        // A field typo is the commoner one, so it gets the same help a root typo gets: the names
        // that *are* there. `Value::keys` answers for a map and for nothing else, which is exactly
        // the case this arm is left holding.
        let mut message = path.map_or_else(
            || format!("this value has no field `{key}`"),
            |path| format!("`{path}` has no field `{key}`"),
        );
        if let Some(known) = subject.keys() {
            Self::list_names(&known, &mut message, "; it has ");
        }
        Err(Error::new(ErrorKind::UnknownVariable, message).at(at))
    }

    fn compare(
        &self,
        op: CompareOp,
        left: &Expr,
        right: &Expr,
        at: Position,
    ) -> Result<Value, Error> {
        let left = self.eval(left, at)?;
        let right = self.eval(right, at)?;
        let answer = match op {
            CompareOp::Eq => left == right,
            CompareOp::Ne => left != right,
            _ => {
                let ordering = left.partial_cmp(&right).ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidOperation,
                        format!(
                            "{} and {} cannot be compared with `{}`",
                            left.kind().phrase(),
                            right.kind().phrase(),
                            op.spelling()
                        ),
                    )
                    .at(at)
                })?;
                match op {
                    CompareOp::Lt => ordering.is_lt(),
                    CompareOp::Le => ordering.is_le(),
                    CompareOp::Gt => ordering.is_gt(),
                    CompareOp::Ge => ordering.is_ge(),
                    CompareOp::Eq | CompareOp::Ne => unreachable!("handled above"),
                }
            }
        };
        Ok(Value::from(answer))
    }

    fn apply(&self, name: &str, value: &Expr, args: &[Expr], at: Position) -> Result<Value, Error> {
        let subject = self.eval(value, at)?;
        // Whether the filter exists is settled *before* what its subject holds, or a misspelled
        // name under `Strict` is reported as an unset value and the message asserts a filter
        // nobody registered — sending the author after a `| default(…)` they already wrote.
        let filter = self.env.filter(name).ok_or_else(|| {
            Error::new(ErrorKind::UnknownFilter, format!("unknown filter `{name}`")).at(at)
        })?;
        if self.env.undefined_behavior() == UndefinedBehavior::Strict && subject.is_undefined() {
            return Err(Error::new(
                ErrorKind::UndefinedError,
                format!("this value is not set, so `{name}` has nothing to apply to"),
            )
            .at(at));
        }
        let args = args
            .iter()
            .map(|arg| self.eval(arg, at))
            .collect::<Result<Vec<_>, _>>()?;
        filter.apply(&subject, &args).map_err(|error| error.at(at))
    }
}
