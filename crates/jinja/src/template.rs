//! A parsed template, bound to the environment it will be rendered against.

use alloc::rc::Rc;
use alloc::string::String;

use crate::environment::Environment;
use crate::error::Error;
use crate::parser::Ast;
use crate::render::Renderer;
use crate::value::Value;

/// A parsed template.
///
/// It borrows the [`Environment`] it came from — the filters and templates a render reaches are the
/// ones registered *now*, not the ones registered when the source was parsed. The parsed body
/// itself is behind an [`Rc`], so handing a template around copies a pointer.
#[derive(Debug, Clone)]
pub struct Template<'env> {
    env: &'env Environment,
    name: Rc<str>,
    ast: Rc<Ast>,
}

impl<'env> Template<'env> {
    pub(crate) const fn new(env: &'env Environment, name: Rc<str>, ast: Rc<Ast>) -> Self {
        Self { env, name, ast }
    }

    /// The name it was added under, or the empty string for one parsed and not kept.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The environment it renders against.
    pub const fn environment(&self) -> &'env Environment {
        self.env
    }

    /// Render against one context, which is normally a map — anything a
    /// [`context!`](crate::context) builds, or a [`Value`] a consumer assembled itself.
    pub fn render(&self, context: Value) -> Result<String, Error> {
        let mut out = String::new();
        self.render_to(context, &mut out)?;
        Ok(out)
    }

    /// Render against one context, appending to `out` rather than allocating a new [`String`].
    ///
    /// `out` may hold a partial render when this fails; a caller that must not show one renders
    /// into a buffer of its own.
    pub fn render_to(&self, context: Value, out: &mut String) -> Result<(), Error> {
        Renderer::render(self.env, &self.ast, context, out)
            .map_err(|error| error.in_template(&self.name))
    }
}
