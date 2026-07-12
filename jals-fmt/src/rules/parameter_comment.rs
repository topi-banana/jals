//! `normalize-parameter-comments`: rewrite a parameter-name block comment (`/*a=*/`) into
//! google-java-format's canonical `/* a= */` form.
//!
//! A plain function with a single call site in [`crate::comments`] (it rewrites comment
//! trivia, not a significant token), kept under `rules/` so every opt-in transformation lives
//! together — exactly like [`crate::rules::trailing_comma`]. Mirrors google-java-format's
//! `CommentsHelper.reformatParameterComment`: the *whole* comment must match
//! `/* <name>(...)? = */`, the name a Java identifier optionally suffixed with `...` (varargs).

use alloc::format;
use alloc::string::String;

/// The `normalize-parameter-comments` rewrite. A zero-sized handle grouping the parameter-comment
/// normalization helpers so they are reached through the type rather than as free functions.
pub(crate) struct ParameterComment;

impl ParameterComment {
    /// Rewrite a `BLOCK_COMMENT` whose entire text is a parameter-name label (`/*a=*/`,
    /// `/* xs...= */`, `/*  a  =  */`) into the canonical `/* <name>= */`, collapsing interior
    /// whitespace. Returns `None` when the text is not such a comment, so the caller keeps it
    /// verbatim — including for Javadoc, which is a separate token kind and never reaches here.
    ///
    /// Already-canonical input (`/* a= */`) maps to a byte-identical string, so the rewrite is a
    /// fixed point and formatting stays idempotent.
    pub(crate) fn normalize(text: &str) -> Option<String> {
        // Strip the `/*` … `*/` fence. (A `DOC_COMMENT` `/** … */` is a different token kind and is
        // not passed in; a `/**/`-style comment has no `=` and fails below.)
        let inner = text.strip_prefix("/*")?.strip_suffix("*/")?;
        // Require a trailing `=` after trimming the surrounding whitespace.
        let body = inner.trim();
        let name = body.strip_suffix('=')?.trim_end();
        // The identifier itself contains no whitespace, so any interior whitespace left after the
        // `=` is stripped (e.g. a stray second `=` in `/* a == */`) is a non-match.
        if name.is_empty() || name.chars().any(char::is_whitespace) {
            return None;
        }
        // Validate `<java-identifier>` optionally followed by `...` (varargs).
        let core = name.strip_suffix("...").unwrap_or(name);
        if !Self::is_java_identifier(core) {
            return None;
        }
        Some(format!("/* {name}= */"))
    }

    /// Whether `s` is a Java identifier: a non-empty run whose first char is an identifier *start*
    /// and the rest identifier *parts*. Approximates `\p{javaJavaIdentifierStart}` /
    /// `\p{javaJavaIdentifierPart}` — exact for the ASCII names these comments use in practice.
    fn is_java_identifier(s: &str) -> bool {
        let mut chars = s.chars();
        match chars.next() {
            Some(c) if Self::is_ident_start(c) => {}
            _ => return false,
        }
        chars.all(Self::is_ident_part)
    }

    fn is_ident_start(c: char) -> bool {
        c.is_alphabetic() || c == '_' || c == '$'
    }

    fn is_ident_part(c: char) -> bool {
        c.is_alphanumeric() || c == '_' || c == '$'
    }
}

#[cfg(test)]
mod tests {
    use super::ParameterComment;

    fn normalize(text: &str) -> Option<alloc::string::String> {
        ParameterComment::normalize(text)
    }

    #[test]
    fn normalizes_a_bare_parameter_comment() {
        assert_eq!(normalize("/*a=*/").as_deref(), Some("/* a= */"));
    }

    #[test]
    fn normalizes_varargs_parameter_comment() {
        assert_eq!(normalize("/*xs...=*/").as_deref(), Some("/* xs...= */"));
    }

    #[test]
    fn collapses_interior_whitespace() {
        assert_eq!(normalize("/*  a  =  */").as_deref(), Some("/* a= */"));
        assert_eq!(normalize("/* a = */").as_deref(), Some("/* a= */"));
    }

    #[test]
    fn already_canonical_is_a_fixed_point() {
        assert_eq!(normalize("/* a= */").as_deref(), Some("/* a= */"));
        assert_eq!(normalize("/* xs...= */").as_deref(), Some("/* xs...= */"));
    }

    #[test]
    fn accepts_underscore_dollar_and_digits_in_name() {
        assert_eq!(normalize("/*_x=*/").as_deref(), Some("/* _x= */"));
        assert_eq!(normalize("/*$a=*/").as_deref(), Some("/* $a= */"));
        assert_eq!(normalize("/*arg1=*/").as_deref(), Some("/* arg1= */"));
    }

    #[test]
    fn rejects_non_parameter_comments() {
        // No `=`.
        assert_eq!(normalize("/**/"), None);
        assert_eq!(normalize("/* hello */"), None);
        // No name before `=`.
        assert_eq!(normalize("/* = */"), None);
        assert_eq!(normalize("/*=*/"), None);
        // A stray second `=` leaves interior whitespace after stripping one `=`.
        assert_eq!(normalize("/* a == */"), None);
        // Multiple words.
        assert_eq!(normalize("/* a b= */"), None);
        // A leading digit is not a valid identifier start.
        assert_eq!(normalize("/*1=*/"), None);
        // Not a block comment fence at all.
        assert_eq!(normalize("// a="), None);
    }

    #[test]
    fn double_application_is_idempotent() {
        let once = normalize("/*a=*/").unwrap();
        assert_eq!(normalize(&once).as_deref(), Some(once.as_str()));
    }
}
