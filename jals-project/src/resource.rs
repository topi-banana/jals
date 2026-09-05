//! Which `[build] resource-dirs` files are rendered as templates, and what a render sees.
//!
//! A resource is whatever the author put in the directory — a PNG, an `.nbt`, a font — so the
//! default is still the byte-for-byte copy it always was. `[build.resources] template` names the
//! ones that are rendered instead, and nothing else in the tree is decoded at all.
//!
//! The engine itself is the [`jinja`] crate, which this module configures rather than implements.
//! Three of its settings are what make a *build tool's* templates behave the way
//! `jals-build/README.md` documents, and each is a decision rather than a default:
//!
//! - [`Environment::set_trim_block_lines`] — a block tag alone on its line takes the whole line
//!   with it. Resources are JSON and XML, where a stray blank line is a diff.
//! - [`UndefinedBehavior::SemiStrict`] — emitting a value that is not set is an error. In a build
//!   tool an unset value is a typo far more often than an intention, and a silently empty
//!   `"version": ""` reaches the jar and fails at load time instead. `| default("…")` is how the
//!   intentional case is spelled.
//! - [`Environment::set_strict_variables`] — a name nothing defines is a typo, not an empty
//!   string. It is the other half of the same rule: *unknown* is refused, *unset* has a `default`.
//!
//! The one thing that is genuinely this crate's is [`Features`]: a build feature set answers
//! membership for **any** name, because features are additive and "is X on" is a well-formed
//! question whether or not X was ever declared. That is a fact about `[features]`, so it lives here
//! as a [`jinja::Object`] rather than as a variant the engine would have to know about.

use alloc::borrow::ToOwned;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use jals_config::{Manifest, ResolvedBuildFeatures, ResourcePattern};
use jals_storage::{DirKey, ProjectView, RelativePath};
use jinja::{Enumerator, Environment, Object, UndefinedBehavior, Value, context};

/// The `[build] resource-dirs` to read, which of their files are rendered, and what the render
/// sees.
///
/// Lowered once, exactly where `[build] remap`'s mapping set is, so a host never reads
/// `[build.resources]` itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResourcePlan {
    dirs: Vec<DirKey>,
    /// Each declared glob beside the text it was written as, which is what an error names.
    templates: Vec<(String, ResourcePattern)>,
    context: TemplateContext,
}

impl ResourcePlan {
    /// Lower `[build] resource-dirs` and `[build.resources]` under one feature selection.
    pub(crate) fn lower(manifest: &Manifest, features: &ResolvedBuildFeatures) -> Self {
        // An entry `Manifest::validate` accepted always parses; one that does not is a manifest
        // that reached here unvalidated, and dropping it is the same answer the missing directory
        // in `entries` gets.
        let dirs = manifest
            .build
            .resource_dirs
            .iter()
            .filter_map(|dir| DirKey::parse(dir).ok())
            .collect();
        let templates = manifest
            .build
            .resources
            .template
            .iter()
            .filter_map(|pattern| {
                ResourcePattern::parse(pattern)
                    .ok()
                    .map(|glob| (pattern.clone(), glob))
            })
            .collect();
        Self {
            dirs,
            templates,
            context: TemplateContext::new(manifest, features.features()),
        }
    }

    /// The lowered `[build] resource-dirs`, for the test that pins the lowering.
    #[cfg(test)]
    pub(crate) fn dirs(&self) -> &[DirKey] {
        &self.dirs
    }

    /// Every resource in `view`, addressed by its path below the directory it was declared under —
    /// exactly as a class is addressed below `classes-dir` — rendered where one was declared.
    ///
    /// Sorted by that path, per directory, because the jar's member order is part of its bytes.
    /// The sort happens **before** anything is rendered, not after: `files_under` yields keys in
    /// segment order while the sort is over the joined string, so rendering during the walk would
    /// make *which* failure gets reported depend on an order the output never has.
    ///
    /// # Errors
    /// A message naming the resource that could not be rendered, or the declaration that named
    /// nothing.
    pub(crate) fn entries(
        &self,
        view: &ProjectView,
    ) -> Result<Vec<(RelativePath, Vec<u8>)>, String> {
        let environment = TemplateContext::environment();
        let context = self.context.value();
        let mut entries = Vec::new();
        let mut seen = BTreeSet::new();
        let mut present = 0usize;
        for dir in &self.dirs {
            // A declared directory that is not there is not a mistake: `[build] resource-dirs`
            // defaults onto every project, and most projects have no resources.
            if view.directory(dir).is_err() {
                continue;
            }
            present += 1;
            let mut found: Vec<_> = view
                .tree()
                .files_under(dir)
                .filter_map(|file| {
                    file.key()
                        .path()
                        .strip_prefix(dir.path())
                        .filter(|path| !path.is_root())
                        .map(|path| (path, file))
                })
                .collect();
            found.sort_by_key(|(path, _)| path.to_string());
            for (path, file) in found {
                let Some(index) = self.matched(&path) else {
                    entries.push((path, file.bytes().to_vec()));
                    continue;
                };
                seen.insert(index);
                let text = file.text().map_err(|error| {
                    format!(
                        "`{path}` is declared in `[build.resources] template` but is not UTF-8: \
                         {error}"
                    )
                })?;
                let rendered = environment
                    .render_str(text, context.clone())
                    .map_err(|error| format!("`{path}`: {error}"))?;
                entries.push((path, rendered.into_bytes()));
            }
        }
        self.check_matched(&seen, present)?;
        Ok(entries)
    }

    /// The declaration this member path is rendered by, if any. First match wins; the index is what
    /// records that the declaration was used.
    fn matched(&self, path: &RelativePath) -> Option<usize> {
        self.templates
            .iter()
            .position(|(_, glob)| glob.matches(path))
    }

    /// Fail on a declaration that rendered nothing.
    ///
    /// Unlike a missing `resource-dirs` entry, which is tolerated because the default lands on
    /// every project, a pattern here was written on purpose — so a typo that quietly ships an
    /// unrendered file is the silent wrong answer, not the failure. The two messages are separate
    /// because the fix is: one says make the directory, the other says fix the glob.
    fn check_matched(&self, seen: &BTreeSet<usize>, present: usize) -> Result<(), String> {
        let mut unmatched = self
            .templates
            .iter()
            .enumerate()
            .filter(|(index, _)| !seen.contains(index))
            .map(|(_, (pattern, _))| pattern.as_str());
        let Some(first) = unmatched.next() else {
            return Ok(());
        };
        if present == 0 {
            return Err(format!(
                "`[build.resources] template` names `{first}`, but no `[build] resource-dirs` \
                 directory exists in this project"
            ));
        }
        Err(format!(
            "`[build.resources] template` entry `{first}` matched no file under `[build] \
             resource-dirs`"
        ))
    }
}

/// The values a resource template can read.
///
/// Two namespaces and nothing else. Environment variables are deliberately absent: a value read
/// from the ambient environment is not part of any cache identity here, so a build that changed
/// nothing else would still have to be assumed stale.
///
/// Held as the manifest's own data rather than as a built [`Value`], so a `ResourcePlan` stays
/// comparable — two plans are equal when the manifest and the selection are.
///
/// Nothing memoizes a render: [`ResourcePlan::entries`] is reached only from `RemapPlan::run`,
/// which runs on every root build, and its bytes reach the cache through `RemapPlan::stage_key` —
/// a content digest over the staged jar. So a change to what the engine *writes* invalidates by
/// construction, and no `TASK_EXECUTION_VERSION`-style constant covers this path. Do not key a
/// future memo on this plan's equality without adding one: the plan names the manifest and the
/// selection, and says nothing about the engine that renders them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TemplateContext {
    /// Each `[package]` key a template may read, and whether the manifest set it. `None` is *known
    /// and absent*, which is what makes `| default("…")` meaningful — as opposed to a key that does
    /// not exist, which is a typo.
    package: BTreeMap<String, Option<String>>,
    features: BTreeSet<String>,
}

impl TemplateContext {
    /// The `[package]` metadata and the resolved build features, as one render sees them.
    fn new(manifest: &Manifest, features: &BTreeSet<String>) -> Self {
        let mut package = BTreeMap::new();
        package.insert("name".to_owned(), manifest.package.name.clone());
        package.insert("version".to_owned(), manifest.package.version.clone());
        Self {
            package,
            features: features.clone(),
        }
    }

    /// The engine as a build tool configures it; see this module's own documentation for why each
    /// of the three settings is what it is.
    fn environment() -> Environment {
        let mut environment = Environment::new();
        environment.set_trim_block_lines(true);
        environment.set_undefined_behavior(UndefinedBehavior::SemiStrict);
        environment.set_strict_variables(true);
        environment
    }

    /// The two namespaces, as the map a render is handed.
    fn value(&self) -> Value {
        let package: BTreeMap<String, Value> = self
            .package
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    value.as_deref().map_or(Value::UNDEFINED, Value::from),
                )
            })
            .collect();
        context! {
            package => Value::from(package),
            features => Value::from_object(Features(self.features.clone())),
        }
    }
}

/// The resolved build features, as a template reads them.
///
/// Indexing it asks *membership*, so it answers for any name at all: features are additive, and "is
/// X on" is a well-formed question whether or not X was ever declared. Checking against the
/// declared set instead would bind a template to `[features]` to buy only typo detection — and it
/// is exactly why this is an [`Object`] here rather than a shape the engine knows about.
#[derive(Debug)]
struct Features(BTreeSet<String>);

impl Object for Features {
    fn get_value(&self, key: &str) -> Option<Value> {
        Some(Value::from(self.0.contains(key)))
    }

    fn enumerate(&self) -> Enumerator {
        // A `BTreeSet`, so a `{% for %}` walks the same order on every host and every run.
        Enumerator::Values(
            self.0
                .iter()
                .map(|name| Value::from(name.as_str()))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use jals_storage::{CodeTree, Entry, FileKey, MemoryStorage};

    use super::*;

    fn manifest(text: &str) -> Manifest {
        text.parse().expect("test manifest is valid")
    }

    fn features(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn context(package: &str, active: &[&str]) -> TemplateContext {
        TemplateContext::new(&manifest(package), &features(active))
    }

    fn render(source: &str, context: &TemplateContext) -> Result<String, String> {
        TemplateContext::environment()
            .render_str(source, context.value())
            .map_err(|error| error.to_string())
    }

    fn view(files: &[(&str, &[u8])]) -> ProjectView {
        MemoryStorage::memory(
            CodeTree::new(files.iter().map(|(path, bytes)| {
                Entry::File(
                    FileKey::parse(path).expect("path is portable"),
                    bytes.to_vec(),
                )
            }))
            .expect("tree is well-formed"),
        )
        .view()
    }

    fn plan(text: &str) -> ResourcePlan {
        let manifest = manifest(text);
        let features = manifest
            .resolve_build_features(&[], false, false)
            .expect("selection is declared");
        ResourcePlan::lower(&manifest, &features)
    }

    const PACKAGE: &str = "[package]\nname = \"hellomod\"\nversion = \"0.1.0\"\n";

    #[test]
    fn package_metadata_reaches_the_render_and_an_unset_key_needs_a_default() {
        let full = context(PACKAGE, &[]);
        assert_eq!(
            render("{{ package.name }}-{{ package.version }}", &full).as_deref(),
            Ok("hellomod-0.1.0")
        );
        // Both spellings reach the same value.
        assert_eq!(
            render("{{ package[\"name\"] }}", &full).as_deref(),
            Ok("hellomod")
        );

        // A `[package]` key that is declared but unset is known and absent: emitting it is an
        // error, testing it is false, and `default` is how the intentional case is written.
        let bare = context("[package]\n", &[]);
        assert_eq!(
            render("{{ package.version }}", &bare),
            Err(
                "line 1, column 1: this value is not set; write `| default(\"…\")` to say what to \
                 use instead"
                    .to_owned()
            )
        );
        assert_eq!(
            render("{{ package.version | default(\"0.0.0\") }}", &bare).as_deref(),
            Ok("0.0.0")
        );
        assert_eq!(
            render(
                "{% if package.version %}set{% else %}unset{% endif %}",
                &bare
            )
            .as_deref(),
            Ok("unset")
        );

        // A typo is refused, and the two namespaces are all a template may read: a value from the
        // ambient environment is part of no cache identity here, so `env` is not one of them.
        assert_eq!(
            render("{{ env.HOME }}", &full),
            Err(
                "line 1, column 1: unknown name `env`; a template can read `features` and \
                 `package`"
                    .to_owned()
            )
        );
        assert_eq!(
            render("{{ package.licence }}", &full),
            Err(
                "line 1, column 1: `package` has no field `licence`; it has `name` and `version`"
                    .to_owned()
            )
        );
    }

    #[test]
    fn features_answer_membership_for_any_name() {
        let context = context(PACKAGE, &["server", "1.20.1", "mixin-extras"]);
        assert_eq!(
            render("{{ features.server }}", &context).as_deref(),
            Ok("true")
        );
        // A name that is not active is `false`, never an error: features are additive, so "is X
        // on" is a well-formed question about any name at all.
        assert_eq!(
            render("{{ features.client }}", &context).as_deref(),
            Ok("false")
        );
        // The bracket spelling is not sugar. `1.20.1` and `mixin-extras` are real feature names in
        // `examples/minecraft_mod/jals.toml`, and neither is a name `a.b` can carry.
        assert_eq!(
            render(
                "{{ features[\"1.20.1\"] }} {{ features[\"mixin-extras\"] }}",
                &context
            )
            .as_deref(),
            Ok("true true")
        );
        // And they iterate in sorted order, so a rendered resource is the same bytes every run.
        assert_eq!(
            render(
                "{% for f in features %}{{ f }}{% if not loop.last %},{% endif %}{% endfor %}",
                &context
            )
            .as_deref(),
            Ok("1.20.1,mixin-extras,server")
        );
    }

    #[test]
    fn a_block_tag_alone_on_its_line_takes_the_line_with_it() {
        // The setting this crate turns on, asserted where it is turned on: without it every
        // `{% if %}` in a JSON resource leaves a blank line behind, so the rendered file differs
        // from a hand-written one by whitespace nobody asked for.
        let source =
            "{\n{% if features.server %}\n  \"env\": \"server\",\n{% endif %}\n  \"x\": 1\n}\n";
        assert_eq!(
            render(source, &context(PACKAGE, &["server"])).as_deref(),
            Ok("{\n  \"env\": \"server\",\n  \"x\": 1\n}\n")
        );
        assert_eq!(
            render(source, &context(PACKAGE, &[])).as_deref(),
            Ok("{\n  \"x\": 1\n}\n")
        );

        // The shape the one shipped resource template actually has
        // (`examples/minecraft_mod/src/main/resources/mixins.hellomod.json`): an `elif` chain over
        // bracket-spelled feature names, inside JSON. Every arm has to render as valid JSON, which
        // is the whole reason the line rule is on.
        let chain = "{\n{% if features[\"since-1.20.5\"] %}\n  \"level\": 21,\n                     {% elif features[\"since-1.18\"] %}\n  \"level\": 17,\n                     {% else %}\n  \"level\": 8,\n{% endif %}\n  \"n\": \"{{ package.name }}\"\n}\n";
        for (active, level) in [
            (&["since-1.18", "since-1.20.5"][..], 21),
            (&["since-1.18"][..], 17),
            (&[][..], 8),
        ] {
            assert_eq!(
                render(chain, &context(PACKAGE, active)).as_deref(),
                Ok(format!("{{\n  \"level\": {level},\n  \"n\": \"hellomod\"\n}}\n").as_str()),
                "{active:?}"
            );
        }
    }

    const DECLARED: &str = "[package]\nname = \"hellomod\"\nversion = \"0.1.0\"\n\
                            [features]\nserver = []\n\
                            [build.resources]\ntemplate = [\"meta.json\", \"cfg/*.xml\"]\n";

    #[test]
    fn only_declared_resources_are_rendered() {
        let plan = plan(DECLARED);
        let entries = plan
            .entries(&view(&[
                (
                    "src/main/resources/meta.json",
                    b"{\"v\":\"{{ package.version }}\"}",
                ),
                ("src/main/resources/cfg/a.xml", b"<v>{{ package.name }}</v>"),
                // Undeclared, and it contains `{{` on purpose: selection is by declaration, never
                // by content, so this has to come back untouched.
                ("src/main/resources/keep.txt", b"{{ package.version }}"),
                (
                    "src/main/resources/icon.png",
                    &[0x89, b'P', b'N', b'G', 0xff],
                ),
            ]))
            .expect("every declaration matches");
        let rendered: Vec<(String, Vec<u8>)> = entries
            .into_iter()
            .map(|(path, bytes)| (path.to_string(), bytes))
            .collect();
        assert_eq!(
            rendered,
            alloc::vec![
                ("cfg/a.xml".to_owned(), b"<v>hellomod</v>".to_vec()),
                (
                    "icon.png".to_owned(),
                    alloc::vec![0x89, b'P', b'N', b'G', 0xff]
                ),
                ("keep.txt".to_owned(), b"{{ package.version }}".to_vec()),
                ("meta.json".to_owned(), b"{\"v\":\"0.1.0\"}".to_vec()),
            ]
        );
    }

    #[test]
    fn a_declaration_that_matches_nothing_fails() {
        // A `resource-dirs` entry that is not there is tolerated because the default lands on every
        // project. A pattern here was written on purpose, so a typo is a failure rather than a file
        // that quietly ships unrendered.
        let error = plan("[build.resources]\ntemplate = [\"typo.json\"]\n")
            .entries(&view(&[("src/main/resources/meta.json", b"{}")]))
            .expect_err("the pattern matches nothing");
        assert!(error.contains("`typo.json` matched no file"), "{error}");

        // No resource directory at all is a different fix, so it is a different sentence.
        let error = plan("[build.resources]\ntemplate = [\"typo.json\"]\n")
            .entries(&view(&[("src/main/java/A.java", b"class A {}")]))
            .expect_err("there is nowhere to match");
        assert!(
            error.contains("no `[build] resource-dirs` directory exists"),
            "{error}"
        );
    }

    #[test]
    fn a_declared_resource_that_is_not_text_fails() {
        let error = plan("[build.resources]\ntemplate = [\"blob.bin\"]\n")
            .entries(&view(&[("src/main/resources/blob.bin", &[0xff, 0xfe])]))
            .expect_err("a template has to be text");
        assert!(error.contains("is not UTF-8"), "{error}");
    }

    #[test]
    fn a_render_failure_names_the_resource() {
        let error = plan("[build.resources]\ntemplate = [\"meta.json\"]\n")
            .entries(&view(&[("src/main/resources/meta.json", b"{{ nope }}")]))
            .expect_err("the template does not render");
        assert_eq!(
            error,
            "`meta.json`: line 1, column 1: unknown name `nope`; a template can read `features` \
             and `package`"
        );
    }

    #[test]
    fn a_missing_resource_directory_is_skipped_in_silence() {
        // The existing behaviour, unchanged: the default lands on every project, and a project with
        // no resources is not a project with a mistake.
        assert!(
            plan("[package]\nname = \"x\"\n")
                .entries(&view(&[("src/main/java/A.java", b"class A {}")]))
                .expect("nothing declared, nothing to fail")
                .is_empty()
        );
    }
}
