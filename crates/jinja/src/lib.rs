//! A small Jinja2 template engine, shaped like [minijinja] and dependent on nothing.
//!
//! ```
//! use jinja::{Environment, Value, context};
//!
//! let mut env = Environment::new();
//! env.add_template("greeting", "Hello, {{ name }}!").expect("the source parses");
//!
//! let rendered = env
//!     .get_template("greeting")
//!     .expect("it was just added")
//!     .render(context! { name => "world" })
//!     .expect("it renders");
//! assert_eq!(rendered, "Hello, world!");
//! # let _: Value = Value::UNDEFINED;
//! ```
//!
//! # What is here
//!
//! `{{ … }}` writes a value, `{% … %}` is a directive, `{# … #}` is a comment. The directives are
//! `if` / `elif` / `else` / `endif`, `for` / `endfor`, and `include`. An expression has names,
//! `.field` and `["field"]` access, string, integer, float, `true`, `false` and `none` literals,
//! `not` / `and` / `or`, the six comparisons, parentheses, and `|` filters with arguments.
//!
//! A `{% for %}` binds `loop`, with `index`, `index0`, `revindex`, `revindex0`, `first`, `last`
//! and `length`.
//!
//! # What is deliberately not
//!
//! `{% extends %}`, `{% block %}`, `{% macro %}`, `{% set %}`, `is` tests, auto-escaping, and
//! arithmetic. Each of them is a second language inside the template, and this one is meant for
//! documents whose author is also the author of the program rendering them: a config file, a
//! manifest, a generated header. Reach for [minijinja] where a template is written by somebody
//! else.
//!
//! There is also no `serde` support, which is minijinja's centrepiece. A [`Value`] is built from
//! the `From` impls, from [`context!`], or from an [`Object`] a consumer implements — which is what
//! lets this crate have no dependencies at all and stay `no_std + alloc` in every configuration.
//!
//! # Where it differs on purpose
//!
//! - **A lookup that finds nothing and a value that is not set are different answers.**
//!   [`Value::get_attr`] returns [`None`] for the first and <code>Some([Value::UNDEFINED])</code> for the
//!   second, so [`Environment::set_strict_variables`] can refuse a *typo* while `| default(…)`
//!   still answers for a value the author knows may be missing. minijinja folds the two together.
//! - **[`Object`] is not `Send + Sync`,** and its keys are `&str`. Nothing here crosses a thread.
//! - **A collection has no text form.** `{{ some_map }}` is an error rather than a debug rendering,
//!   because a template that fills a config file is far more often one field short than it is
//!   asking for a dump.
//! - **Whitespace control is one setting, not two.** [`Environment::set_trim_block_lines`] is
//!   Jinja's `trim_blocks` and `lstrip_blocks` folded together, and there is no `{%- -%}` spelling
//!   beside it.
//! - **A filter is handed no render state** — only its subject and its arguments.
//!
//! # Making a value out of your own type
//!
//! [`Object`] is the seam. A set that answers membership for *any* name, rather than only the names
//! it holds, is a rule about the consumer's domain and stays in the consumer's crate:
//!
//! ```
//! use alloc::collections::BTreeSet;
//! # extern crate alloc;
//! use jinja::{Enumerator, Environment, Object, Value, context};
//!
//! #[derive(Debug)]
//! struct Features(BTreeSet<String>);
//!
//! impl Object for Features {
//!     fn get_value(&self, key: &str) -> Option<Value> {
//!         // Every name is a well-formed question, so this never answers `None`.
//!         Some(Value::from(self.0.contains(key)))
//!     }
//!
//!     fn enumerate(&self) -> Enumerator {
//!         Enumerator::Values(self.0.iter().map(|name| Value::from(name.as_str())).collect())
//!     }
//! }
//!
//! let features = Features(["server".to_owned()].into_iter().collect());
//! let env = Environment::new();
//! let rendered = env
//!     .render_str(
//!         "{% if features.server %}on{% else %}off{% endif %} {{ features.client }}",
//!         context! { features => Value::from_object(features) },
//!     )
//!     .expect("it renders");
//! assert_eq!(rendered, "on false");
//! ```
//!
//! [minijinja]: https://docs.rs/minijinja

#![no_std]

extern crate alloc;

mod ast;
mod environment;
mod error;
mod filters;
mod parser;
mod render;
mod template;
mod value;

pub use crate::environment::{Environment, Filter, UndefinedBehavior};
pub use crate::error::{Error, ErrorKind};
pub use crate::template::Template;
pub use crate::value::{Enumerator, Object, Value, ValueKind};

/// Re-exports [`context!`] expands to. Not part of the API; nothing here is stable.
#[doc(hidden)]
pub mod __private {
    pub use alloc::vec::Vec;
}

/// Build the map a template is rendered against.
///
/// `key => value` names a value; a bare `key` is shorthand for `key => key`, exactly as minijinja
/// spells it. Every value goes through [`Value`]'s `From` impls, so a [`Value`] passes through
/// unchanged.
///
/// ```
/// use jinja::{Environment, context};
///
/// let version = "1.2.3";
/// let context = context! { name => "hellomod", version };
///
/// let rendered = Environment::new()
///     .render_str("{{ name }}-{{ version }}", context)
///     .expect("it renders");
/// assert_eq!(rendered, "hellomod-1.2.3");
/// ```
#[macro_export]
macro_rules! context {
    ($($key:ident $(=> $value:expr)?),* $(,)?) => {{
        let entries: $crate::__private::Vec<(&'static str, $crate::Value)> =
            $crate::__private::Vec::from([$( $crate::context!(@entry $key $(=> $value)?) ),*]);
        $crate::Value::from_entries(entries)
    }};
    (@entry $key:ident => $value:expr) => {
        (stringify!($key), $crate::Value::from($value))
    };
    (@entry $key:ident) => {
        $crate::context!(@entry $key => $key)
    };
}
