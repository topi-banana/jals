//! `emit` — compile one Java file and write the *bytes* out, for a reader who has a case name and
//! wants what the compiler produced from it.
//!
//! The corpus binaries say how far a case got (`jals-compile`, `jals-wasm`) and, with
//! `--list-gaps`, which file to open. This is the step after that: it hands the same file to the
//! same front end and writes the class files or the WebAssembly module, so `javap -c`,
//! `wasm-tools print`, and a real JVM or engine can be pointed at them.
//!
//! ```sh
//! cargo run --release -p jals-tests --example emit -- <File.java> <out-dir>
//! cargo run --release -p jals-tests --example emit -- --wasm <File.java> <out.wasm>
//! ```
//!
//! It resolves against the host JDK's `ct.sym`, which is what `jals-compile` does and what makes a
//! corpus case reproduce here; `--stdlib` uses the embedded stubs instead, which is what
//! `jals-javac`'s own tests resolve against. An example rather than a binary: it is a development
//! aid, so it stays out of the four the README's table is about.

use std::path::PathBuf;
use std::process::ExitCode;

use jals_hir::{FileAnalysis, FileId, FileSemantics, LoweredClasspath, ProjectIndex, TypedFile};
use jals_javac::lower::Compile;
use jals_javac::wasm::CompileWasm;
use jals_syntax::SyntaxNode;
use jals_tests::compile::Jdk;

/// Java 25, matching the class files the rest of the workspace pins its fixtures to.
const MAJOR_JAVA_25: u16 = 69;

fn main() -> ExitCode {
    let mut wasm = false;
    let mut stdlib = false;
    let mut positional = Vec::new();
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--wasm" => wasm = true,
            "--stdlib" => stdlib = true,
            _ => positional.push(argument),
        }
    }
    let [source, destination] = positional.as_slice() else {
        eprintln!("usage: emit [--wasm] [--stdlib] <File.java> <out-dir|out.wasm>");
        return ExitCode::FAILURE;
    };

    let classpath = if stdlib {
        None
    } else {
        match Jdk::detect().map(|jdk| jdk.classpath()) {
            Some(Ok((classpath, count))) => {
                eprintln!("classpath: {count} signatures from ct.sym");
                Some(classpath)
            }
            Some(Err(error)) => {
                eprintln!("error: {error}");
                return ExitCode::FAILURE;
            }
            None => {
                eprintln!("error: no JDK on this host; pass --stdlib to use the embedded stubs");
                return ExitCode::FAILURE;
            }
        }
    };

    let text = match std::fs::read_to_string(source) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("error: read {source}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let root = jals_exec::block_on_inline(jals_syntax::Parse::parse(&text)).syntax();
    let roots = [(FileId(0), root)];
    let index = build_index(&roots, classpath.as_ref());
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(&roots[0].1));
    let semantics: FileSemantics<'_> = analysis.in_project(&index, FileId(0));
    let typed: TypedFile<'_> = jals_exec::block_on_inline(semantics.typed());

    if wasm {
        match CompileWasm::project(&[typed], &index) {
            Ok(bytes) => {
                if let Err(error) = std::fs::write(destination, &bytes) {
                    eprintln!("error: write {destination}: {error}");
                    return ExitCode::FAILURE;
                }
                println!("{destination}");
            }
            Err(error) => {
                eprintln!("wasm error: {error}");
                return ExitCode::FAILURE;
            }
        }
        return ExitCode::SUCCESS;
    }

    match Compile::file(typed, MAJOR_JAVA_25) {
        Ok(classes) => {
            for class in &classes {
                let path =
                    PathBuf::from(destination).join(format!("{}.class", class.internal_name));
                if let Some(parent) = path.parent()
                    && let Err(error) = std::fs::create_dir_all(parent)
                {
                    eprintln!("error: create {}: {error}", parent.display());
                    return ExitCode::FAILURE;
                }
                if let Err(error) = std::fs::write(&path, &class.bytes) {
                    eprintln!("error: write {}: {error}", path.display());
                    return ExitCode::FAILURE;
                }
                println!("{}", path.display());
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("lower error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The index the file is bound against: the host JDK's own signatures, or the embedded stubs.
fn build_index(
    roots: &[(FileId, SyntaxNode)],
    classpath: Option<&LoweredClasspath>,
) -> ProjectIndex {
    let builder = ProjectIndex::builder(roots);
    jals_exec::block_on_inline(match classpath {
        Some(classpath) => builder.with_classpath(classpath).build(),
        None => builder.with_stdlib().build(),
    })
}
