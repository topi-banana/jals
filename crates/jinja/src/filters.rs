//! The filters [`Environment::new`](crate::Environment::new) starts with.
//!
//! Each is an ordinary [`Filter`](crate::Filter), registered by name — there is nothing an
//! implementor can reach that a consumer's own filter cannot, which is what makes
//! [`Environment::empty`](crate::Environment::empty) a usable starting point rather than a
//! crippled one.

use alloc::string::String;
use alloc::vec::Vec;

use crate::error::{Error, ErrorKind};
use crate::value::Value;

/// How many arguments a filter takes, so the count is checked in one place.
struct Arity;

impl Arity {
    /// Refuse an argument list outside `min..=max`, naming the filter.
    fn check(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Error> {
        if args.len() < min {
            return Err(Error::new(
                ErrorKind::MissingArgument,
                alloc::format!("`{name}` takes at least {min} argument(s)"),
            ));
        }
        if args.len() > max {
            return Err(Error::new(
                ErrorKind::TooManyArguments,
                alloc::format!("`{name}` takes at most {max} argument(s)"),
            ));
        }
        Ok(())
    }

    /// One argument as text, for the filters whose arguments are all strings.
    fn text(name: &str, args: &[Value], at: usize) -> Result<String, Error> {
        let argument = args.get(at).unwrap_or(&Value::UNDEFINED);
        Self::unset(name, argument)?;
        argument.to_text().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidOperation,
                alloc::format!("`{name}` takes text arguments"),
            )
        })
    }

    /// The subject as text, for the filters that only make sense over one.
    fn subject(name: &str, value: &Value) -> Result<String, Error> {
        Self::unset(name, value)?;
        value.to_text().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidOperation,
                alloc::format!("`{name}` needs a value with a text form"),
            )
        })
    }

    /// Refuse a value that is *not set* before it is read as one with *no text form*.
    ///
    /// [`Value::to_text`] answers [`None`] for both, and they are the crate's two different
    /// mistakes with two different fixes: a shape that has no text form is a type error, while a
    /// value the author knows may be missing is what `| default(…)` is written for. Folding them
    /// together here is what would hand the type error's message to the one that has a fix.
    fn unset(name: &str, value: &Value) -> Result<(), Error> {
        if value.is_undefined() {
            return Err(Error::new(
                ErrorKind::UndefinedError,
                alloc::format!(
                    "this value is not set, so `{name}` has nothing to read; write \
                     `| default(\"…\")` before it to say what to use instead"
                ),
            ));
        }
        Ok(())
    }
}

/// A built-in as a plain function pointer, which is what makes the table below one literal.
pub(crate) type BuiltinFilter = fn(&Value, &[Value]) -> Result<Value, Error>;

/// The built-in filters, one associated function each.
pub(crate) struct Builtins;

impl Builtins {
    /// `{{ x | default("…") }}` — the argument when `x` is not set, `x` otherwise.
    ///
    /// This is the whole reason *undefined* and *unknown* are different answers: a value the author
    /// knows may be missing gets a fallback here, while a name nobody defined stays a typo.
    fn default(value: &Value, args: &[Value]) -> Result<Value, Error> {
        Arity::check("default", args, 0, 1)?;
        if value.is_undefined() {
            // With no argument there is no fallback, so the value stays *unset* rather than
            // becoming the empty string. That keeps the zero-argument spelling Jinja's under
            // `Lenient` — where an unset value writes `""` anyway — while leaving a stricter
            // `UndefinedBehavior` its say, which is the whole reason a caller chose one.
            return Ok(args.first().cloned().unwrap_or(Value::UNDEFINED));
        }
        Ok(value.clone())
    }

    /// `{{ x | upper }}`.
    fn upper(value: &Value, args: &[Value]) -> Result<Value, Error> {
        Arity::check("upper", args, 0, 0)?;
        Ok(Value::from(Arity::subject("upper", value)?.to_uppercase()))
    }

    /// `{{ x | lower }}`.
    fn lower(value: &Value, args: &[Value]) -> Result<Value, Error> {
        Arity::check("lower", args, 0, 0)?;
        Ok(Value::from(Arity::subject("lower", value)?.to_lowercase()))
    }

    /// `{{ x | trim }}` — whitespace off both ends.
    fn trim(value: &Value, args: &[Value]) -> Result<Value, Error> {
        Arity::check("trim", args, 0, 0)?;
        Ok(Value::from(Arity::subject("trim", value)?.trim()))
    }

    /// `{{ x | string }}` — the text form, so a number can be compared with one.
    fn string(value: &Value, args: &[Value]) -> Result<Value, Error> {
        Arity::check("string", args, 0, 0)?;
        Ok(Value::from(Arity::subject("string", value)?))
    }

    /// `{{ x | length }}` — characters, items, or keys.
    fn length(value: &Value, args: &[Value]) -> Result<Value, Error> {
        Arity::check("length", args, 0, 0)?;
        Self::measure(value)
    }

    /// `{{ x | count }}` — [`Self::length`] under its other name.
    ///
    /// Written out rather than registered twice, because the arity check names the filter and a
    /// shared body would tell somebody who wrote `count` to go and read about `length`.
    fn count(value: &Value, args: &[Value]) -> Result<Value, Error> {
        Arity::check("count", args, 0, 0)?;
        Self::measure(value)
    }

    /// How many items a value holds, or a message saying this shape does not answer that.
    fn measure(value: &Value) -> Result<Value, Error> {
        value.len().map(Value::from).ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidOperation,
                alloc::format!("{} has no length", value.kind().phrase()),
            )
        })
    }

    /// `{{ items | join(", ") }}`.
    fn join(value: &Value, args: &[Value]) -> Result<Value, Error> {
        Arity::check("join", args, 0, 1)?;
        let separator = if args.is_empty() {
            String::new()
        } else {
            Arity::text("join", args, 0)?
        };
        let text = Self::sequence("join", value)?
            .iter()
            .map(|item| Arity::subject("join", item))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Value::from(text.join(&separator)))
    }

    /// `{{ x | replace("a", "b") }}`.
    fn replace(value: &Value, args: &[Value]) -> Result<Value, Error> {
        Arity::check("replace", args, 2, 2)?;
        let subject = Arity::subject("replace", value)?;
        let from = Arity::text("replace", args, 0)?;
        let to = Arity::text("replace", args, 1)?;
        Ok(Value::from(subject.replace(&from, &to)))
    }

    /// `{{ items | first }}` — undefined when there is nothing to take, so `| default(…)` still
    /// answers for an empty sequence.
    fn first(value: &Value, args: &[Value]) -> Result<Value, Error> {
        Arity::check("first", args, 0, 0)?;
        Ok(Self::sequence("first", value)?
            .first()
            .cloned()
            .unwrap_or(Value::UNDEFINED))
    }

    /// `{{ items | last }}`.
    fn last(value: &Value, args: &[Value]) -> Result<Value, Error> {
        Arity::check("last", args, 0, 0)?;
        Ok(Self::sequence("last", value)?
            .last()
            .cloned()
            .unwrap_or(Value::UNDEFINED))
    }

    /// `{{ items | reverse }}`.
    fn reverse(value: &Value, args: &[Value]) -> Result<Value, Error> {
        Arity::check("reverse", args, 0, 0)?;
        let mut items = Self::sequence("reverse", value)?;
        items.reverse();
        Ok(Value::from(items))
    }

    /// The values a sequence filter walks, or a message saying this shape has none.
    fn sequence(name: &str, value: &Value) -> Result<Vec<Value>, Error> {
        value.try_iter().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidOperation,
                alloc::format!(
                    "`{name}` needs something to iterate, not {}",
                    value.kind().phrase()
                ),
            )
        })
    }

    /// Every built-in, beside the name it is registered under.
    pub(crate) fn all() -> [(&'static str, BuiltinFilter); 12] {
        [
            ("default", Self::default),
            ("upper", Self::upper),
            ("lower", Self::lower),
            ("trim", Self::trim),
            ("string", Self::string),
            ("length", Self::length),
            ("count", Self::count),
            ("join", Self::join),
            ("replace", Self::replace),
            ("first", Self::first),
            ("last", Self::last),
            ("reverse", Self::reverse),
        ]
    }
}
