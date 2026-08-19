//! `[complexity]` — the same meaning is available in a simpler form.
//!
//! A finding here is never about correctness or speed: the code does exactly what it says, and
//! says it with more syntax than it needs. The distinction from `[style]` is that a complexity
//! finding names a construct that can be *deleted* or *merged*, while a style finding names one
//! that would be *rewritten* — which is why an unnecessary nesting level is here and a wildcard
//! import is not.

use super::NoOptions;

lint_section! {
    /// `[complexity]` — syntax that can be merged or removed without changing meaning.
    Complexity: Complexity {
        /// `collapsible-if` — an `if` whose entire body is another `if`, where neither has an
        /// `else`: the two conditions can be joined with `&&`. Ports `clippy::collapsible_if`.
        /// An intervening statement, a comment between the two, or an `else` on either makes the
        /// nesting load-bearing, and none of those is reported.
        "collapsible-if" => collapsible_if: NoOptions = Warn,
    }
}
