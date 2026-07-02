//! Lexical scopes: the nested regions that bound a name's visibility.

use alloc::vec::Vec;
use core::ops::Range;

use crate::def::DefId;

/// A stable, dense identifier for a [`Scope`] within one [`Resolved`](crate::Resolved) file.
///
/// It indexes [`Resolved::scopes`](crate::Resolved::scopes). The file scope is always [`ScopeId`]`(0)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopeId(pub(crate) u32);

/// What sort of region a scope covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    /// The whole compilation unit: top-level type declarations.
    File,
    /// A type body (class / interface / enum / record / annotation type / anonymous class): its
    /// members and type parameters. Members are *hoisted* — visible before their declaration.
    Type,
    /// A method or constructor: its type parameters and parameters.
    Method,
    /// A block `{ ... }`: its local variables, visible only after their declaration.
    Block,
    /// A `for` / for-each header and body.
    For,
    /// A `catch` clause: its exception variable.
    Catch,
    /// A try-with-resources head: its resource variables.
    Resources,
    /// A `switch` rule or group: its pattern variables.
    Switch,
    /// A lambda: its parameters.
    Lambda,
}

impl ScopeKind {
    /// Whether a binding in this scope is visible only *after* its declaration point.
    ///
    /// Local variables and resources are sequential (`use(x); int x;` does not see `x`). Members,
    /// parameters, type parameters, and pattern bindings are visible throughout their scope, so
    /// forward references resolve.
    pub fn is_sequential(self) -> bool {
        matches!(
            self,
            ScopeKind::Block | ScopeKind::For | ScopeKind::Resources
        )
    }
}

/// A lexical scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    /// This scope's identifier.
    pub id: ScopeId,
    /// What sort of region it covers.
    pub kind: ScopeKind,
    /// The enclosing scope, or `None` for the file scope.
    pub parent: Option<ScopeId>,
    /// The byte range of the syntax node this scope covers.
    pub range: Range<usize>,
    /// The definitions declared directly in this scope, in source order.
    pub(crate) defs: Vec<DefId>,
}
