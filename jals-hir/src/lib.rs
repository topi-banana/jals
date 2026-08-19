#![cfg_attr(not(test), no_std)]
// Every id in the index/resolver (`ItemId`, `MemberId`, `ScopeId`, `DefId`, and the reserved
// `FileId` blocks) is a `u32` allocated from a monotonic `Vec` length or enumeration index. Narrowing
// that `usize` count/index to the `u32` id representation is the deliberate id width — the id space is
// `u32` by design and never approaches its limit — so the truncation lint is allowed crate-wide
// rather than papered over with `as`-site attributes.
#![allow(clippy::cast_possible_truncation)]
//! Semantic analysis for Java/JALS source, over the `jals-syntax` CST.
//!
//! The analysis binds each *reference* (an identifier use) to the *definition* (binding) it names,
//! resolves type names across a project, and infers a type for every declaration and expression.
//! This is the foundation for go-to-definition, find-references, unused-binding detection, hover,
//! completion, and every semantic lint.
//!
//! Three layers, in the one order they compose:
//! - **File-local** ([`FileAnalysis`]): binds value, method, and type-name references within one
//!   file. Resolved: locals, parameters (method / constructor / lambda), fields (including forward
//!   references), methods (bare-callee calls), type parameters, enum constants, catch / resource /
//!   for-each / pattern variables, and file-local type names (a sibling class, a type parameter).
//!   Left [`Unresolved`](Resolution::Unresolved): member-access right-hand names (`obj.field` —
//!   needs a type) and any name with no file-local definition (imported or external types,
//!   inherited members). `this` / `super` are not recorded as references at all.
//! - **Project-wide** ([`ProjectIndex`]): a symbol index over many files. It resolves the
//!   type-name references the file-local pass left [`Unresolved`](Resolution::Unresolved) against
//!   the project's other source files — the basis for cross-file go-to-definition and "cannot
//!   resolve symbol".
//! - **Bound and typed** ([`FileSemantics`] → [`TypedFile`]): a [`FileAnalysis`] bound to a
//!   [`ProjectIndex`] with [`in_project`](FileAnalysis::in_project), which assigns each declaration
//!   and expression a structural [`Ty`] from both. It covers the structural / local subset
//!   (literals, names, arithmetic, casts, `new`, arrays, `var`) and member access (`obj.field`,
//!   `recv.method()`) on project types; an external type's members and target-typed forms
//!   (lambdas, method references, switch expressions) stay [`Ty::Unknown`].
//!
//! **The order is the interface.** A caller never sequences the layers itself: it analyses a file,
//! binds it to a project, and asks. The inference is run once per binding, on demand, and shared —
//! which is why the intermediate resolution and inference results are not exported. Holding one
//! would be holding a step.
//!
//! It never panics: an incomplete or erroneous tree yields a best-effort result, an unresolvable
//! reference is recorded as [`Resolution::Unresolved`], and an un-inferable type is [`Ty::Unknown`].
//!
//! # Example
//!
//! ```
//! use jals_hir::FileAnalysis;
//! let analysis = jals_exec::block_on_inline(FileAnalysis::parse(
//!     "class C { int x; int get() { return x; } }",
//! ));
//! // The `x` in `return x;` resolves back to the field `x`.
//! let r = analysis.references().iter().find(|r| r.name == "x").unwrap();
//! let jals_hir::Resolution::Def(id) = r.resolution else { panic!("x should resolve") };
//! assert_eq!(analysis.def(id).name, "x");
//! ```

extern crate alloc;

mod analysis;
mod classpath;
mod dead_if;
mod def;
mod imports;
mod infer;
mod project;
mod reference;
mod resolve;
mod scope;
mod stdlib;
mod throws;
mod ty;

pub use analysis::{FileAnalysis, FileSemantics, TypedFile};
pub use dead_if::DeadIf;
pub use def::{Def, DefId, DefKind, Namespace};
pub use imports::UnusedImport;
pub use infer::{Completion, MismatchKind, Signature, SignatureHelp, TypeMismatch};
pub use project::{
    FileFacts, FileId, Fqn, Item, ItemId, ItemOrigin, LoweredClasspath, Member, MemberId,
    MemberModifiers, MemberType, Param, ProjectIndex, ProjectIndexBuilder, SourceLocations,
    Supertype, TypeParamDecl, TypeResolution, UnresolvedType,
};
pub use reference::{Reference, Resolution};
pub use throws::UnreportedException;
pub use ty::{ClassTy, Primitive, Ty};

#[cfg(test)]
mod tests {
    use super::*;
    use jals_exec::block_on_inline;

    /// Synchronous test-side driver for the async [`FileAnalysis::parse`].
    fn resolve(src: &str) -> FileAnalysis {
        block_on_inline(FileAnalysis::parse(src))
    }

    /// The `Resolution` of the first reference named `name`.
    fn resolution_of(resolved: &FileAnalysis, name: &str) -> Resolution {
        resolved
            .references()
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

    /// Parsing inside [`FileAnalysis::parse`] and analysing a tree the caller already parsed reach
    /// the same answer — the two entry points differ only in who owns the parse.
    #[test]
    fn parsing_and_analysing_an_existing_tree_agree() {
        let src = "class C { void m() { int x = 1; use(x); } }";
        let parse = block_on_inline(jals_syntax::Parse::parse(src));
        let parsed = resolve(src);
        let existing = block_on_inline(FileAnalysis::of(&parse.syntax()));
        assert_eq!(parsed.defs(), existing.defs());
        assert_eq!(parsed.references(), existing.references());
    }

    /// Without an index a reference type name is known only by spelling, but the structural
    /// inference (the `int`, the `var`) still answers. Lives here because the file-local inference
    /// it exercises is a step of the analysis and is not exported.
    #[test]
    fn project_free_inference_names_reference_types_externally() {
        let src = "class C { void m() { Helper h = make(); var n = 1; } } class Helper { }";
        let analysis = resolve(src);
        let inference = block_on_inline(infer::TypeInference::infer_node(
            analysis.root(),
            analysis.resolved(),
        ));
        let helper = analysis.defs().iter().find(|d| d.name == "h").unwrap();
        let n = analysis.defs().iter().find(|d| d.name == "n").unwrap();
        assert_eq!(inference.type_of_def(helper.id).to_string(), "Helper");
        assert_eq!(inference.type_of_def(n.id).to_string(), "int");
    }

    #[test]
    fn arbitrary_input_does_not_panic() {
        for src in ["", "}{)(", "class", "int x = ;;;", "🦀 class C {"] {
            let _ = resolve(src);
        }
    }
}
