//! Builds LSP folding ranges from the lossless CST.
//!
//! Syntax-only: a range is emitted for each brace-delimited body node (class/enum/module
//! bodies, statement and block-bodied-lambda blocks — which also cover control-flow via
//! their `BLOCK` child — switch blocks, and array initializers), for each multi-line
//! block/doc comment, and for a consecutive group of imports. There is no name resolution.
//! Single-line constructs (`{}`, `{ return; }`, a one-line `/* */`) are skipped: a fold
//! must span at least two lines.

use async_lsp::lsp_types::{FoldingRange, FoldingRangeKind};
use jals_syntax::ast::{AstNode, SourceFile};
use jals_syntax::{Parse, SyntaxElement, SyntaxKind, SyntaxNode};
use text_size::{TextRange, TextSize};

use crate::line_index::LineIndex;

/// Compute folding ranges from the cached parse of `text`.
pub(crate) fn folding_range(
    parse: &Parse,
    text: &str,
    line_index: &LineIndex,
) -> Vec<FoldingRange> {
    let root = parse.syntax();
    let mut ranges = Vec::new();

    // Brace-delimited body nodes. A `BLOCK` is the body of methods/constructors/initializers
    // and of every control-flow statement (if/while/for/try/...) and block-bodied lambda, so
    // matching `BLOCK` covers those without enumerating each statement kind.
    for node in root.descendants() {
        if is_foldable_body(node.kind())
            && let Some(r) = brace_fold(&node, text, line_index)
        {
            ranges.push(r);
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
            let start = line_index.position(text, range.start()).line;
            let end = line_index.position(text, last_offset(range)).line;
            if let Some(r) = line_fold(start, end, Some(FoldingRangeKind::Comment)) {
                ranges.push(r);
            }
        }
    }

    // A consecutive group of imports collapses into one region (first import line .. last
    // import line), so a long import block can be tucked away.
    if let Some(file) = SourceFile::cast(root) {
        let mut imports = file.imports();
        if let Some(first) = imports.next()
            && let Some(last) = imports.last()
        {
            let start = line_index
                .position(text, first.syntax().text_range().start())
                .line;
            let end = line_index
                .position(text, last_offset(last.syntax().text_range()))
                .line;
            if let Some(r) = line_fold(start, end, Some(FoldingRangeKind::Imports)) {
                ranges.push(r);
            }
        }
    }

    ranges
}

/// The brace-delimited body node kinds we fold.
const fn is_foldable_body(kind: SyntaxKind) -> bool {
    use SyntaxKind::{ARRAY_INIT, BLOCK, CLASS_BODY, ENUM_BODY, MODULE_BODY, SWITCH_BLOCK};
    matches!(
        kind,
        CLASS_BODY | ENUM_BODY | MODULE_BODY | BLOCK | SWITCH_BLOCK | ARRAY_INIT
    )
}

/// Fold a `{ ... }` node from the line of its `{` to the line *before* its `}`, keeping the
/// closing brace visible after folding (rust-analyzer / TypeScript-LS convention).
fn brace_fold(node: &SyntaxNode, text: &str, idx: &LineIndex) -> Option<FoldingRange> {
    let range = node.text_range();
    let open = idx.position(text, range.start()).line;
    let close = idx.position(text, last_offset(range)).line;
    line_fold(open, close.saturating_sub(1), None)
}

/// Build a line-based fold, or `None` if it does not span at least two lines.
fn line_fold(
    start_line: u32,
    end_line: u32,
    kind: Option<FoldingRangeKind>,
) -> Option<FoldingRange> {
    if start_line < end_line {
        Some(FoldingRange {
            start_line,
            end_line,
            kind,
            ..FoldingRange::default()
        })
    } else {
        None
    }
}

/// Offset of the last byte of `range` (`end` − 1), clamped so an empty range never
/// underflows. Lands on the closing `}` / `*/` / `;` glyph.
fn last_offset(range: TextRange) -> TextSize {
    range
        .end()
        .checked_sub(TextSize::from(1))
        .unwrap_or_else(|| range.end())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(start_line, end_line, kind)` tuples, sorted, for order-independent asserts.
    fn folds(text: &str) -> Vec<(u32, u32, Option<FoldingRangeKind>)> {
        let mut v: Vec<_> = folding_range(&jals_syntax::parse(text), text, &LineIndex::new(text))
            .into_iter()
            .map(|r| (r.start_line, r.end_line, r.kind))
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
        assert!(f.contains(&(0, 3, None)), "class body: {f:?}");
        assert!(f.contains(&(1, 2, None)), "method block: {f:?}");
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
        let blocks = f.iter().filter(|t| t.2.is_none()).count();
        assert_eq!(blocks, 3, "class body + method + if: {f:?}");
        assert!(f.contains(&(0, 5, None)));
        assert!(f.contains(&(1, 4, None)));
        assert!(f.contains(&(2, 3, None)));
    }

    #[test]
    fn array_init_and_switch_block_fold() {
        // 1:   int[] a = {
        // 2:     1,
        // 3:     2,
        // 4:   };
        let arr = folds("class C {\n  int[] a = {\n    1,\n    2,\n  };\n}");
        assert!(arr.contains(&(1, 3, None)), "array init: {arr:?}");

        // 2:     switch (x) {
        // 3:       case 1: break;
        // 4:     }
        let sw = folds(
            "class C {\n  void m(int x) {\n    switch (x) {\n      case 1: break;\n    }\n  }\n}",
        );
        assert!(sw.contains(&(2, 3, None)), "switch block: {sw:?}");
    }

    #[test]
    fn multiline_comment_folds_as_comment() {
        // 0: /*
        // 1:  * header
        // 2:  */
        let f = folds("/*\n * header\n */\nclass C {}");
        assert!(
            f.contains(&(0, 2, Some(FoldingRangeKind::Comment))),
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
            f.contains(&(0, 2, Some(FoldingRangeKind::Imports))),
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
