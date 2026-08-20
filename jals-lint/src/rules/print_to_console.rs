//! `print-to-console`: a call on `System.out` or `System.err`.
//!
//! Ports `clippy::print_stdout` and `clippy::print_stderr` as **one** rule with a
//! [`streams`](jals_config::lint::PrintToConsole::streams) key. The pair is exclusive in practice
//! and not in the schema: two lints can be enabled in any combination, including the combination
//! neither of them names, while one key with three values reaches every reachable state and no
//! unreachable one.
//!
//! `[restriction]`, so `allow` by default: printing is correct Java, and only a project that has
//! chosen a logging framework has a reason to call it a finding.
//!
//! The receiver is matched by **spelling** — `System.out`, `System.err`, and their fully qualified
//! forms — rather than by resolution. A local named `System` is the recognized false positive, and
//! it is the price of a rule that stays syntactic; a project that has one sets the rule back to
//! `allow`, which is where it started.

use alloc::string::String;
use alloc::vec::Vec;

use jals_config::Category;
use jals_config::lint::{Config, ConsoleStreams};
use jals_exec::{LocalBoxFuture, Yielder};
use jals_syntax::ast::{AstNode, CallExpr, Expr};
use jals_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "print-to-console",
    category: Category::Restriction,
    level: |config| config.restriction.print_to_console.level,
    needs_clean_parse: false,
    check: Checker::Syntactic(api::check),
};

/// The `print-to-console` rule.
mod api {
    use super::{
        AstNode, CallExpr, Config, ConsoleStreams, Expr, Finding, LocalBoxFuture, String,
        SyntaxElement, SyntaxKind, SyntaxNode, Vec, Yielder,
    };

    /// The table-edge shim: boxes the async rule body once per file.
    pub(crate) fn check<'a>(
        root: &'a SyntaxNode,
        config: &'a Config,
    ) -> LocalBoxFuture<'a, Vec<Finding>> {
        alloc::boxed::Box::pin(check_impl(root, config))
    }

    async fn check_impl(root: &SyntaxNode, config: &Config) -> Vec<Finding> {
        let streams = config.restriction.print_to_console.options.streams;
        let mut yielder = Yielder::new();
        let mut out = Vec::new();
        for node in root.descendants() {
            yielder.tick().await;
            let Some(call) = CallExpr::cast(node) else {
                continue;
            };
            // `System.out.println(x)` — the callee names the method on a receiver that is itself
            // the `System.out` field access.
            let Some(Expr::FieldAccess(callee)) = call.callee() else {
                continue;
            };
            let Some(receiver) = callee.receiver() else {
                continue;
            };
            let Some(stream) = console_stream(receiver.syntax()) else {
                continue;
            };
            if !reports(streams, stream) {
                continue;
            }
            out.push(Finding::at_node(
                call.syntax(),
                alloc::format!("writing to `System.{stream}`; log through the project's logger"),
            ));
        }
        out
    }

    /// Which console stream `receiver` names, if it names one. The receiver's significant tokens
    /// are joined so that `System.out` and `java.lang.System.out` answer alike whatever whitespace
    /// or comments were written between them.
    fn console_stream(receiver: &SyntaxNode) -> Option<&'static str> {
        // A receiver bigger than `java.lang.System.out` is not a qualified name at all.
        let mut spelling = String::new();
        for token in receiver
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|token| !token.kind().is_trivia())
        {
            if !matches!(token.kind(), SyntaxKind::IDENT | SyntaxKind::DOT) {
                return None;
            }
            spelling.push_str(token.text());
        }
        match spelling.as_str() {
            "System.out" | "java.lang.System.out" => Some("out"),
            "System.err" | "java.lang.System.err" => Some("err"),
            _ => None,
        }
    }

    /// Whether the configured `streams` covers `stream`.
    fn reports(streams: ConsoleStreams, stream: &str) -> bool {
        match streams {
            ConsoleStreams::Both => true,
            ConsoleStreams::Stdout => stream == "out",
            ConsoleStreams::Stderr => stream == "err",
        }
    }
}
