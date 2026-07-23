extern crate std;

use alloc::borrow::ToOwned;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use jals_exec::block_on_inline;
use jals_storage::{ArtifactCache, MemoryCache, RelativePath};

use crate::dialect::{DialectFlags, DialectFrontend};
use crate::driver::Driver;
use crate::frontend::Frontend;
use crate::ir::{FrontendDiagnostic, Ir, IrFile, LoweredFile, LoweredTree};
use crate::key::FrontendKey;
use crate::vanilla::VanillaFrontend;

/// Fixture namespace for these tests.
struct Fixture;

impl Fixture {
    fn file(path: &str, bytes: &[u8]) -> IrFile {
        IrFile::new(RelativePath::parse(path).unwrap(), Arc::from(bytes))
    }

    /// Two sources, in canonical order.
    fn sources() -> Vec<IrFile> {
        let mut files = vec![
            Self::file("src/main/java/Main.java", b"class Main {}\n"),
            Self::file("src/main/java/Util.java", b"class Util {}\n"),
        ];
        FrontendKey::canonical_order(&mut files);
        files
    }
}

#[test]
fn vanilla_emits_every_input_unchanged() {
    let files = Fixture::sources();
    let mut cache = ArtifactCache::new(MemoryCache::default());
    let lowered = block_on_inline(Driver::lower(&VanillaFrontend, &mut cache, &files)).unwrap();

    assert_eq!(lowered.tree.files().len(), files.len());
    for (input, output) in files.iter().zip(lowered.tree.files()) {
        assert_eq!(input.path, output.path);
        let bytes = block_on_inline(cache.lookup(&output.key)).unwrap().unwrap();
        assert_eq!(bytes.as_slice(), &input.bytes[..]);
    }
}

/// Pins the provenance fold to a literal.
///
/// Every other test here compares the fold against code that would change alongside it. This
/// one compares against a constant, so a reordered or dropped field turns into a red test
/// rather than a silent invalidation that only surfaces as mysteriously cold rebuilds.
#[test]
fn frontend_out_key_is_stable() {
    let files = Fixture::sources();
    let mut cache = ArtifactCache::new(MemoryCache::default());
    let lowered = block_on_inline(Driver::lower(&VanillaFrontend, &mut cache, &files)).unwrap();

    let keys: Vec<_> = lowered
        .tree
        .files()
        .iter()
        .map(|file| file.key.provenance().to_hex())
        .collect();

    assert_eq!(
        keys,
        vec![
            FROZEN_MAIN_PROVENANCE.to_owned(),
            FROZEN_UTIL_PROVENANCE.to_owned(),
        ],
        "the provenance fold changed; if that was deliberate, update these literals and \
         understand that it makes every cached frontend output in the wild unreachable"
    );
}

#[test]
fn tree_digest_is_independent_of_construction_order() {
    let files = Fixture::sources();
    let mut cache = ArtifactCache::new(MemoryCache::default());
    let lowered = block_on_inline(Driver::lower(&VanillaFrontend, &mut cache, &files)).unwrap();

    let mut reversed: Vec<LoweredFile> = lowered.tree.files().to_vec();
    reversed.reverse();
    let rebuilt = LoweredTree::new(reversed).unwrap();

    assert_eq!(lowered.tree.digest(), rebuilt.digest());
    assert_eq!(lowered.tree, rebuilt);
}

/// Discovery walks the filesystem, whose order is neither sorted nor stable across platforms.
/// A project-scoped digest that inherited that order would make a cache entry built on one
/// machine miss on another.
#[test]
fn project_digest_is_independent_of_discovery_order() {
    let ordered = Fixture::sources();
    let mut shuffled = ordered.clone();
    shuffled.reverse();
    FrontendKey::canonical_order(&mut shuffled);

    assert_eq!(
        FrontendKey::project(&ordered),
        FrontendKey::project(&shuffled)
    );
}

#[test]
fn tree_manifest_round_trips() {
    let files = Fixture::sources();
    let mut cache = ArtifactCache::new(MemoryCache::default());
    let lowered = block_on_inline(Driver::lower(&VanillaFrontend, &mut cache, &files)).unwrap();

    let encoded = lowered.tree.encode();
    assert_eq!(LoweredTree::decode(&encoded).unwrap(), lowered.tree);
}

#[test]
fn a_damaged_manifest_is_rejected_rather_than_misread() {
    let files = Fixture::sources();
    let mut cache = ArtifactCache::new(MemoryCache::default());
    let lowered = block_on_inline(Driver::lower(&VanillaFrontend, &mut cache, &files)).unwrap();

    let encoded = lowered.tree.encode();
    for cut in 0..encoded.len() {
        assert!(LoweredTree::decode(&encoded[..cut]).is_err());
    }
    let mut trailing = encoded;
    trailing.push(0);
    assert!(LoweredTree::decode(&trailing).is_err());
}

/// A warm rebuild must restore the lowering from its manifest without running the frontend.
/// Asserted with a frontend that fails if invoked twice, so a regression to "always re-run"
/// is loud rather than merely slow.
#[test]
fn second_lowering_restores_from_cache_without_running_the_frontend() {
    use core::cell::Cell;

    struct RunsOnce {
        inner: VanillaFrontend,
        runs: Cell<u32>,
    }

    impl crate::frontend::Frontend for RunsOnce {
        fn caps(&self) -> crate::frontend::FrontendCaps {
            self.inner.caps()
        }
        fn config_digest(&self) -> jals_storage::ContentDigest {
            self.inner.config_digest()
        }
        fn run<'a>(&'a self, ir: crate::ir::Ir<'a>) -> crate::frontend::FrontendFuture<'a> {
            assert_eq!(self.runs.get(), 0, "frontend ran against a warm cache");
            self.runs.set(self.runs.get() + 1);
            self.inner.run(ir)
        }
        fn describe(&self, ir: &crate::ir::Ir<'_>) -> alloc::string::String {
            self.inner.describe(ir)
        }
    }

    let files = Fixture::sources();
    let mut cache = ArtifactCache::new(MemoryCache::default());
    let frontend = RunsOnce {
        inner: VanillaFrontend,
        runs: Cell::new(0),
    };

    let cold = block_on_inline(Driver::lower(&frontend, &mut cache, &files)).unwrap();
    assert!(!cold.cached);

    let warm = block_on_inline(Driver::lower(&frontend, &mut cache, &files)).unwrap();
    assert!(warm.cached);
    assert_eq!(cold.tree, warm.tree);
}

/// Editing one file must change that file's key and leave its sibling's untouched — the
/// per-file invalidation a frontend earns by declaring `IrLevel::Bytes`.
#[test]
fn editing_one_file_leaves_the_other_key_unchanged() {
    let before = Fixture::sources();
    let mut after = before.clone();
    after[0] = Fixture::file("src/main/java/Main.java", b"class Main { int x; }\n");

    let mut cache = ArtifactCache::new(MemoryCache::default());
    let first = block_on_inline(Driver::lower(&VanillaFrontend, &mut cache, &before)).unwrap();
    let second = block_on_inline(Driver::lower(&VanillaFrontend, &mut cache, &after)).unwrap();

    let changed = &first.tree.files()[0];
    let changed_after = &second.tree.files()[0];
    assert_eq!(changed.path, changed_after.path);
    assert_ne!(changed.key, changed_after.key);

    let untouched = &first.tree.files()[1];
    let untouched_after = &second.tree.files()[1];
    assert_eq!(untouched.path, untouched_after.path);
    assert_eq!(
        untouched.key, untouched_after.key,
        "a Bytes-level frontend must not be invalidated by a sibling edit"
    );
}

// ===== Dialect frontend: grouped-import desugaring =====

/// Test helpers, grouped in a module (like the parser tests) so they are not free functions.
mod helpers {
    use super::*;

    /// Run the dialect frontend (grouped imports on) over one source and return the emitted bytes.
    pub(super) fn desugar(src: &str) -> Vec<u8> {
        let files = vec![Fixture::file("src/main/java/Main.java", src.as_bytes())];
        let frontend = DialectFrontend::new(DialectFlags {
            grouped_imports: true,
            ..DialectFlags::default()
        });
        let output = block_on_inline(frontend.run(Ir::Bytes { files: &files })).unwrap();
        assert!(
            !output.has_errors(),
            "unexpected desugar error: {:?}",
            output.diagnostics
        );
        assert_eq!(output.files.len(), 1);
        output.files.into_iter().next().unwrap().1
    }

    /// Run the dialect frontend and return the emitted source as a string.
    pub(super) fn desugar_str(src: &str) -> alloc::string::String {
        alloc::string::String::from_utf8(desugar(src)).unwrap()
    }

    /// Run the dialect frontend over a source expected to fail, returning its diagnostics and the
    /// bytes it emitted.
    pub(super) fn desugar_failing(src: &str) -> (Vec<FrontendDiagnostic>, Vec<u8>) {
        let files = vec![Fixture::file("src/main/java/Main.java", src.as_bytes())];
        let frontend = DialectFrontend::new(DialectFlags {
            grouped_imports: true,
            ..DialectFlags::default()
        });
        let output = block_on_inline(frontend.run(Ir::Bytes { files: &files })).unwrap();
        assert!(output.has_errors());
        assert_eq!(output.files.len(), 1);
        let bytes = output.files.into_iter().next().unwrap().1;
        (output.diagnostics, bytes)
    }

    pub(super) fn newlines(text: &str) -> usize {
        text.matches('\n').count()
    }

    /// 1-based line of the first occurrence of `needle` in `text`.
    pub(super) fn line_of(text: &str, needle: &str) -> usize {
        let offset = text.find(needle).expect("needle present");
        1 + text[..offset].matches('\n').count()
    }
}

use helpers::{desugar_failing, desugar_str, line_of, newlines};

#[test]
fn desugars_single_line_group_onto_one_line() {
    // The headline case: 0 newlines in, 0 newlines out, one physical line.
    let out = desugar_str("import java.util.{HashMap, ArrayList};\n");
    assert_eq!(
        out,
        "import java.util.HashMap; import java.util.ArrayList;\n"
    );
}

#[test]
fn single_member_and_trailing_comma_produce_one_statement() {
    assert_eq!(desugar_str("import a.{B};\n"), "import a.B;\n");
    assert_eq!(desugar_str("import a.{B,};\n"), "import a.B;\n");
}

#[test]
fn nested_and_wildcard_members_concatenate_correctly() {
    let out = desugar_str("import java.util.{HashMap, regex.Pattern, concurrent.*};\n");
    assert_eq!(
        out,
        "import java.util.HashMap; import java.util.regex.Pattern; \
         import java.util.concurrent.*;\n"
    );
}

#[test]
fn static_group_puts_static_on_every_member() {
    let out = desugar_str("import static java.lang.Math.{PI, E};\n");
    assert_eq!(
        out,
        "import static java.lang.Math.PI; import static java.lang.Math.E;\n"
    );
}

#[test]
fn empty_group_expands_to_nothing_but_keeps_the_line() {
    let src = "package p;\nimport a.{};\nclass C {}\n";
    let out = desugar_str(src);
    // The import line collapses to blank, but `class C {}` stays on line 3.
    assert_eq!(newlines(&out), newlines(src));
    assert_eq!(line_of(&out, "class C {}"), 3);
}

#[test]
fn multiline_group_reproduces_the_original_newline_count() {
    let src = "import java.util.{\n    HashMap,\n    ArrayList\n};\nclass C {}\n";
    let out = desugar_str(src);
    // The significant span held 3 newlines; the replacement must too, so `class C {}` — which was
    // on line 5 — is still on line 5.
    assert_eq!(newlines(&out), newlines(src));
    assert_eq!(line_of(src, "class C {}"), 5);
    assert_eq!(line_of(&out, "class C {}"), 5);
    assert!(out.contains("import java.util.HashMap;"));
    assert!(out.contains("import java.util.ArrayList;"));
}

#[test]
fn body_lines_keep_their_numbers_after_expansion() {
    // The core requirement: expansion never shifts following lines (Java stack-trace fidelity).
    let src = "package p;\n\
               import java.util.{HashMap, ArrayList};\n\
               public class Foo {\n\
               \x20\x20\x20\x20void m() { throw new RuntimeException(\"x\"); }\n\
               }\n";
    let out = desugar_str(src);
    assert!(out.contains("import java.util.HashMap;"));
    assert!(out.contains("import java.util.ArrayList;"));
    assert_eq!(newlines(&out), newlines(src));
    assert_eq!(line_of(&out, "public class Foo"), 3);
    assert_eq!(line_of(&out, "throw new RuntimeException"), 4);
}

#[test]
fn ungrouped_and_grouped_imports_mix_without_touching_the_plain_one() {
    let src = "import java.io.IOException;\nimport java.util.{List, Map};\n";
    let out = desugar_str(src);
    // The plain import's bytes are untouched.
    assert!(out.starts_with("import java.io.IOException;\n"));
    assert!(out.contains("import java.util.List; import java.util.Map;"));
}

#[test]
fn source_without_groups_is_emitted_unchanged() {
    let src = "import java.util.List;\nclass C {}\n";
    assert_eq!(desugar_str(src), src);
}

#[test]
fn malformed_group_is_emitted_verbatim_with_an_error() {
    let src = "import java.util.{HashMap, ArrayList;\nclass C {}\n";
    let (_diagnostics, bytes) = desugar_failing(src);
    // The output keeps one entry per input even though the driver will reject the lowering, so
    // `FrontendOutput` never describes a half-emitted tree.
    assert_eq!(bytes, src.as_bytes());
}

#[test]
fn malformed_group_diagnostic_names_the_line_and_the_parse_error() {
    // The lowering is rejected, so `javac` never runs and nothing downstream restates the error:
    // the diagnostic is the only report the user gets and must locate the group on its own.
    let src = "package p;\n\nimport java.util.{HashMap, ArrayList;\nclass C {}\n";
    let (diagnostics, _bytes) = desugar_failing(src);
    let message = &diagnostics[0].message;
    assert!(
        message.contains("on line 3"),
        "diagnostic should name the group's line: {message}"
    );
    assert!(
        message.contains("RBRACE"),
        "diagnostic should carry the parser's own message: {message}"
    );
}

#[test]
fn group_without_a_semicolon_is_diagnosed_at_its_own_line() {
    // No `;` at all: there is no span to replace, so the fallback anchor is the `import` keyword.
    let src = "package p;\nimport a.{B}\nclass C {}\n";
    let (diagnostics, bytes) = desugar_failing(src);
    assert_eq!(bytes, src.as_bytes());
    let message = &diagnostics[0].message;
    assert!(
        message.contains("on line 2") && message.contains("missing `;`"),
        "unexpected diagnostic: {message}"
    );
}

#[test]
fn config_digest_distinguishes_enabled_flags() {
    let on = DialectFrontend::new(DialectFlags {
        grouped_imports: true,
        ..DialectFlags::default()
    });
    let off = DialectFrontend::new(DialectFlags {
        grouped_imports: false,
        ..DialectFlags::default()
    });
    assert_ne!(on.config_digest(), off.config_digest());
    assert!(on.caps().type_stable);
}

// Frozen provenance digests for `sources()` under the vanilla frontend. Deliberately literals
// and not recomputed: recomputing them here would make the test tautological, which is exactly
// the failure mode it exists to prevent.
const FROZEN_MAIN_PROVENANCE: &str =
    "29b0dd5f77ef9e58f4574247179eade0c6d89d19a376ac61bc4ab126fa842ee8";
const FROZEN_UTIL_PROVENANCE: &str =
    "70b703a32c04a480bde53777192b0cf9d11048625ff104ef450f6b6beac094e4";

// ===== Dialect frontend: attributes and `cfg` conditional compilation =====

/// Attribute-test helpers, mirroring the grouped-import ones.
mod attr_helpers {
    use super::*;

    pub(super) fn attr_flags(features: &[&str]) -> DialectFlags {
        DialectFlags {
            attributes: true,
            build_features: features.iter().map(|f| (*f).to_owned()).collect(),
            ..DialectFlags::default()
        }
    }

    /// Run the dialect frontend (attributes on, grouped imports off) over one source with the
    /// given enabled build features and return the emitted source.
    pub(super) fn strip(src: &str, features: &[&str]) -> alloc::string::String {
        let files = vec![Fixture::file("src/main/java/Main.java", src.as_bytes())];
        let frontend = DialectFrontend::new(attr_flags(features));
        let output = block_on_inline(frontend.run(Ir::Bytes { files: &files })).unwrap();
        assert!(
            !output.has_errors(),
            "unexpected attribute error: {:?}",
            output.diagnostics
        );
        assert_eq!(output.files.len(), 1);
        alloc::string::String::from_utf8(output.files.into_iter().next().unwrap().1).unwrap()
    }

    /// Run the dialect frontend over a source expected to fail, returning the error messages and
    /// the emitted bytes.
    pub(super) fn strip_failing(
        src: &str,
        features: &[&str],
    ) -> (Vec<alloc::string::String>, Vec<u8>) {
        let files = vec![Fixture::file("src/main/java/Main.java", src.as_bytes())];
        let frontend = DialectFrontend::new(attr_flags(features));
        let output = block_on_inline(frontend.run(Ir::Bytes { files: &files })).unwrap();
        assert!(
            output.has_errors(),
            "expected an error: {:?}",
            output.diagnostics
        );
        assert_eq!(output.files.len(), 1);
        let bytes = output.files.into_iter().next().unwrap().1;
        let messages = output
            .diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect();
        (messages, bytes)
    }
}

use attr_helpers::{attr_flags, strip, strip_failing};

#[test]
fn enabled_attribute_is_blanked_in_place() {
    // Length-preserving: the attribute's bytes become spaces, everything else is verbatim.
    let src = "#[cfg(feature = \"x\")]\nclass C {}\n";
    let out = strip(src, &["x"]);
    assert_eq!(out, "                     \nclass C {}\n");
    assert_eq!(out.len(), src.len());
}

#[test]
fn false_cfg_blanks_the_whole_member_and_keeps_line_numbers() {
    let src = "class C {\n    #[cfg(feature = \"x\")]\n    void gone() {\n        f();\n    }\n    void kept() {}\n}\n";
    let out = strip(src, &[]);
    assert_eq!(out.len(), src.len());
    assert_eq!(newlines(&out), newlines(src));
    assert!(!out.contains("gone"));
    assert!(!out.contains("f();"));
    // `kept` was on line 6 and still is.
    assert_eq!(line_of(src, "void kept"), 6);
    assert_eq!(line_of(&out, "void kept"), 6);
}

#[test]
fn true_cfg_keeps_the_member_and_strips_only_the_attribute() {
    let src = "class C {\n    #[cfg(feature = \"x\")]\n    void kept() {}\n}\n";
    let out = strip(src, &["x"]);
    assert!(out.contains("void kept() {}"));
    assert!(!out.contains("#["));
    assert_eq!(line_of(&out, "void kept"), 3);
}

#[test]
fn predicate_combinators_evaluate_like_rust_cfg() {
    let src =
        |pred: &str| alloc::format!("class C {{\n    #[cfg({pred})]\n    void m() {{}}\n}}\n");
    let on = |pred: &str, features: &[&str]| strip(&src(pred), features).contains("void m");
    assert!(on("all(feature = \"a\", feature = \"b\")", &["a", "b"]));
    assert!(!on("all(feature = \"a\", feature = \"b\")", &["a"]));
    assert!(on("all()", &[]));
    assert!(on("any(feature = \"a\", feature = \"b\")", &["b"]));
    assert!(!on("any()", &["a"]));
    assert!(on("not(feature = \"a\")", &[]));
    assert!(!on("not(feature = \"a\")", &["a"]));
    assert!(on(
        "any(all(feature = \"a\"), not(feature = \"b\"))",
        &["c"]
    ));
}

#[test]
fn unknown_feature_name_is_simply_false() {
    // Features are additive (Cargo semantics): testing an undeclared name is not an error.
    let src = "class C {\n    #[cfg(feature = \"nope\")]\n    void m() {}\n}\n";
    let out = strip(src, &["other"]);
    assert!(!out.contains("void m"));
}

#[test]
fn structural_errors_fail_the_file_verbatim() {
    for (src, needle) in [
        ("#[foo]\nclass C {}\n", "unknown attribute `foo` on line 1"),
        (
            "#[cfg]\nclass C {}\n",
            "malformed `cfg` attribute on line 1",
        ),
        (
            "#[cfg(feature = \"a\", feature = \"b\")]\nclass C {}\n",
            "malformed `cfg` attribute on line 1",
        ),
        (
            "#[cfg(wibble = \"a\")]\nclass C {}\n",
            "malformed `cfg` attribute on line 1",
        ),
        (
            "class C {\n    #[cfg(feature = 1)]\n    void m() {}\n}\n",
            "feature name on line 2",
        ),
        (
            "#[cfg(feature = \"a\\nb\")]\nclass C {}\n",
            "must be a plain string literal",
        ),
        (
            "class C { void m(#[cfg(feature = \"x\")] int p) {} }\n",
            "not supported on this construct",
        ),
        (
            "class C { public #[cfg(feature = \"x\")] void m() {} }\n",
            "must come before modifiers",
        ),
        // A for-init attribute never even becomes an ATTRIBUTE node (the header's error
        // recovery shreds it), so the stray-`#` net catches it.
        (
            "class C { void m() { for (#[cfg(feature = \"x\")] int i = 0;;) {} } }\n",
            "does not begin an attribute",
        ),
        (
            "class C { void m() { a # b; } }\n",
            "does not begin an attribute",
        ),
    ] {
        let (messages, bytes) = strip_failing(src, &["x", "a", "b"]);
        assert_eq!(
            bytes,
            src.as_bytes(),
            "failing file must emit verbatim: {src:?}"
        );
        assert!(
            messages.iter().any(|m| m.contains(needle)),
            "expected {needle:?} in {messages:?} for {src:?}"
        );
    }
}

#[test]
fn attributes_inside_a_disabled_host_are_not_validated() {
    // `#[wat]` would be an error, but its enclosing class is cfg'd out (Rust parity).
    let src = "#[cfg(feature = \"x\")]\nclass Gone {\n    #[wat]\n    void m() {}\n}\n";
    let out = strip(src, &[]);
    assert!(!out.contains("wat"));
    assert!(!out.contains("Gone"));
}

#[test]
fn any_false_attribute_among_several_disables_the_host() {
    let src =
        "class C {\n    #[cfg(feature = \"a\")]\n    #[cfg(feature = \"b\")]\n    void m() {}\n}\n";
    assert!(!strip(src, &["a"]).contains("void m"));
    assert!(strip(src, &["a", "b"]).contains("void m"));
}

#[test]
fn cfg_on_imports_and_statements() {
    let src = "#[cfg(feature = \"x\")] import a.B;\nclass C {\n    void m() {\n        #[cfg(feature = \"x\")] f();\n        g();\n    }\n}\n";
    let with = strip(src, &["x"]);
    assert!(with.contains("import a.B;"));
    assert!(with.contains("f();"));
    let without = strip(src, &[]);
    assert!(!without.contains("import a.B;"));
    assert!(!without.contains("f()"));
    assert!(without.contains("g();"));
    assert_eq!(line_of(&without, "g();"), 5);
}

#[test]
fn stripped_sole_body_statement_leaves_a_semicolon() {
    let src = "class C {\n    void m() {\n        if (c) #[cfg(feature = \"x\")] f();\n        while (c) #[cfg(feature = \"x\")] g();\n    }\n}\n";
    let out = strip(src, &[]);
    assert!(out.contains("if (c) ;"));
    assert!(out.contains("while (c) ;"));
    assert_eq!(out.len(), src.len());
}

#[test]
fn cfg_false_grouped_import_is_blanked_not_expanded() {
    let src = "#[cfg(feature = \"x\")] import java.util.{HashMap, ArrayList};\nclass C {}\n";
    let frontend = DialectFrontend::new(DialectFlags {
        grouped_imports: true,
        ..attr_flags(&[])
    });
    let files = vec![Fixture::file("src/main/java/Main.java", src.as_bytes())];
    let output = block_on_inline(frontend.run(Ir::Bytes { files: &files })).unwrap();
    assert!(!output.has_errors(), "{:?}", output.diagnostics);
    let out = alloc::string::String::from_utf8(output.files.into_iter().next().unwrap().1).unwrap();
    assert!(!out.contains("import"));
    assert_eq!(out.len(), src.len());
    assert_eq!(line_of(&out, "class C {}"), 2);
}

#[test]
fn grouped_import_and_attribute_rewrite_in_one_pass() {
    let src = "#[cfg(feature = \"x\")] import java.util.{HashMap, ArrayList};\nclass C {}\n";
    let frontend = DialectFrontend::new(DialectFlags {
        grouped_imports: true,
        ..attr_flags(&["x"])
    });
    let files = vec![Fixture::file("src/main/java/Main.java", src.as_bytes())];
    let output = block_on_inline(frontend.run(Ir::Bytes { files: &files })).unwrap();
    assert!(!output.has_errors(), "{:?}", output.diagnostics);
    let out = alloc::string::String::from_utf8(output.files.into_iter().next().unwrap().1).unwrap();
    // The attribute is blanked and the group is expanded, in the same output.
    assert!(!out.contains("#["));
    assert!(out.contains("import java.util.HashMap; import java.util.ArrayList;"));
    assert_eq!(line_of(&out, "class C {}"), 2);
}

#[test]
fn cfg_false_on_the_only_type_leaves_a_blank_unit() {
    let src = "package p;\n#[cfg(feature = \"x\")]\nclass Only {}\n";
    let out = strip(src, &[]);
    assert!(out.starts_with("package p;\n"));
    assert!(!out.contains("Only"));
    assert_eq!(newlines(&out), newlines(src));
    assert_eq!(out.len(), src.len());
}

#[test]
fn attribute_config_digest_tracks_features_canonically() {
    let digest = |flags: DialectFlags| DialectFrontend::new(flags).config_digest();
    // Same set, same digest (BTreeSet canonicalizes insertion order).
    assert_eq!(
        digest(attr_flags(&["a", "b"])),
        digest(attr_flags(&["b", "a"]))
    );
    // Different sets and different flags differ.
    assert_ne!(digest(attr_flags(&["a"])), digest(attr_flags(&["b"])));
    assert_ne!(digest(attr_flags(&[])), digest(DialectFlags::default()));
    // Two names must not collide across the separator.
    assert_ne!(digest(attr_flags(&["ab"])), digest(attr_flags(&["a", "b"])));
    // Attributes flip type stability (a false cfg can remove whole types).
    assert!(!DialectFrontend::new(attr_flags(&[])).caps().type_stable);
}

#[test]
fn attributes_off_passes_attribute_syntax_through() {
    // With only grouped imports on, attribute syntax is not this frontend's concern (the lint
    // gate reports it; javac would reject it) — bytes pass through untouched.
    let src = "#[cfg(feature = \"x\")]\nclass C {}\n";
    let frontend = DialectFrontend::new(DialectFlags {
        grouped_imports: true,
        ..DialectFlags::default()
    });
    let files = vec![Fixture::file("src/main/java/Main.java", src.as_bytes())];
    let output = block_on_inline(frontend.run(Ir::Bytes { files: &files })).unwrap();
    assert!(!output.has_errors());
    assert_eq!(output.files[0].1, src.as_bytes());
}
