//! The registry a template is rendered against: its filters, its globals, and the handful of
//! decisions that change what a template *means* rather than what it says.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::error::{Error, ErrorKind};
use crate::filters::Builtins;
use crate::parser::Ast;
use crate::template::Template;
use crate::value::Value;

/// What happens to a value that is known and not set.
///
/// The four are minijinja's. Three of them are a ladder — [`Self::Lenient`], [`Self::SemiStrict`],
/// [`Self::Strict`], each refusing everything the one before it refuses and one thing more.
/// [`Self::Chainable`] is a **step aside** rather than a rung: it is [`Self::Lenient`] with reading
/// a field *from* an unset value allowed, which is the one thing [`Self::Lenient`] refuses. Do not
/// read the declaration order as strictness order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UndefinedBehavior {
    /// Writing an undefined value produces the empty string; reading a field *from* one is an
    /// error. Jinja's own default, and this crate's.
    #[default]
    Lenient,
    /// As [`Self::Lenient`], except that reading a field from an undefined value is undefined
    /// again — so `{{ a.b.c }}` answers rather than failing halfway down. This is the one rung that
    /// *loosens* rather than tightens; see the note on the enum itself.
    Chainable,
    /// Writing an undefined value is an error; `| default(…)` still answers for one.
    ///
    /// This is the rung a tool that writes files wants: an unset value reaching the output is the
    /// silent wrong answer, and the author who meant it says so with a `default`.
    SemiStrict,
    /// As [`Self::SemiStrict`], and passing an undefined value to *any* filter is an error too —
    /// `default` included, which is what makes this rung different from the one before it.
    Strict,
}

/// Something a `|` can apply.
///
/// Blanket-implemented for every `Fn(&Value, &[Value]) -> Result<Value, Error>`, so a plain
/// function or a closure is a filter with no wrapper. Unlike minijinja's, a filter is handed no
/// render state and takes its subject by reference: everything it may read is that subject and its
/// arguments, which is what keeps a filter testable on its own.
pub trait Filter {
    /// Apply this filter to `value`, with the arguments the template wrote.
    fn apply(&self, value: &Value, args: &[Value]) -> Result<Value, Error>;
}

impl<F: Fn(&Value, &[Value]) -> Result<Value, Error>> Filter for F {
    fn apply(&self, value: &Value, args: &[Value]) -> Result<Value, Error> {
        self(value, args)
    }
}

/// Templates, filters, globals, and the settings a render reads.
///
/// Parsing settings — [`Self::set_trim_block_lines`] — are read when a template is *added*, so set
/// them before adding one. Everything else is read at render time.
pub struct Environment {
    templates: BTreeMap<String, Rc<Ast>>,
    globals: BTreeMap<String, Value>,
    filters: BTreeMap<String, Rc<dyn Filter>>,
    undefined_behavior: UndefinedBehavior,
    strict_variables: bool,
    trim_block_lines: bool,
}

impl Environment {
    /// An environment with the built-in filters registered.
    pub fn new() -> Self {
        let mut environment = Self::empty();
        for (name, filter) in Builtins::all() {
            environment.add_filter(name, filter);
        }
        environment
    }

    /// An environment with nothing registered at all, for a consumer that wants to say exactly
    /// what a template may call.
    pub fn empty() -> Self {
        Self {
            templates: BTreeMap::new(),
            globals: BTreeMap::new(),
            filters: BTreeMap::new(),
            undefined_behavior: UndefinedBehavior::Lenient,
            strict_variables: false,
            trim_block_lines: false,
        }
    }

    /// Parse `source` and keep it under `name`, which is what `{% include %}` and
    /// [`Self::get_template`] look it up by, and what an error from it is labelled with.
    pub fn add_template(&mut self, name: impl Into<String>, source: &str) -> Result<(), Error> {
        let name = name.into();
        let ast =
            Ast::parse(source, self.trim_block_lines).map_err(|error| error.in_template(&name))?;
        self.templates.insert(name, Rc::new(ast));
        Ok(())
    }

    /// Forget one template. Answers whether there was one.
    pub fn remove_template(&mut self, name: &str) -> bool {
        self.templates.remove(name).is_some()
    }

    /// Forget every template.
    pub fn clear_templates(&mut self) {
        self.templates.clear();
    }

    /// The names of the templates this environment holds, in name order.
    pub fn template_names(&self) -> impl Iterator<Item = &str> {
        self.templates.keys().map(String::as_str)
    }

    /// The template added under this name.
    pub fn get_template(&self, name: &str) -> Result<Template<'_>, Error> {
        let ast = self.templates.get(name).ok_or_else(|| {
            Error::new(
                ErrorKind::TemplateNotFound,
                format!("no template named `{name}`"),
            )
        })?;
        Ok(Template::new(self, Rc::from(name), Rc::clone(ast)))
    }

    /// Parse a template without keeping it. It has no name, so its errors carry none either.
    pub fn template_from_str(&self, source: &str) -> Result<Template<'_>, Error> {
        let ast = Ast::parse(source, self.trim_block_lines)?;
        Ok(Template::new(self, Rc::from(""), Rc::new(ast)))
    }

    /// Parse and render in one step, for the caller that renders a source exactly once.
    pub fn render_str(&self, source: &str, context: Value) -> Result<String, Error> {
        self.template_from_str(source)?.render(context)
    }

    /// Register a filter under `name`, replacing any filter already there.
    pub fn add_filter(&mut self, name: impl Into<String>, filter: impl Filter + 'static) {
        self.filters.insert(name.into(), Rc::new(filter));
    }

    /// Unregister a filter. Answers whether there was one.
    pub fn remove_filter(&mut self, name: &str) -> bool {
        self.filters.remove(name).is_some()
    }

    /// The names of the registered filters, in name order.
    pub fn filter_names(&self) -> impl Iterator<Item = &str> {
        self.filters.keys().map(String::as_str)
    }

    /// A value every template can read, whatever context it is rendered with.
    pub fn add_global(&mut self, name: impl Into<String>, value: Value) {
        self.globals.insert(name.into(), value);
    }

    /// Unregister a global. Answers whether there was one.
    pub fn remove_global(&mut self, name: &str) -> bool {
        self.globals.remove(name).is_some()
    }

    /// What happens to a value that is known and not set.
    pub const fn set_undefined_behavior(&mut self, behavior: UndefinedBehavior) {
        self.undefined_behavior = behavior;
    }

    /// What happens to a value that is known and not set.
    pub const fn undefined_behavior(&self) -> UndefinedBehavior {
        self.undefined_behavior
    }

    /// Whether a name **nothing defines** is an error rather than undefined.
    ///
    /// Off by default, which is Jinja's answer. Twig calls the same switch `strict_variables`, and
    /// a tool whose templates are read by their author rather than written by a stranger usually
    /// wants it on: at that point a name nobody defined is a typo, and reporting it as one is the
    /// difference between a build that fails and a config file that ships wrong.
    ///
    /// It is deliberately not part of [`UndefinedBehavior`]: *unknown* and *undefined* are separate
    /// questions, and the whole point of `| default(…)` is that the second has an answer.
    pub const fn set_strict_variables(&mut self, strict: bool) {
        self.strict_variables = strict;
    }

    /// Whether a name nothing defines is an error.
    pub const fn strict_variables(&self) -> bool {
        self.strict_variables
    }

    /// Whether a block or comment tag alone on its line takes the whole line with it.
    ///
    /// Off by default, so a template renders as Jinja's defaults would. This is Jinja's
    /// `trim_blocks` and `lstrip_blocks` folded into **one** rule rather than two: a tag sharing
    /// its line with anything at all keeps every byte around it, and there is no `{%- -%}`
    /// spelling to remember beside it. Turn it on for templates that render JSON, XML, or anything
    /// else where a blank line left behind by a `{% if %}` is a diff.
    ///
    /// Read when a template is added, so set it before adding one.
    pub const fn set_trim_block_lines(&mut self, trim: bool) {
        self.trim_block_lines = trim;
    }

    /// Whether a block or comment tag alone on its line takes the whole line with it.
    pub const fn trim_block_lines(&self) -> bool {
        self.trim_block_lines
    }

    /// The filter registered under this name.
    pub(crate) fn filter(&self, name: &str) -> Option<&Rc<dyn Filter>> {
        self.filters.get(name)
    }

    /// The global registered under this name.
    pub(crate) fn global(&self, name: &str) -> Option<&Value> {
        self.globals.get(name)
    }

    /// The parsed template under this name, for `{% include %}`.
    pub(crate) fn ast(&self, name: &str) -> Option<Rc<Ast>> {
        self.templates.get(name).map(Rc::clone)
    }

    /// The global names, which an unknown-name error lists beside the context's own.
    pub(crate) fn global_names(&self) -> Vec<&str> {
        self.globals.keys().map(String::as_str).collect()
    }
}

impl Default for Environment {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Environment {
    /// The filters are boxed closures with no `Debug` of their own, so what is shown is the shape:
    /// which names are registered, and how the settings stand.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Environment")
            .field("templates", &self.templates.keys().collect::<Vec<_>>())
            .field("globals", &self.globals)
            .field("filters", &self.filters.keys().collect::<Vec<_>>())
            .field("undefined_behavior", &self.undefined_behavior)
            .field("strict_variables", &self.strict_variables)
            .field("trim_block_lines", &self.trim_block_lines)
            .finish()
    }
}
