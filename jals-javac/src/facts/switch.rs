//! What one `switch` arm's labels say.
//!
//! The `facts` header lists the arm reader among the three copies that were "duplicated down to
//! its explanatory comment", and it was: the two backends held the same forty lines, the same
//! pattern-kind list, and the same note about a `Guard`'s condition, differing only in the last
//! statement of the loop. That note is the load-bearing part — a `when` clause's condition is an
//! expression child of the label just as a `case` key is, so a reader that took every expression
//! child folded the guard into the jump table.
//!
//! What each backend does with the answer is its own: the JVM allocates the arm's entry label and
//! wasm rejects a `String` key it has no representation for. Neither is a fact about the source.

use alloc::vec::Vec;

use jals_syntax::SyntaxNode;
use jals_syntax::ast::{self, AstNode as _};

use super::{CaseKey, FactError, Facts, Result};

/// One arm's labels, as the source wrote them.
pub(crate) struct ArmLabels {
    /// The `case` keys reaching this arm, already folded. Empty for a bare `default`.
    pub(crate) keys: Vec<CaseKey>,
    /// The `case T t` patterns reaching this arm, in the order they are written.
    ///
    /// A pattern is not a constant, so it indexes no jump table: a `switch` with one dispatches by
    /// testing each arm's type in source order, which is what JLS §14.11.1 says a pattern `switch`
    /// does.
    pub(crate) patterns: Vec<SyntaxNode>,
    /// The arm's `when` clause, which runs after the pattern bound and before the arm is taken.
    pub(crate) guard: Option<ast::Expr>,
    /// Whether one of this arm's labels is `default`.
    pub(crate) is_default: bool,
}

impl Facts<'_> {
    /// What one arm's `case` / `default` labels say.
    pub(crate) fn switch_arm(
        self,
        labels: impl Iterator<Item = ast::SwitchLabel>,
    ) -> Result<ArmLabels> {
        use jals_syntax::SyntaxKind::{RECORD_PATTERN, TYPE_PATTERN, UNNAMED_PATTERN};
        let mut out = ArmLabels {
            keys: Vec::new(),
            patterns: Vec::new(),
            guard: None,
            is_default: false,
        };
        for label in labels {
            if label.is_default() {
                out.is_default = true;
            }
            out.patterns
                .extend(label.syntax().children().filter(|child| {
                    matches!(
                        child.kind(),
                        TYPE_PATTERN | RECORD_PATTERN | UNNAMED_PATTERN
                    )
                }));
            if let Some(clause) = label.syntax().children().find_map(ast::Guard::cast) {
                out.guard = clause.condition();
                if out.guard.is_none() {
                    return Err(FactError::Unsupported("a guarded `case`"));
                }
            }
            // A `Guard`'s condition is an expression child of the label too, so the keys are read
            // only when there is no guard to have contributed one.
            if out.guard.is_none() {
                for value in label.syntax().children().filter_map(ast::Expr::cast) {
                    out.keys.push(self.case_key(&value)?);
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use alloc::vec::Vec;

    use jals_exec::block_on_inline;
    use jals_hir::{FileAnalysis, FileId, ProjectIndex};
    use jals_syntax::ast::{self, AstNode as _};

    use super::{ArmLabels, Facts};
    use crate::facts::CaseKey;

    /// Every arm of the first `switch` in `source`, read.
    fn arms(source: &str) -> Vec<ArmLabels> {
        let root = block_on_inline(jals_syntax::Parse::parse(source)).syntax();
        let analysis = block_on_inline(FileAnalysis::of(&root));
        let index = block_on_inline(
            ProjectIndex::builder(&[(FileId(0), root.clone())])
                .with_stdlib()
                .build(),
        );
        let semantics = analysis.in_project(&index, FileId(0));
        let facts = Facts::of(block_on_inline(semantics.typed()));
        root.descendants()
            .filter_map(ast::SwitchGroup::cast)
            .map(|group| {
                facts
                    .switch_arm(group.labels())
                    .expect("the arm's labels are readable")
            })
            .collect()
    }

    /// A `when` clause is the arm's guard and not one of its keys.
    ///
    /// The condition is an expression child of the label exactly as a `case` key is, so a reader
    /// that took every expression child folded the guard into the jump table — a key whose value is
    /// whatever the guard happens to evaluate to, in a class file that verifies. That is the note
    /// the two copies of this reader each carried.
    #[test]
    fn a_guards_condition_is_not_a_key() {
        let source = "class C { void m(Object o) { switch (o) { \
                      case Integer i when i > 0: break; case String s: break; default: break; } } }";
        let read = arms(source);
        let shapes: Vec<(usize, usize, bool, bool)> = read
            .iter()
            .map(|arm| {
                (
                    arm.keys.len(),
                    arm.patterns.len(),
                    arm.guard.is_some(),
                    arm.is_default,
                )
            })
            .collect();
        assert_eq!(
            shapes,
            [
                (0, 1, true, false),
                (0, 1, false, false),
                (0, 0, false, true)
            ]
        );
    }

    /// Several `case` labels on one arm are that arm's keys, in written order, and a `default`
    /// sharing the arm does not erase them.
    #[test]
    fn every_case_label_on_an_arm_is_one_of_its_keys() {
        let source = "class C { void m(int n) { switch (n) { \
                      case 1: case 2 + 3: break; case 4: default: break; } } }";
        let read = arms(source);
        let keys: Vec<Vec<CaseKey>> = read.iter().map(|arm| arm.keys.clone()).collect();
        assert_eq!(
            keys,
            [
                alloc::vec![CaseKey::Int(1), CaseKey::Int(5)],
                alloc::vec![CaseKey::Int(4)],
            ]
        );
        assert_eq!(
            read.iter().map(|arm| arm.is_default).collect::<Vec<_>>(),
            [false, true]
        );
    }

    /// A `String` key is read here and rejected by the backend that has no representation for one:
    /// what the source says and what a target can hold are different questions.
    #[test]
    fn a_string_key_is_read_rather_than_refused() {
        let source = r#"class C { void m(String s) { switch (s) { case "a": break; } } }"#;
        let read = arms(source);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].keys, [CaseKey::Text(String::from("a"))]);
    }
}
