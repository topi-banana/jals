#![cfg_attr(not(test), no_std)]
//! File-local name resolution for Java/JALS source, over the `jals-syntax` CST.
//!
//! [`resolve`] binds each *reference* (an identifier use) to the *definition* (binding) it names,
//! within a single source file. This is the foundation for go-to-definition, find-references,
//! unused-binding detection, and type inference.
//!
//! Three layers:
//! - **File-local** ([`resolve`] / [`resolve_node`] → [`Resolved`]): binds value, method, and
//!   type-name references within one file. Resolved: locals, parameters (method / constructor /
//!   lambda), fields (including forward references), methods (bare-callee calls), type parameters,
//!   enum constants, catch / resource / for-each / pattern variables, and file-local type names
//!   (a sibling class, a type parameter). Left [`Unresolved`](Resolution::Unresolved):
//!   member-access right-hand names
//!   (`obj.field` — needs a type) and any name with no file-local definition (imported or external
//!   types, inherited members). `this` / `super` are not recorded as references at all.
//! - **Project-wide** ([`ProjectIndex`]): a symbol index over many files. It resolves the
//!   type-name references the file-local pass left [`Unresolved`](Resolution::Unresolved) against
//!   the project's other
//!   source files — the basis for cross-file go-to-definition and "cannot resolve symbol".
//! - **Type inference** ([`infer`] / [`infer_node`] → [`TypeInference`]): assigns each declaration
//!   and expression a structural [`Ty`], reusing the [`Resolved`] bindings and the [`ProjectIndex`]
//!   for reference type names and members — the basis for hover and member go-to-definition. It
//!   covers the structural / local subset (literals, names, arithmetic, casts, `new`, arrays,
//!   `var`) and member access (`obj.field`, `recv.method()`) on project types, resolved through the
//!   [`ProjectIndex`] member model; an external type's members and target-typed forms (lambdas,
//!   method references, switch expressions) stay [`Ty::Unknown`].
//!
//! It never panics: an incomplete or erroneous tree yields a best-effort result, an unresolvable
//! reference is recorded as [`Resolution::Unresolved`], and an un-inferable type is [`Ty::Unknown`].
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

extern crate alloc;

mod classpath;
mod def;
mod infer;
mod project;
mod reference;
mod resolve;
mod scope;
mod stdlib;
mod throws;
mod ty;

use jals_syntax::SyntaxNode;

pub use def::{Def, DefId, DefKind, Namespace};
pub use infer::{
    Completion, Signature, SignatureHelp, TypeInference, TypeMismatch, at_member_access, infer,
    infer_node, member_completions, scope_completions, signature_help, type_mismatches,
};
pub use project::{
    FileFacts, FileId, Fqn, Item, ItemId, ItemOrigin, LoweredClasspath, Member, MemberId,
    MemberType, Param, ProjectIndex, ProjectIndexBuilder, SourceLocations, Supertype,
    TypeParamDecl, TypeResolution,
};
pub use reference::{Reference, Resolution};
pub use resolve::Resolved;
pub use scope::{Scope, ScopeId, ScopeKind};
pub use throws::{UnreportedException, unreported_exceptions};
pub use ty::{ClassTy, Primitive, Ty};

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
