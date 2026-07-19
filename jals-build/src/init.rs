//! Project scaffolding for `jals init`.
//!
//! [`InitOptions::scaffold`] turns an [`InitOptions`] into the files a fresh JALS/Java project needs — a
//! `jals.toml` manifest, a starter `Main.java`, and a `.gitignore` — as pure [`ScaffoldFile`] data.
//! Like the rest of this crate it touches neither the filesystem nor a process: `jals-cli` decides
//! where to create the project and writes the files. Keeping the logic pure makes it deterministic,
//! unit-testable, and `wasm32`-compatible.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use jals_config::Build;
use jals_storage::FileKey;

/// The simple name of the starter class, and the `[run] main-class` that runs it. The starter lives
/// in the default (unnamed) package, so the simple name is also the fully-qualified name.
const MAIN_CLASS: &str = "Main";

/// Inputs for scaffolding a new project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitOptions {
    /// Project name, written to `[package] name`. Typically the target directory's name.
    pub name: String,
}

/// A single file to create when scaffolding, as pure data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaffoldFile {
    /// Path relative to the project root.
    pub path: FileKey,
    /// The file's full contents.
    pub contents: String,
}

impl InitOptions {
    /// Build the files for a new project: a `jals.toml` manifest, a starter `Main.java` under the
    /// default source directory, and a `.gitignore` that ignores the build output.
    ///
    /// The result is ordered manifest-first; `jals-cli` writes each file, creating parent directories
    /// as needed. The scaffold follows [`Build::default`]'s Maven-style layout, so the manifest leaves
    /// `[build]` unset and `Main.java` is placed under the default source root.
    pub fn scaffold(&self) -> Vec<ScaffoldFile> {
        let build = Build::default();
        let source_dir = build
            .source_dirs
            .first()
            .cloned()
            .unwrap_or_else(|| "src/main/java".to_owned());

        vec![
            ScaffoldFile {
                path: FileKey::parse("jals.toml").expect("scaffold path is valid"),
                contents: ScaffoldFile::manifest_template(&self.name),
            },
            ScaffoldFile {
                path: FileKey::parse(&format!("{source_dir}/{MAIN_CLASS}.java"))
                    .expect("default source path is valid"),
                contents: ScaffoldFile::main_java(),
            },
            ScaffoldFile {
                path: FileKey::parse(".gitignore").expect("scaffold path is valid"),
                contents: ScaffoldFile::gitignore(&build.classes_dir),
            },
        ]
    }
}

impl ScaffoldFile {
    /// Render the `jals.toml` template, with `[build]` left commented out so the project uses the
    /// defaults until the user wants to customize it.
    fn manifest_template(name: &str) -> String {
        // Render `s` as a TOML basic string (quoted, with the characters TOML requires escaped), so
        // an unusual project name can never produce a malformed manifest.
        fn toml_string(s: &str) -> String {
            let mut out = String::with_capacity(s.len() + 2);
            out.push('"');
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }

        format!(
            "[package]\n\
             name = {name}\n\
             version = \"0.1.0\"\n\
             \n\
             # Optional compilation settings. Uncomment [build] and the keys you need.\n\
             # [build]\n\
             # source-dirs = [\"src/main/java\"]\n\
             # classes-dir = \"target/classes\"\n\
             # release = 21\n\
             # script = {{ type = \"rhai\", file = \"build.rhai\" }}\n\
             \n\
             [run]\n\
             main-class = \"{MAIN_CLASS}\"\n",
            name = toml_string(name),
        )
    }

    /// The starter source file: a hello-world `Main` in the default package.
    fn main_java() -> String {
        format!(
            "public class {MAIN_CLASS} {{\n\
            \x20   public static void main(String[] args) {{\n\
            \x20       System.out.println(\"Hello, world!\");\n\
            \x20   }}\n\
             }}\n"
        )
    }

    /// Ignore the build output. The entry is the first path component of the manifest's
    /// `classes-dir` (e.g. `target` of `target/classes`), so a custom default still produces a
    /// sensible `.gitignore`.
    fn gitignore(classes_dir: &str) -> String {
        let root = classes_dir.split('/').next().unwrap_or(classes_dir);
        format!("/{root}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jals_config::Manifest;

    fn find<'a>(files: &'a [ScaffoldFile], path: &str) -> &'a ScaffoldFile {
        files
            .iter()
            .find(|f| f.path == FileKey::parse(path).unwrap())
            .unwrap_or_else(|| panic!("scaffold is missing {path}"))
    }

    #[test]
    fn scaffolds_manifest_source_and_gitignore() {
        let files = (InitOptions {
            name: "demo".to_owned(),
        })
        .scaffold();

        let manifest = find(&files, "jals.toml");
        assert!(manifest.contents.contains("name = \"demo\""));
        assert!(manifest.contents.contains("main-class = \"Main\""));
        assert!(
            manifest
                .contents
                .contains("# script = { type = \"rhai\", file = \"build.rhai\" }")
        );

        let main = find(&files, "src/main/java/Main.java");
        assert!(main.contents.contains("public class Main"));
        assert!(
            main.contents
                .contains("public static void main(String[] args)")
        );

        let ignore = find(&files, ".gitignore");
        assert_eq!(ignore.contents, "/target\n");
    }

    #[test]
    fn manifest_round_trips_through_the_parser() {
        let files = (InitOptions {
            name: "demo".to_owned(),
        })
        .scaffold();
        let manifest: Manifest = toml::from_str(&find(&files, "jals.toml").contents).unwrap();
        assert_eq!(manifest.package.name.as_deref(), Some("demo"));
        assert_eq!(manifest.package.version.as_deref(), Some("0.1.0"));
        assert_eq!(manifest.run.main_class.as_deref(), Some("Main"));
        // `[build]` is commented out, so the parsed manifest keeps the Maven defaults.
        assert_eq!(manifest.build.source_dirs, vec!["src/main/java".to_owned()]);
        assert_eq!(manifest.build.classes_dir, "target/classes");
        assert_eq!(manifest.build.script, None);
    }

    #[test]
    fn special_characters_in_the_name_are_escaped() {
        let files = (InitOptions {
            name: "a\"b\\c".to_owned(),
        })
        .scaffold();
        let manifest: Manifest = toml::from_str(&find(&files, "jals.toml").contents).unwrap();
        assert_eq!(manifest.package.name.as_deref(), Some("a\"b\\c"));
    }

    #[test]
    fn the_starter_class_matches_the_run_main_class() {
        let files = (InitOptions {
            name: "demo".to_owned(),
        })
        .scaffold();
        let manifest: Manifest = toml::from_str(&find(&files, "jals.toml").contents).unwrap();
        let main_class = manifest.run.main_class.unwrap();
        // The default-package class is reachable by its simple name, which the source defines.
        assert!(
            find(&files, "src/main/java/Main.java")
                .contents
                .contains(&format!("class {main_class}"))
        );
    }
}
