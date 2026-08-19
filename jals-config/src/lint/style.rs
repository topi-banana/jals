//! `[style]` — the code is correct and does not read the way Java is conventionally written.
//!
//! A style finding names a construct that would be *rewritten* rather than deleted (that is
//! `[complexity]`) or renamed (that is `[naming]`). Both rules here carry an option, because both
//! conventions have a well-attested second answer that is a project policy rather than a mistake:
//! static wildcard imports, and braces on multi-line bodies only.

use serde::{Deserialize, Serialize};

use super::LintOptions;

/// What `wildcard-import` does with `import static a.B.*;`.
///
/// A static wildcard is the conventional way to pull in a test DSL's assertions
/// (`import static org.junit.jupiter.api.Assertions.*;`), so the two forms are separated: a
/// project can ban on-demand type imports while keeping the static ones. This is one key rather
/// than a second rule because the two answers are exclusive — a static wildcard import is either
/// reported or it is not, and two rules would let a config say both.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StaticWildcard {
    /// Report `import static a.B.*;` like any other wildcard. The default.
    #[default]
    Report,
    /// Leave static wildcards alone; report only on-demand type imports.
    Allow,
}

/// `wildcard-import` options.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct WildcardImport {
    /// Whether a static wildcard import is reported.
    pub static_imports: StaticWildcard,
}

/// When a control-flow body has to be a `{ ... }` block.
///
/// The three answers Java style guides actually give, as one exclusive choice. Checkstyle spells
/// the same thing as `NeedBraces` with an `allowSingleLineStatement` flag — a boolean beside a
/// boolean, where the middle answer is only reachable by setting both.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BracePolicy {
    /// Every body, however short. The default, and google-java-format's rule.
    #[default]
    Always,
    /// Only a body that does not fit on the same line as its keyword — so `if (x) return;` passes
    /// and a bare statement on the next line does not.
    MultiLine,
}

/// `missing-braces` options.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct MissingBraces {
    /// When braces are required.
    pub policy: BracePolicy,
}

/// See [`LintOptions`]: this rule takes options, so it always serializes as a table.
impl LintOptions for WildcardImport {}

/// See [`LintOptions`]: this rule takes options, so it always serializes as a table.
impl LintOptions for MissingBraces {}

lint_section! {
    /// `[style]` — correct code written unconventionally.
    Style: Style {
        /// `wildcard-import` — `import java.util.*;`, including the member form a jals grouped
        /// import spells as `import java.util.{concurrent.*};`. A wildcard makes the file's
        /// dependencies unreadable and its resolution sensitive to what a library adds later.
        "wildcard-import" => wildcard_import: WildcardImport = Warn,
        /// `missing-braces` — an `if`, `else`, `while`, `for` or `do` body written as a bare
        /// statement. An `else if` chain is not reported for its `else`; the trailing `if` is
        /// checked on its own.
        "missing-braces" => missing_braces: MissingBraces = Warn,
    }
}
