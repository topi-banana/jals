//! What a module carries when the compile asked for positions, and what a run does with it.
//!
//! A trap has no stack a host can walk — the engine's stack is gone by the time the failure
//! reaches it — so the backend leaves a breadcrumb instead: each statement writes its index into
//! an exported global before its own code runs, and the module carries the table that turns the
//! index into a file and a byte range. This is the run side of that arrangement, and of the
//! instruction budget a run can be held to.

#![cfg(feature = "wasm-run")]

use jals_hir::{FileAnalysis, FileId, FileSemantics, ProjectIndex, TypedFile};
use jals_javac::wasm::{CompileWasm, WasmOptions};
use jals_syntax::SyntaxNode;

use jals_build::{WasmRunError, WasmRunOutcome, WasmRunner, WasmValue};

/// The project's own module, compiled from `sources` with the options stated.
fn compile(sources: &[&str], options: WasmOptions) -> Vec<u8> {
    let roots: Vec<(FileId, SyntaxNode)> = sources
        .iter()
        .enumerate()
        .map(|(index, text)| {
            (
                FileId(u32::try_from(index).expect("a source count that fits")),
                jals_exec::block_on_inline(jals_syntax::Parse::parse(text)).syntax(),
            )
        })
        .collect();
    let index = jals_exec::block_on_inline(ProjectIndex::builder(&roots).with_stdlib().build());
    let analyses: Vec<FileAnalysis> = roots
        .iter()
        .map(|(_, root)| jals_exec::block_on_inline(FileAnalysis::of(root)))
        .collect();
    let semantics: Vec<FileSemantics<'_>> = roots
        .iter()
        .zip(&analyses)
        .map(|((file, _), analysis)| analysis.in_project(&index, *file))
        .collect();
    let typed: Vec<TypedFile<'_>> = semantics
        .iter()
        .map(|binding| jals_exec::block_on_inline(binding.typed()))
        .collect();
    CompileWasm::project(&typed, &[], &index, options).expect("the project compiles")
}

/// The positions option, which is what every compile here but one states.
fn positions() -> WasmOptions {
    WasmOptions {
        positions: true,
        ..WasmOptions::default()
    }
}

/// Run the module, with an instruction budget when one is given.
fn run(
    module: &[u8],
    invoke: Option<&str>,
    fuel: Option<u32>,
) -> Result<WasmRunOutcome, WasmRunError> {
    WasmRunner::run(&jals_build::WasmRunRequest {
        module,
        invoke,
        args: &[],
        natives: &jals_native::NativeBindings::new(),
        libraries: &[],
        foreign: &[],
        fuel,
        progress: &jals_progress::Progress::SILENT,
    })
}

/// A trap reports the statement it happened in, not the method or the file.
#[test]
fn a_trap_reports_the_statement_it_happened_at() {
    let project = r"
package app;

public class Main {
    public static int run() {
        int zero = 0;
        return 1 / zero;
    }
}
";
    let bytes = compile(&[project], positions());
    let error = run(&bytes, Some("run"), None).expect_err("dividing by zero traps");

    assert!(
        matches!(error, WasmRunError::Located { .. }),
        "the module carries positions, so the failure is placed: {error}"
    );
    let statement = "return 1 / zero;";
    let start = project
        .find(statement)
        .expect("the statement is in the source");
    assert_eq!(
        error.location(),
        Some((FileId(0), start..start + statement.len())),
        "the position is the statement the division was written in: {error}"
    );
}

/// A module compiled without positions fails as the bare trap: nothing to read, nothing to say.
#[test]
fn a_module_without_positions_reports_no_location() {
    let project = r"
package app;

public class Main {
    public static int run() {
        int zero = 0;
        return 1 / zero;
    }
}
";
    let bytes = compile(&[project], WasmOptions::default());
    let error = run(&bytes, Some("run"), None).expect_err("dividing by zero traps");

    assert!(
        matches!(error, WasmRunError::Trap(_)),
        "an ordinary compile has no global for the runner to read: {error}"
    );
    assert_eq!(error.location(), None);
}

/// A loop that never returns is stopped by the budget, and names the statement it was in.
///
/// The body is written without braces on purpose: a block body is a statement too, and a
/// checkpoint can fall between entering the block and entering its first statement, in which case
/// the block is the truthful answer. Unbraced, the body statement is the only write per iteration,
/// so the position is the one the loop was in and nothing else.
#[test]
fn a_run_over_its_budget_stops_and_names_the_statement() {
    let project = r"
package app;

public class Main {
    public static int run() {
        int n = 0;
        while (n >= 0)
            n = n + 1;
        return n;
    }
}
";
    let bytes = compile(&[project], positions());
    let error = run(&bytes, Some("run"), Some(10_000)).expect_err("the loop never returns");

    let statement = "n = n + 1;";
    let start = project
        .find(statement)
        .expect("the statement is in the source");
    assert_eq!(
        error.location(),
        Some((FileId(0), start..start + statement.len())),
        "the position is the statement the loop was in when the budget ran out: {error}"
    );
    assert!(
        error.to_string().contains("instruction budget"),
        "the failure is the budget, not the code: {error}"
    );
}

/// The same shape of program with a budget large enough to matter returns what it computed.
#[test]
fn a_run_inside_its_budget_returns_normally() {
    let project = r"
package app;

public class Main {
    public static int run() {
        int total = 0;
        for (int i = 0; i < 10; i = i + 1) {
            total = total + i;
        }
        return total;
    }
}
";
    let bytes = compile(&[project], positions());
    let outcome = run(&bytes, Some("run"), Some(1_000_000)).expect("the loop is tiny");
    assert_eq!(outcome, WasmRunOutcome::Returned(vec![WasmValue::I32(45)]));
}

/// The start function runs under the budget too: a `static` initialiser that never finishes is
/// stopped like any other code, even when no export was named.
#[test]
fn a_static_initialiser_is_inside_the_budget() {
    let project = r"
package app;

public class Main {
    static int spun = spin();

    static int spin() {
        int n = 0;
        while (n >= 0) {
            n = n + 1;
        }
        return n;
    }
}
";
    let bytes = compile(&[project], positions());
    let error = run(&bytes, None, Some(10_000)).expect_err("the initialiser never returns");

    assert!(
        matches!(error, WasmRunError::Located { .. }),
        "the loop is the module's own code, so the position is read for it too: {error}"
    );
    assert!(
        error.to_string().contains("instruction budget"),
        "the failure is the budget, not the code: {error}"
    );
}
