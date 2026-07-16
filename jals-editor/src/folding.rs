//! Protocol-neutral folding ranges from the lossless CST.
//!
//! Syntax-only: a fold is emitted for each brace-delimited body node (class/enum/module bodies,
//! statement and block-bodied-lambda blocks — which also cover control-flow via their `BLOCK`
//! child — switch blocks, and array initializers), for each multi-line block/doc comment, and
//! for a consecutive group of imports. There is no name resolution. Single-line constructs
//! (`{}`, `{ return; }`, a one-line `/* */`) are skipped: a fold must span at least two lines.
//!
//! Folding is inherently line-based, so folds are expressed in zero-based lines over the shared
//! [`LineIndex`]; hosts only map them to their protocol's shape (LSP `FoldingRange`, Monaco's
//! one-based ranges).

use alloc::vec::Vec;

use jals_syntax::ast::{AstNode, SourceFile};
use jals_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

use crate::LineIndex;

/// What a fold covers — hosts that distinguish fold flavors map this to their protocol kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FoldKind {
    /// A brace-delimited body (`{ … }`).
    Region,
    /// A multi-line block/doc comment.
    Comment,
    /// A consecutive group of import declarations.
    Imports,
}

/// One fold: an inclusive zero-based line span (spanning at least two lines) and its kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fold {
    /// The first folded line.
    pub start_line: u32,
    /// The last folded line. For a brace fold this is the line *before* the closing `}`, keeping
    /// the brace visible after folding (rust-analyzer / TypeScript-LS convention).
    pub end_line: u32,
    /// What the fold covers.
    pub kind: FoldKind,
}

/// Computes a file's folding ranges.
pub struct Folds;

impl Folds {
    /// Compute the folds of `root` over `text` (the source it was parsed from).
    pub fn of(root: &SyntaxNode, text: &str, index: &LineIndex) -> Vec<Fold> {
        let line_of = |offset: usize| index.position(text, offset).line;
        let mut folds = Vec::new();

        // Brace-delimited body nodes. A `BLOCK` is the body of methods/constructors/initializers
        // and of every control-flow statement (if/while/for/try/...) and block-bodied lambda, so
        // matching `BLOCK` covers those without enumerating each statement kind.
        for node in root.descendants() {
            if Self::is_foldable_body(node.kind()) {
                let range = node.text_range();
                let open = line_of(usize::from(range.start()));
                let close = line_of(Self::last_offset(range));
                // Fold up to the line *before* the closing brace, keeping it visible.
                folds.extend(Self::line_fold(
                    open,
                    close.saturating_sub(1),
                    FoldKind::Region,
                ));
            }
        }

        // Multi-line block/doc comments (license headers, Javadoc). Line comments are one line
        // each and are not folded.
        for token in root
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
        {
            if matches!(
                token.kind(),
                SyntaxKind::BLOCK_COMMENT | SyntaxKind::DOC_COMMENT
            ) {
                let range = token.text_range();
                let start = line_of(usize::from(range.start()));
                let end = line_of(Self::last_offset(range));
                folds.extend(Self::line_fold(start, end, FoldKind::Comment));
            }
        }

        // A consecutive group of imports collapses into one region (first import line .. last
        // import line), so a long import block can be tucked away.
        if let Some(file) = SourceFile::cast(root.clone()) {
            let mut imports = file.imports();
            if let Some(first) = imports.next()
                && let Some(last) = imports.last()
            {
                let start = line_of(usize::from(first.syntax().text_range().start()));
                let end = line_of(Self::last_offset(last.syntax().text_range()));
                folds.extend(Self::line_fold(start, end, FoldKind::Imports));
            }
        }

        folds
    }

    /// The brace-delimited body node kinds we fold.
    const fn is_foldable_body(kind: SyntaxKind) -> bool {
        use SyntaxKind::{ARRAY_INIT, BLOCK, CLASS_BODY, ENUM_BODY, MODULE_BODY, SWITCH_BLOCK};
        matches!(
            kind,
            CLASS_BODY | ENUM_BODY | MODULE_BODY | BLOCK | SWITCH_BLOCK | ARRAY_INIT
        )
    }

    /// Build a line fold, or `None` if it does not span at least two lines.
    fn line_fold(start_line: u32, end_line: u32, kind: FoldKind) -> Option<Fold> {
        (start_line < end_line).then_some(Fold {
            start_line,
            end_line,
            kind,
        })
    }

    /// Offset of the last byte of `range` (`end` − 1), clamped so an empty range never
    /// underflows. Lands on the closing `}` / `*/` / `;` glyph.
    fn last_offset(range: text_size::TextRange) -> usize {
        usize::from(range.end()).saturating_sub(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(start_line, end_line, kind)` tuples, sorted, for order-independent asserts.
    fn folds(text: &str) -> Vec<(u32, u32, FoldKind)> {
        let mut v: Vec<_> = Folds::of(
            &jals_syntax::Parse::parse(text).syntax(),
            text,
            &LineIndex::new(text),
        )
        .into_iter()
        .map(|f| (f.start_line, f.end_line, f.kind))
        .collect();
        v.sort_by_key(|&(s, e, _)| (s, e));
        v
    }

    #[test]
    fn class_and_method_bodies_fold() {
        // 0: class C {
        // 1:   void m() {
        // 2:     return;
        // 3:   }
        // 4: }
        // Closing braces stay visible: the class body folds up to line 3 (its `}` is on
        // line 4) and the method block up to line 2 (its `}` is on line 3).
        let f = folds("class C {\n  void m() {\n    return;\n  }\n}");
        assert!(f.contains(&(0, 3, FoldKind::Region)), "class body: {f:?}");
        assert!(f.contains(&(1, 2, FoldKind::Region)), "method block: {f:?}");
    }

    #[test]
    fn single_line_block_has_no_fold() {
        // Everything on one line -> nothing spans >= 2 lines.
        assert!(folds("class C { void m() {} }").is_empty());
    }

    #[test]
    fn nested_blocks_each_fold() {
        // 0: class C {
        // 1:   void m() {
        // 2:     if (x) {
        // 3:       y();
        // 4:     }
        // 5:   }
        // 6: }
        let f = folds("class C {\n  void m() {\n    if (x) {\n      y();\n    }\n  }\n}");
        let regions = f.iter().filter(|t| t.2 == FoldKind::Region).count();
        assert_eq!(regions, 3, "class body + method + if: {f:?}");
        assert!(f.contains(&(0, 5, FoldKind::Region)));
        assert!(f.contains(&(1, 4, FoldKind::Region)));
        assert!(f.contains(&(2, 3, FoldKind::Region)));
    }

    #[test]
    fn array_init_and_switch_block_fold() {
        // 1:   int[] a = {
        // 2:     1,
        // 3:     2,
        // 4:   };
        let arr = folds("class C {\n  int[] a = {\n    1,\n    2,\n  };\n}");
        assert!(
            arr.contains(&(1, 3, FoldKind::Region)),
            "array init: {arr:?}"
        );

        // 2:     switch (x) {
        // 3:       case 1: break;
        // 4:     }
        let sw = folds(
            "class C {\n  void m(int x) {\n    switch (x) {\n      case 1: break;\n    }\n  }\n}",
        );
        assert!(
            sw.contains(&(2, 3, FoldKind::Region)),
            "switch block: {sw:?}"
        );
    }

    #[test]
    fn multiline_comment_folds_as_comment() {
        // 0: /*
        // 1:  * header
        // 2:  */
        let f = folds("/*\n * header\n */\nclass C {}");
        assert!(
            f.contains(&(0, 2, FoldKind::Comment)),
            "block comment: {f:?}"
        );
    }

    #[test]
    fn one_line_comment_has_no_fold() {
        assert!(folds("/* one line */\nclass C {}").is_empty());
    }

    #[test]
    fn import_group_folds() {
        // 0: import java.util.List;
        // 1: import java.util.Map;
        // 2: import java.util.Set;
        let f = folds(
            "import java.util.List;\nimport java.util.Map;\nimport java.util.Set;\nclass C {}",
        );
        assert!(
            f.contains(&(0, 2, FoldKind::Imports)),
            "import group: {f:?}"
        );
    }

    #[test]
    fn single_import_has_no_fold() {
        assert!(folds("import java.util.List;\nclass C {}").is_empty());
    }

    #[test]
    fn never_panics_on_broken_input() {
        for src in [
            "",
            "class",
            "class C {",
            "/* unterminated",
            "{\n}\n",
            "import",
        ] {
            let _ = folds(src);
        }
    }
}
