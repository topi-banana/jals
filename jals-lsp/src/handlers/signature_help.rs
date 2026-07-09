//! Signature help (`textDocument/signatureHelp`): show the overloads of the method being called and
//! highlight the argument the cursor is on.
//!
//! The semantic work — finding the call, resolving its overloads, rendering each signature — is
//! [`jals_hir::signature_help`], which needs the project member model. The cross-file path lives on
//! the workspace ([`Workspace::signature_help`](crate::state::Workspace::signature_help)), which
//! holds the index; this module holds the **file-local** fallback ([`signature_help_local`], which
//! builds a single-file index so the document's own methods are visible) and [`signature_help_to_lsp`]
//! — the mapping from `jals-hir`'s pure shape to the LSP payload that both paths share.

// The overload/parameter indices and UTF-16 label lengths here are bounded by a single source
// document, so the `usize`/`u32` conversions cannot truncate in practice.
#![allow(clippy::cast_possible_truncation)]

use async_lsp::lsp_types::{
    ParameterInformation, ParameterLabel, Position, SignatureHelp, SignatureInformation,
};
use jals_hir::{FileId, ProjectIndex};
use jals_syntax::Parse;

use crate::line_index::LineIndex;

/// Maps `jals-hir`'s pure signature-help data to the LSP payload. Each parameter range is a byte
/// range into its signature's label; LSP wants UTF-16 code-unit offsets, so they are converted here.
pub fn signature_help_to_lsp(help: &jals_hir::SignatureHelp) -> SignatureHelp {
    let signatures = help
        .signatures
        .iter()
        .map(|sig| SignatureInformation {
            label: sig.label.clone(),
            documentation: None,
            parameters: Some(
                sig.parameters
                    .iter()
                    .map(|range| ParameterInformation {
                        label: ParameterLabel::LabelOffsets([
                            utf16_len(&sig.label[..range.start]),
                            utf16_len(&sig.label[..range.end]),
                        ]),
                        documentation: None,
                    })
                    .collect(),
            ),
            active_parameter: None,
        })
        .collect();
    SignatureHelp {
        signatures,
        active_signature: Some(help.active_signature as u32),
        active_parameter: Some(help.active_parameter as u32),
    }
}

/// The number of UTF-16 code units in `s` (LSP offsets are counted in UTF-16).
fn utf16_len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

/// The signature help for the call at `position`, computed over this one file by building a
/// single-file project index with the `java.lang` stubs folded in (so the document's own methods
/// and the core JDK methods are visible). The fallback for a file outside any indexed project; the
/// cross-file path is [`Workspace::signature_help`](crate::state::Workspace::signature_help).
pub fn signature_help_local(
    parse: &Parse,
    text: &str,
    line_index: &LineIndex,
    position: Position,
) -> Option<SignatureHelp> {
    let root = parse.syntax();
    let offset = u32::from(line_index.offset(text, position)) as usize;
    let index = ProjectIndex::builder(&[(FileId(0), root.clone())])
        .with_stdlib()
        .build();
    let resolved = jals_hir::resolve_node(&root);
    let help = jals_hir::signature_help(&root, &resolved, &index, FileId(0), offset)?;
    Some(signature_help_to_lsp(&help))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Signature help with the cursor placed just after the first occurrence of `cursor` in `text`.
    fn help_at(text: &str, cursor: &str) -> SignatureHelp {
        let line_index = LineIndex::new(text);
        let offset = text.find(cursor).expect("cursor substring") + cursor.len();
        let pos = line_index.position(text, text_size::TextSize::from(offset as u32));
        let parse = jals_syntax::parse(text);
        signature_help_local(&parse, text, &line_index, pos).expect("signature help")
    }

    #[test]
    fn renders_and_marks_the_active_parameter() {
        let help = help_at(
            "class C { int area(int w, int h) {} void g() { area(1, ); } }",
            "area(1, ",
        );
        assert_eq!(help.signatures.len(), 1);
        assert_eq!(help.signatures[0].label, "area(int w, int h)");
        assert_eq!(help.active_parameter, Some(1));
        assert_eq!(help.active_signature, Some(0));
    }

    #[test]
    fn parameter_label_offsets_are_utf16() {
        // `値` is 3 UTF-8 bytes but 1 UTF-16 code unit, so the 2nd parameter sits at UTF-16 9..14,
        // not byte 11..16 — the offsets must be counted in UTF-16.
        let text = "class C { void f(int 値, int b) {} void g() { f(1, ); } }";
        let help = help_at(text, "f(1, ");
        let params = help.signatures[0].parameters.as_ref().unwrap();
        let offsets: Vec<[u32; 2]> = params
            .iter()
            .map(|p| match p.label {
                ParameterLabel::LabelOffsets(o) => o,
                ParameterLabel::Simple(_) => panic!("expected label offsets"),
            })
            .collect();
        assert_eq!(offsets, vec![[2, 7], [9, 14]]);
    }

    #[test]
    fn no_help_outside_a_call() {
        let text = "class C { int x = 0; }";
        let line_index = LineIndex::new(text);
        let pos = line_index.position(text, text_size::TextSize::from(0));
        let parse = jals_syntax::parse(text);
        assert!(signature_help_local(&parse, text, &line_index, pos).is_none());
    }
}
