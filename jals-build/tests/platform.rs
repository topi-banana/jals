//! The platform package, compiled and run.
//!
//! Here rather than in `jals-platform` because this is the one crate holding both halves:
//! `jals-platform` has no compiler and no engine, and `jals-javac` does not know packages exist.
//!
//! # What this replaces
//!
//! A test that built two indexes — one with the package, one with the hand-written stubs it
//! shadowed — and diffed their member sets per fully-qualified name, to catch a member the package
//! forgot to declare. That invariant no longer exists to check: there is one text now, so there is
//! no second member set to lose one against.
//!
//! What is left is the claim that text actually carries its weight: it compiles, the tiers keep
//! `java.lang.Object` out of the module, and a program written against it runs.

use jals_build::{Assertions, BackendOptions, BackendRequest, BackendSelection, BackendSource};
use jals_config::BackendKind;
use jals_native::{PackageSelection, SourceKind};
use jals_platform::{CapturedHost, JavaBase};
use jals_progress::Progress;
use jals_storage::{CacheKey, CacheNamespace, ContentDigest, RelativePath};
use std::rc::Rc;

/// The platform over a host whose clock reads a fixed instant, so a test can assert an exact number.
pub(crate) fn platform(host: Rc<CapturedHost>) -> PackageSelection {
    PackageSelection::of([JavaBase::package(host)])
}

fn source(path: &str, text: &str) -> BackendSource {
    let bytes = text.as_bytes().to_vec();
    BackendSource {
        path: RelativePath::parse(path).expect("a valid path"),
        key: CacheKey::new(
            CacheNamespace::FrontendOutput,
            ContentDigest::of(b"platform-test"),
            ContentDigest::of(&bytes),
        ),
        bytes,
    }
}

/// Compile `sources` as a wasm project with the platform linked.
///
/// One pipeline, returning a `Result`, because the tests that expect a refusal have to be running
/// the same compile as the ones that expect a module.
pub(crate) fn compile(
    sources: &[(&str, &str)],
    packages: PackageSelection,
) -> Result<Vec<u8>, String> {
    let tree: Vec<BackendSource> = sources
        .iter()
        .map(|(path, text)| source(path, text))
        .collect();
    let options = BackendOptions::default();
    let request = BackendRequest {
        progress: &Progress::SILENT,
        tree: &tree,
        classpath: &[],
        options: &options,
    };
    // Through the selection, not by naming the backend: `BackendSelection` is the one place
    // `[build] backend` becomes an implementation, and a test that reached past it would be
    // exercising a construction no host performs.
    let BackendSelection::Available(backend) = BackendSelection::in_process(
        BackendKind::JalsWasm {},
        None,
        Assertions::Disabled,
        packages,
    ) else {
        panic!("the in-process wasm backend is always available");
    };
    let outcome = jals_exec::block_on_inline(backend.compile(&request))
        .map_err(|error| format!("{error:?}"))?;
    if !outcome.success() {
        return Err(outcome.messages.join("\n"));
    }
    Ok(outcome.artifacts.into_iter().next().expect("one module").1)
}

/// **It compiles.** Every implementation unit is lowered, not only the reachable ones — a library
/// input has every body compiled — so this is the whole 50-odd files going through the backend.
#[test]
fn the_platform_compiles_into_a_module() {
    let host = Rc::new(CapturedHost::new());
    let module = compile(
        &[(
            "Main.java",
            "public final class Main { public static int run() { return 1; } }",
        )],
        platform(host),
    )
    .expect("the platform compiles");
    assert!(
        module.starts_with(b"\0asm"),
        "the backend produced something that is not a module"
    );
}

/// **The tier keeps `java.lang.Object` out of the module**, and every other signature unit with it.
///
/// `Object` *is* the backend's `anyref`: it answers for that name before it consults its struct
/// table, so a declared `Object` would be one question with two answers — a field present on some
/// instances and not others. That used to be a rule stated in prose for a reviewer to enforce.
/// Here it is the type: `link_sources` cannot yield a signature unit, so there is no way to hand
/// the compile that file.
#[test]
fn no_signature_unit_reaches_the_compile() {
    let host = Rc::new(CapturedHost::new());
    let selection = platform(host);

    let linked: Vec<&str> = selection
        .link_sources()
        .map(|(_, s)| s.path.as_ref())
        .collect();
    assert!(
        !linked.contains(&"java/lang/Object.java"),
        "`Object` is the backend's own `anyref`; a compiled one would be a second answer"
    );
    assert!(
        !linked.contains(&"java/util/Map.java") && !linked.contains(&"java/util/Optional.java"),
        "the containers nobody implemented are declared, so nothing there can be lowered"
    );
    assert!(
        linked.contains(&"java/lang/String.java") && linked.contains(&"java/util/ArrayList.java"),
        "the checks above are vacuous unless something *is* linked"
    );

    // And the index still sees them: a program that names `Object` or `List` resolves.
    let indexed: Vec<&str> = selection
        .analysis_sources()
        .map(|(_, s)| s.path.as_ref())
        .collect();
    assert!(indexed.contains(&"java/lang/Object.java"));
    assert!(indexed.contains(&"java/util/List.java"));
    assert!(indexed.contains(&"java/util/Map.java"));
    assert_eq!(
        indexed.len() - linked.len(),
        selection
            .analysis_sources()
            .filter(|(_, s)| matches!(s.kind, SourceKind::Signatures))
            .count(),
        "every unit is in exactly one of the two answers"
    );
}

/// **It runs.** The renderings, the parses, the arithmetic, and the two streams staying apart,
/// asserted through a module the engine actually executed.
///
/// Gated on `wasm-run` because that feature is what links an engine. Every assertion here is about
/// Java the platform ships — `Integer.toString`'s negative accumulation, `Math.sqrt`'s Newton
/// passes, `String`'s hash — reached through a program that only calls it.
#[cfg(feature = "wasm-run")]
mod running {
    use super::{compile, platform};
    use jals_build::{WasmRunOutcome, WasmRunRequest, WasmRunner};
    use jals_platform::CapturedHost;
    use jals_progress::Progress;
    use std::rc::Rc;

    /// Compile `source` against the platform, run `export`, and hand back what the host captured.
    fn run(source: &str, export: &str) -> (String, String) {
        let host = Rc::new(CapturedHost::at(1_700_000_000_000));
        let packages = platform(Rc::clone(&host));
        let bindings = packages.bindings();
        let module = compile(&[("Main.java", source)], packages).expect("the platform compiles");
        let outcome = WasmRunner::run(&WasmRunRequest {
            module: &module,
            invoke: Some(export),
            args: &[],
            natives: &bindings,
            progress: &Progress::SILENT,
        })
        .expect("the module links and runs");
        assert!(matches!(outcome, WasmRunOutcome::Returned(_)));
        (host.take_out(), host.take_err())
    }

    /// Every import the module declares is bound.
    ///
    /// Asserted by the run happening at all: an engine refuses a module whose imports are unmet, so
    /// a missing binding fails at instantiation and never reaches the body.
    #[test]
    fn the_clock_reaches_the_host_the_package_was_built_over() {
        let (out, _) = run(
            "public final class Main {\n\
             public static void run() { System.out.println(System.currentTimeMillis()); }\n\
             }",
            "run",
        );
        assert_eq!(out, "1700000000000\n");
    }

    /// The two streams stay apart all the way to the host, which is why `Stream` is an enum.
    #[test]
    fn a_program_prints_through_the_streams_the_host_supplied() {
        let (out, err) = run(
            "public final class Main {\n\
             public static void run() {\n\
             System.out.println(Integer.MIN_VALUE);\n\
             System.out.println(true);\n\
             System.err.println(7);\n\
             }\n\
             }",
            "run",
        );
        assert_eq!(out, "-2147483648\ntrue\n");
        assert_eq!(err, "7\n");
    }

    /// `String` is a `char[]`, and everything on it is the package's own Java.
    #[test]
    fn strings_behave() {
        let (out, _) = run(
            "public final class Main {\n\
             private static final char[] TEXT = {'h', 'e', 'l', 'l', 'o'};\n\
             public static void run() {\n\
             String s = new String(TEXT);\n\
             System.out.println(s.length());\n\
             System.out.println(s.hashCode());\n\
             System.out.println(s.substring(1, 3));\n\
             System.out.println(s.indexOf('l'));\n\
             }\n\
             }",
            "run",
        );
        // `"hello".hashCode()` is 99162322 on any JVM; the loop that produces it is Java here.
        assert_eq!(out, "5\n99162322\nel\n2\n");
    }

    /// `equals` dispatches: `String.equals` has a body and wins over the identity fallback, while
    /// a reference comparison stays identity.
    #[test]
    fn equals_dispatches_to_an_override_and_falls_back_to_identity() {
        let (out, _) = run(
            "import java.util.ArrayList;\n\
             public final class Main {\n\
             private static final char[] AB = {'a', 'b'};\n\
             private static final char[] AB2 = {'a', 'b'};\n\
             public static void run() {\n\
             String x = new String(AB);\n\
             String y = new String(AB2);\n\
             System.out.println(x.equals(y));\n\
             System.out.println(x == y);\n\
             Object o = x;\n\
             System.out.println(o.equals(y));\n\
             System.out.println(o.equals(x));\n\
             }\n\
             }",
            "run",
        );
        assert_eq!(out, "true\nfalse\ntrue\ntrue\n");
    }

    /// The floating-point seam: a rendering the host produced, and a parse it read back.
    #[test]
    fn the_floating_point_seam_round_trips() {
        let (out, _) = run(
            "public final class Main {\n\
             private static final char[] TEXT = {'0', '.', '1'};\n\
             public static void run() {\n\
             System.out.println(Math.sqrt(2.0));\n\
             System.out.println(Double.doubleToRawLongBits(1.0));\n\
             System.out.println(Double.parseDouble(new String(TEXT)));\n\
             System.out.println(Math.max(0.0, -0.0));\n\
             }\n\
             }",
            "run",
        );
        assert_eq!(out, "1.4142135623730951\n4607182418800017408\n0.1\n0.0\n");
    }

    /// A surrogate pair is one character, so reversing the code units alone is not a reversal.
    ///
    /// Swapping raw `char`s leaves every pair inverted — a low surrogate ahead of its high one,
    /// which is not valid UTF-16 — and that matters more here than on a JVM: the host decodes what
    /// it is handed, so an inverted pair reaches a terminal as two replacement characters rather
    /// than as the character somebody wrote. Asserted as code units, because the failure is
    /// invisible in the decoded text.
    #[test]
    fn reversing_a_builder_reverses_characters_and_not_code_units() {
        let (out, _) = run(
            "public final class Main {\n\
             public static void run() {\n\
             StringBuilder b = new StringBuilder();\n\
             b.append('a').append((char) 0xD83D).append((char) 0xDE00).append('b');\n\
             String r = b.reverse().toString();\n\
             for (int i = 0; i < r.length(); i++) {\n\
             System.out.println(Integer.toHexString(r.charAt(i)));\n\
             }\n\
             }\n\
             }",
            "run",
        );
        // `b`, then the pair the *right* way round, then `a` — what a JDK 25 `reverse()` answers.
        assert_eq!(out, "62\nd83d\nde00\n61\n");
    }

    /// `indexOf(int, int)` takes a caller's number, and `Integer.MAX_VALUE` is a legal one.
    ///
    /// The supplementary path guarded its loop with `i + 1 < length`, which wraps to
    /// `Integer.MIN_VALUE` at the top of the range, passes, and indexes past the array — an
    /// `ArrayIndexOutOfBoundsException` where a JDK returns `-1`. The rows after it are the
    /// ordinary answers, so a guard that over-corrected would fail here too.
    #[test]
    fn an_oversized_start_index_is_answered_rather_than_thrown() {
        let (out, _) = run(
            "public final class Main {\n\
             private static final char[] PLAIN = {'h', 'e', 'l', 'l', 'o'};\n\
             private static final char[] PAIR = {'a', (char) 0xD83D, (char) 0xDE00, 'b'};\n\
             public static void run() {\n\
             String plain = new String(PLAIN);\n\
             String pair = new String(PAIR);\n\
             System.out.println(plain.indexOf(0x1F600, Integer.MAX_VALUE));\n\
             System.out.println(plain.indexOf('l', Integer.MAX_VALUE));\n\
             System.out.println(pair.indexOf(0x1F600, 0));\n\
             System.out.println(plain.indexOf('l', 0));\n\
             }\n\
             }",
            "run",
        );
        assert_eq!(out, "-1\n-1\n1\n2\n");
    }

    /// `parseDouble(null)` is a `NullPointerException`, and `parseInt(null)` is not.
    ///
    /// The JDK reaches `text.trim()` before it looks at anything, so the dereference is what fails
    /// at `double` and `float` width, while the integer parsers state the format refusal. Getting
    /// it wrong is silent: a `catch (NumberFormatException)` recovers here and propagates on a JVM.
    #[test]
    fn a_null_text_fails_the_way_each_parser_fails_on_a_jvm() {
        let (out, _) = run(
            "public final class Main {\n\
             private static final char[] NPE = {'N', 'P', 'E'};\n\
             private static final char[] NFE = {'N', 'F', 'E'};\n\
             private static void say(char[] which) { System.out.println(new String(which)); }\n\
             public static void run() {\n\
             try { Double.parseDouble(null); } catch (NullPointerException e) { say(NPE); }\n\
             catch (NumberFormatException e) { say(NFE); }\n\
             try { Float.parseFloat(null); } catch (NullPointerException e) { say(NPE); }\n\
             catch (NumberFormatException e) { say(NFE); }\n\
             try { Integer.parseInt(null); } catch (NullPointerException e) { say(NPE); }\n\
             catch (NumberFormatException e) { say(NFE); }\n\
             }\n\
             }",
            "run",
        );
        assert_eq!(out, "NPE\nNPE\nNFE\n");
    }

    /// A builder that outgrows its buffer keeps growing, rather than spinning.
    ///
    /// The doubling loop overflowed `int` before a `char[]` could reach `Integer.MAX_VALUE`, and an
    /// overflowed `grown` is negative — so it stayed below the target, the next doubling landed on
    /// zero, and `0 * 2` is zero for ever. Every runtime in this workspace is current-thread, so
    /// that wedges the process with no error to report. This does not reach the overflow (no test
    /// allocates two gigabytes) — it pins that the guard did not break ordinary growth, and the
    /// overflow arm is a one-line `grown = needed` on the same loop.
    #[test]
    fn a_builder_grows_past_its_initial_buffer() {
        let (out, _) = run(
            "public final class Main {\n\
             public static void run() {\n\
             StringBuilder b = new StringBuilder();\n\
             for (int i = 0; i < 5000; i++) { b.append('x'); }\n\
             System.out.println(b.length());\n\
             System.out.println(b.charAt(4999));\n\
             }\n\
             }",
            "run",
        );
        assert_eq!(out, "5000\nx\n");
    }

    /// An exception the package raises reaches the project's own `catch`.
    ///
    /// The whole hierarchy is the package's Java — `NumberFormatException` through
    /// `IllegalArgumentException` through `RuntimeException` — so this is a throw, a stack unwind
    /// and a type test all inside the module.
    #[test]
    fn an_exception_the_package_raises_reaches_the_projects_catch() {
        let (out, _) = run(
            "public final class Main {\n\
             private static final char[] BAD = {'1', '2', 'x'};\n\
             public static void run() {\n\
             try {\n\
             Integer.parseInt(new String(BAD));\n\
             } catch (NumberFormatException failure) {\n\
             System.out.println(failure.toString());\n\
             }\n\
             }\n\
             }",
            "run",
        );
        assert_eq!(out, "java.lang.NumberFormatException: 12x\n");
    }

    /// The same, for the one parse whose work happens **in Rust**.
    ///
    /// `Integer.parseInt` is ordinary Java and throws; `Double.parseDouble` reaches a binding, and
    /// a binding that refused would trap — which is not an exception at all. A trap stops the
    /// module, so the `catch` below would never run and the two spellings of "this is not a
    /// number" would answer differently. The verdict crosses the boundary as a value precisely so
    /// this test can exist.
    #[test]
    fn a_parse_that_fails_in_the_host_still_throws_into_the_projects_catch() {
        let (out, _) = run(
            "public final class Main {\n\
             private static final char[] BAD = {'1', '2', 'x'};\n\
             private static final char[] GOOD = {'2', '.', '5'};\n\
             public static void run() {\n\
             try {\n\
             Double.parseDouble(new String(BAD));\n\
             System.out.println(0.0);\n\
             } catch (NumberFormatException failure) {\n\
             System.out.println(failure.toString());\n\
             }\n\
             System.out.println(Double.parseDouble(new String(GOOD)));\n\
             }\n\
             }",
            "run",
        );
        // The second line is what says the module was still running: a trap would have taken the
        // whole run with it, and an empty `catch` would have printed `0.0` instead.
        assert_eq!(out, "java.lang.NumberFormatException: 12x\n2.5\n");
    }

    /// **A `Vec` in Rust, reached through Java.** The list's storage is a `Vec<HostValue>` in the
    /// host's table; its elements are Java references the host roots, so an object added in one
    /// native call comes back out of a later one as the same object.
    #[test]
    fn a_native_list_holds_references_across_calls() {
        let (out, _) = run(
            "import java.util.ArrayList;\n\
             public final class Main {\n\
             private static final char[] AB = {'a', 'b'};\n\
             private static final char[] AB2 = {'a', 'b'};\n\
             public static void run() {\n\
             ArrayList<String> list = new ArrayList<String>();\n\
             String x = new String(AB);\n\
             list.add(x);\n\
             list.add(new String(AB2));\n\
             System.out.println(list.size());\n\
             System.out.println(list.get(0) == x);\n\
             System.out.println(list.get(0).equals(list.get(1)));\n\
             System.out.println(list.contains(x));\n\
             System.out.println(list.indexOf(new String(AB2)));\n\
             System.out.println(list.remove(0) == x);\n\
             System.out.println(list.size());\n\
             }\n\
             }",
            "run",
        );
        assert_eq!(out, "2\ntrue\ntrue\ntrue\n0\ntrue\n1\n");
    }

    /// The iterator is ordinary Java holding the list it walks, and a `for` over it reaches the
    /// `Vec` one element at a time.
    #[test]
    fn a_native_list_iterates_and_reorders() {
        let (out, _) = run(
            "import java.util.ArrayList;\n\
             import java.util.Iterator;\n\
             public final class Main {\n\
             private static final char[] ONE = {'1'};\n\
             private static final char[] TWO = {'2'};\n\
             private static final char[] THREE = {'3'};\n\
             public static void run() {\n\
             ArrayList<String> list = new ArrayList<String>();\n\
             list.add(new String(ONE));\n\
             list.add(new String(TWO));\n\
             list.add(1, new String(THREE));\n\
             System.out.println(list.size());\n\
             System.out.println(list.get(1).charAt(0));\n\
             System.out.println(list.set(0, new String(THREE)).charAt(0));\n\
             Iterator<String> it = list.iterator();\n\
             while (it.hasNext()) {\n\
             System.out.println(it.next().charAt(0));\n\
             }\n\
             }\n\
             }",
            "run",
        );
        assert_eq!(out, "3\n3\n1\n3\n3\n2\n");
    }

    /// A list index that is not one throws the exception a JVM throws, which a program can catch —
    /// the check is in the Java half, so it is an exception rather than a host trap.
    #[test]
    fn a_list_index_out_of_bounds_throws_like_a_jvm() {
        let (out, _) = run(
            "import java.util.ArrayList;\n\
             public final class Main {\n\
             public static void run() {\n\
             ArrayList<String> list = new ArrayList<String>();\n\
             try {\n\
             list.get(0);\n\
             System.out.println(false);\n\
             } catch (IndexOutOfBoundsException caught) {\n\
             System.out.println(true);\n\
             }\n\
             }\n\
             }",
            "run",
        );
        assert_eq!(out, "true\n");
    }
}
