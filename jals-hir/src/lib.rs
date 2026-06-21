//! File-local name resolution for Java/JALS source, over the `jals-syntax` CST.
//!
//! [`resolve`] binds each *reference* (an identifier use) to the *definition* (binding) it names,
//! within a single source file. This is the foundation for go-to-definition, find-references,
//! unused-binding detection, and — later — type inference.
//!
//! Scope of Phase 1:
//! - **Resolved:** locals, parameters (method / constructor / lambda), fields (including forward
//!   references), methods (bare-callee calls), type parameters, enum constants, and catch /
//!   resource / for-each / pattern variables.
//! - **Out of scope (left [`Unresolved`]):** member-access right-hand names (`obj.field` — needs a
//!   type), type-name references, and any name with no file-local definition (imported or external
//!   types, inherited members). `this` / `super` are not recorded as references at all.
//!
//! It never panics: an incomplete or erroneous tree yields a best-effort result, and an
//! unresolvable reference is recorded as [`Resolution::Unresolved`] rather than failing.
//!
//! # Example
//!
//! ```
//! let resolved = jals_hir::resolve("class C { int x; int get() { return x; } }");
//! // The `x` in `return x;` resolves back to the field `x`.
//! let r = resolved.references.iter().find(|r| r.name == "x").unwrap();
//! let jals_hir::Resolution::Def(id) = r.resolution else { panic!("x should resolve") };
//! assert_eq!(resolved.def(id).name, "x");
//! ```

mod def;
mod reference;
mod resolve;
mod scope;

use jals_syntax::SyntaxNode;

pub use def::{Def, DefId, DefKind, Namespace};
pub use reference::{Reference, Resolution};
pub use resolve::Resolved;
pub use scope::{Scope, ScopeId, ScopeKind};

/// Parses `src` and resolves names within it.
pub fn resolve(src: &str) -> Resolved {
    resolve_node(&jals_syntax::parse(src).syntax())
}

/// Resolves names over an already-parsed CST `root` (the `SOURCE_FILE` node).
///
/// This is the half a caller holding a cached parse tree (the language server, which keeps an
/// `Arc<Parse>` per document; a lint rule, which is handed the root) calls without reparsing —
/// mirroring `jals_lint::lint_node`.
pub fn resolve_node(root: &SyntaxNode) -> Resolved {
    resolve::Resolver::new(root).run()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `Resolution` of the first reference named `name`.
    fn resolution_of(resolved: &Resolved, name: &str) -> Resolution {
        resolved
            .references
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("no reference named `{name}`"))
            .resolution
    }

    #[test]
    fn local_resolves_to_its_declaration() {
        let resolved = resolve("class C { void m() { int x = 1; use(x); } }");
        let Resolution::Def(id) = resolution_of(&resolved, "x") else {
            panic!("x should resolve");
        };
        assert_eq!(resolved.def(id).kind, DefKind::Local);
    }

    #[test]
    fn use_before_declaration_is_unresolved() {
        let resolved = resolve("class C { void m() { use(x); int x = 1; } }");
        assert_eq!(resolution_of(&resolved, "x"), Resolution::Unresolved);
    }

    #[test]
    fn field_is_visible_before_its_declaration() {
        // A method body may reference a field declared later in the class (members are hoisted).
        let resolved = resolve("class C { int get() { return x; } int x; }");
        let Resolution::Def(id) = resolution_of(&resolved, "x") else {
            panic!("forward field reference should resolve");
        };
        assert_eq!(resolved.def(id).kind, DefKind::Field);
    }

    #[test]
    fn unknown_name_is_unresolved() {
        let resolved = resolve("class C { void m() { use(nope); } }");
        assert_eq!(resolution_of(&resolved, "nope"), Resolution::Unresolved);
    }

    #[test]
    fn resolve_node_matches_resolve() {
        let src = "class C { void m() { int x = 1; use(x); } }";
        let parse = jals_syntax::parse(src);
        assert_eq!(resolve(src), resolve_node(&parse.syntax()));
    }

    #[test]
    fn arbitrary_input_does_not_panic() {
        for src in ["", "}{)(", "class", "int x = ;;;", "🦀 class C {"] {
            let _ = resolve(src);
        }
    }
}
