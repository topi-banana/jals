//! Graph discovery, preprocessing, and projection over real host projects.
//!
//! These were an integration test until the assembly seam landed. They live inside the crate now
//! because what they exercise is the seam's *inside*: `discover`, `preprocess`, and the projection
//! are crate-internal steps that [`ProjectAssembly`](crate::ProjectAssembly) sequences, so a test
//! reaching them from outside would be the very hand-sequencing the seam exists to prevent. The
//! module is `native`-gated, which is the same range the integration test built in.

use std::fs;
use std::path::Path;
use std::process::Command;

use jals_build::build_script::{BuildScriptEnvironment, BuildScriptLimits};
use jals_build::task::TaskPublishIntent;
use jals_classpath::{DependencyLocation, ProjectInputOptions};
use jals_config::{Manifest, ResolvedBuildFeatures};
use jals_exec::Exec;
use jals_storage::{CodeTree, DirKey, Entry, FileKey, MemoryStorage, NativeStorage, RelativePath};

use crate::graph::NodeKind;
use crate::memory::MemoryProjectGraph;
use crate::native::NativeProjectGraph;
use crate::{CompileClasspathEntry, GraphError, GraphPreprocess, ProjectScript};

/// A fetch capability for graphs that declare no task plan. Reaching it is the failure.
struct UnreachableFetcher;

impl jals_classpath::Fetcher for UnreachableFetcher {
    async fn fetch(&self, locator: &str) -> Result<Vec<u8>, String> {
        panic!("this graph must not fetch, but asked for `{locator}`")
    }
}

/// Preprocessing inputs for a graph under test, defaulting everything a task plan would need.
///
/// A macro rather than a helper function because the borrowed defaults have to outlive the call and
/// nothing here owns them; as temporaries in the calling statement they live exactly long enough.
macro_rules! inert {
    () => {
        inert!(
            &BuildScriptEnvironment::new(),
            &ResolvedBuildFeatures::default(),
            &BuildScriptLimits::default()
        )
    };
    ($environment:expr, $features:expr, $limits:expr) => {
        GraphPreprocess {
            exec: &Exec::inline(),
            fetcher: &UnreachableFetcher,
            environment: $environment,
            root_features: $features,
            limits: $limits,
            network: jals_classpath::NetworkPolicy::Offline,
        }
    };
}

fn write(root: &Path, path: &str, contents: impl AsRef<[u8]>) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn manifest(text: &str) -> Manifest {
    text.parse().unwrap()
}

/// `paths` put back through `fs::canonicalize`, so a comparison against a freshly canonicalized
/// expectation is about *where* each path points rather than how it is spelled.
///
/// The adapter hands out canonical paths with the Windows verbatim prefix already stripped, which
/// is the spelling a host wants and not the one `fs::canonicalize` returns.
fn canonicalized(paths: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    paths
        .iter()
        .map(|path| fs::canonicalize(path).unwrap())
        .collect()
}

fn classpath_contains(entry: &CompileClasspathEntry, suffix: &str) -> bool {
    match entry {
        CompileClasspathEntry::File(file) => file.path.to_string().ends_with(suffix),
        CompileClasspathEntry::Tree(tree) => tree
            .members
            .iter()
            .any(|member| member.path.to_string().ends_with(suffix)),
    }
}

async fn storage(root: &Path, exec: &Exec) -> NativeStorage {
    NativeStorage::native(root, root.join(".cache"), exec.clone())
        .await
        .unwrap()
}

/// A `[build]` entry of a *dependency* names the project that declared it, as a `[dependencies]`
/// entry does — and needs it more, since `src/main/java` is what every project in the graph writes
/// and the entry alone narrows the search to nothing. The native location is a host directory, so
/// this also pins that a reader is given a path they can open.
#[test]
fn a_dependency_build_entry_names_the_project_that_declared_it() {
    jals_exec::tokio_rt::run(|exec| async move {
        let project = tempfile::tempdir().unwrap();
        write(
            project.path(),
            "dep/jals.toml",
            "[build]\nsource-dirs = [\"src/main/java\"]\nclasspath = [\"lib\"]\n",
        );
        let root = manifest("[dependencies]\ndep = { path = \"dep\" }\n");

        let graph = NativeProjectGraph::discover(
            &root,
            project.path(),
            &exec,
            jals_classpath::NetworkPolicy::Online,
        )
        .await
        .unwrap();
        let rendered = graph
            .warnings()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(rendered.len(), 2, "{rendered:?}");
        // The declaring project is the dependency's own directory, not the root's, and the tails
        // are host I/O errors this does not pin — the subject is what these are here for.
        for (entry, subject) in [
            ("src/main/java", "source directory is unavailable"),
            ("lib", "classpath entry is unavailable"),
        ] {
            assert!(
                rendered.iter().any(|warning| {
                    warning.starts_with(&format!("dependency `{entry}` of project `"))
                        && warning.contains(&format!("dep`: {subject}"))
                }),
                "{rendered:?}"
            );
        }
    })
    .unwrap();
}

#[test]
fn transitive_path_graph_is_classified_in_parent_discovery_order() {
    jals_exec::tokio_rt::run(|exec| async move {
        let project = tempfile::tempdir().unwrap();
        write(
            project.path(),
            "a/jals.toml",
            "[dependencies]\nb = { path = \"../b\" }\n",
        );
        write(project.path(), "a/src/A.java", "class A {}\n");
        write(project.path(), "b/src/main/java/B.java", "class B {}\n");
        let root = manifest("[dependencies]\na = { path = \"a\" }\n");

        let graph = NativeProjectGraph::discover(
            &root,
            project.path(),
            &exec,
            jals_classpath::NetworkPolicy::Online,
        )
        .await
        .unwrap();
        let metadata = graph.metadata();
        assert_eq!(metadata.nodes().len(), 2);
        assert_eq!(
            metadata
                .nodes()
                .iter()
                .map(|node| node.kind)
                .collect::<Vec<_>>(),
            [NodeKind::JalsSource, NodeKind::PlainSource]
        );
        assert_eq!(
            metadata
                .edges()
                .iter()
                .map(|edge| edge.dependency.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
    })
    .unwrap();
}

#[test]
fn native_and_memory_providers_coexist_under_native_features() {
    jals_exec::tokio_rt::run(|exec| async move {
        let project = tempfile::tempdir().unwrap();
        let root = manifest("[dependencies]\nmem = { path = \"dep\" }\n");
        let memory = MemoryStorage::memory(
            CodeTree::new([Entry::File(
                FileKey::parse("dep/src/Memory.java").unwrap(),
                b"class Memory {}".to_vec(),
            )])
            .unwrap(),
        );
        let memory_graph = MemoryProjectGraph::discover(&root, &memory.view())
            .await
            .unwrap();
        assert_eq!(memory_graph.metadata().nodes().len(), 1);

        write(project.path(), "dep/src/Native.java", "class Native {}\n");
        let native_graph = NativeProjectGraph::discover(
            &root,
            project.path(),
            &exec,
            jals_classpath::NetworkPolicy::Online,
        )
        .await
        .unwrap();
        assert_eq!(native_graph.metadata().nodes().len(), 1);
    })
    .unwrap();
}

#[test]
fn native_companion_source_archives_are_role_distinct() {
    jals_exec::tokio_rt::run(|exec| async move {
        let project = tempfile::tempdir().unwrap();
        write(project.path(), "lib/binary.jar", b"binary");
        write(project.path(), "lib/sources.jar", b"sources");
        let root = manifest(
            "[dependencies]\nlocal = { jar = \"lib/binary.jar\", sources = \"lib/sources.jar\" }\n\
             remote = { jar = \"https://example.invalid/binary.jar\", sources = \"https://example.invalid/sources.jar\" }\n",
        );
        let mut cache = MemoryStorage::memory(CodeTree::default());
        let graph = NativeProjectGraph::discover(&root, project.path(), &exec, jals_classpath::NetworkPolicy::Online)
            .await
            .unwrap()
            .preprocess(cache.artifacts_mut(), inert!())
            .await
            .unwrap();
        let assembly = graph.assemble(cache.artifacts_mut()).await;
        assert_eq!(assembly.graph.nodes().len(), 4);
        assert_eq!(assembly.plan.dependencies.len(), 2);
        assert_eq!(assembly.plan.source_archives.len(), 2);
        assert_eq!(assembly.compile_classpath.len(), 1);
    })
    .unwrap();
}

#[test]
fn manifest_probe_is_exact_and_malformed_manifest_is_hard() {
    jals_exec::tokio_rt::run(|exec| async move {
        let project = tempfile::tempdir().unwrap();
        write(project.path(), "base/jals.toml", "not valid = [\n");
        write(project.path(), "base/selected/src/S.java", "class S {}\n");
        let selected =
            manifest("[dependencies]\nselected = { path = \"base\", dir = \"selected\" }\n");
        let graph = NativeProjectGraph::discover(
            &selected,
            project.path(),
            &exec,
            jals_classpath::NetworkPolicy::Online,
        )
        .await
        .unwrap();
        assert_eq!(graph.metadata().nodes()[0].kind, NodeKind::PlainSource);

        write(
            project.path(),
            "base/selected/jals.toml",
            "[build]\nsource-dirs = [\n",
        );
        let error = NativeProjectGraph::discover(
            &selected,
            project.path(),
            &exec,
            jals_classpath::NetworkPolicy::Online,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, GraphError::MalformedManifest { .. }));

        write(
            project.path(),
            "base/selected/jals.toml",
            "[build]\nsource-dirs = [\"src\"]\n",
        );
        let graph = NativeProjectGraph::discover(
            &selected,
            project.path(),
            &exec,
            jals_classpath::NetworkPolicy::Online,
        )
        .await
        .unwrap();
        assert_eq!(graph.metadata().nodes()[0].kind, NodeKind::JalsSource);
    })
    .unwrap();
}

#[test]
fn diamond_deduplicates_nodes_and_cycle_reports_edge_chain() {
    jals_exec::tokio_rt::run(|exec| async move {
        let project = tempfile::tempdir().unwrap();
        for side in ["left", "right"] {
            write(
                project.path(),
                &format!("{side}/jals.toml"),
                "[dependencies]\nshared = { path = \"../shared\" }\n",
            );
        }
        write(project.path(), "shared/src/S.java", "class S {}\n");
        let root =
            manifest("[dependencies]\nleft = { path = \"left\" }\nright = { path = \"right\" }\n");
        let graph = NativeProjectGraph::discover(
            &root,
            project.path(),
            &exec,
            jals_classpath::NetworkPolicy::Online,
        )
        .await
        .unwrap();
        assert_eq!(graph.metadata().nodes().len(), 3);
        assert_eq!(graph.metadata().edges().len(), 4);

        write(
            project.path(),
            "shared/jals.toml",
            "[dependencies]\nleft-again = { path = \"../left\" }\n",
        );
        let error = NativeProjectGraph::discover(
            &root,
            project.path(),
            &exec,
            jals_classpath::NetworkPolicy::Online,
        )
        .await
        .unwrap_err();
        let GraphError::Cycle { chain } = error else {
            panic!("expected cycle");
        };
        assert_eq!(
            chain
                .iter()
                .map(|edge| edge.dependency.as_str())
                .collect::<Vec<_>>(),
            ["shared", "left-again"]
        );
    })
    .unwrap();
}

#[test]
fn relative_child_jar_and_classpath_become_verified_artifacts() {
    jals_exec::tokio_rt::run(|exec| async move {
        let project = tempfile::tempdir().unwrap();
        write(
            project.path(),
            "child/jals.toml",
            "[build]\nsource-dirs = [\"src\"]\nclasspath = [\"../lib/Api.class\"]\n\
             [dependencies]\njar = { jar = \"../lib/dep.jar\" }\n",
        );
        write(project.path(), "child/src/C.java", "class C {}\n");
        write(project.path(), "lib/Api.class", b"class bytes");
        write(project.path(), "lib/dep.jar", b"jar bytes");
        let root = manifest("[dependencies]\nchild = { path = \"child\" }\n");
        let mut root_storage = storage(project.path(), &exec).await;
        let graph = NativeProjectGraph::discover(
            &root,
            project.path(),
            &exec,
            jals_classpath::NetworkPolicy::Online,
        )
        .await
        .unwrap();
        let graph = graph
            .preprocess(root_storage.artifacts_mut(), inert!())
            .await
            .unwrap();
        let assembly = graph.assemble(root_storage.artifacts_mut()).await;

        assert!(assembly.errors.is_empty(), "{:?}", assembly.errors);
        assert!(matches!(
            assembly.plan.dependencies[0].location,
            DependencyLocation::Artifact(_)
        ));
        assert!(
            assembly
                .compile_classpath
                .iter()
                .any(|entry| classpath_contains(entry, "Api.class"))
        );
        assert!(
            assembly
                .compile_classpath
                .iter()
                .any(|entry| classpath_contains(entry, "dep.jar"))
        );
    })
    .unwrap();
}

#[test]
fn declared_classpath_directory_remains_one_compile_tree() {
    jals_exec::tokio_rt::run(|exec| async move {
        let project = tempfile::tempdir().unwrap();
        write(
            project.path(),
            "child/jals.toml",
            "[build]\nclasspath = [\"../classes\"]\n",
        );
        write(project.path(), "classes/pkg/Api.class", b"api");
        write(project.path(), "classes/pkg/internal/Impl.class", b"impl");
        let root = manifest("[dependencies]\nchild = { path = \"child\" }\n");
        let mut root_storage = storage(project.path(), &exec).await;
        let graph = NativeProjectGraph::discover(
            &root,
            project.path(),
            &exec,
            jals_classpath::NetworkPolicy::Online,
        )
        .await
        .unwrap()
        .preprocess(root_storage.artifacts_mut(), inert!())
        .await
        .unwrap();
        let assembly = graph.assemble(root_storage.artifacts_mut()).await;

        assert!(assembly.errors.is_empty(), "{:?}", assembly.errors);
        assert_eq!(assembly.plan.classpath.len(), 2);
        let [CompileClasspathEntry::Tree(tree)] = assembly.compile_classpath.as_slice() else {
            panic!("expected one compile classpath tree");
        };
        assert_eq!(
            tree.members
                .iter()
                .map(|member| member.path.to_string())
                .collect::<Vec<_>>(),
            ["pkg/Api.class", "pkg/internal/Impl.class"]
        );
    })
    .unwrap();
}

#[test]
fn binary_diamond_emits_one_first_edge_spec_and_ors_recursive() {
    jals_exec::tokio_rt::run(|exec| async move {
        let project = tempfile::tempdir().unwrap();
        write(
            project.path(),
            "left/jals.toml",
            "[dependencies]\nshared = { jar = \"../lib/shared.jar\" }\n",
        );
        write(
            project.path(),
            "right/jals.toml",
            "[dependencies]\nalias = { jar = \"../lib/shared.jar\", recursive = true }\n",
        );
        write(project.path(), "lib/shared.jar", b"shared");
        let root =
            manifest("[dependencies]\nleft = { path = \"left\" }\nright = { path = \"right\" }\n");
        let mut root_storage = storage(project.path(), &exec).await;
        let graph = NativeProjectGraph::discover(
            &root,
            project.path(),
            &exec,
            jals_classpath::NetworkPolicy::Online,
        )
        .await
        .unwrap()
        .preprocess(root_storage.artifacts_mut(), inert!())
        .await
        .unwrap();
        let assembly = graph.assemble(root_storage.artifacts_mut()).await;

        assert_eq!(assembly.plan.dependencies.len(), 1);
        assert_eq!(assembly.plan.dependencies[0].name.as_str(), "shared");
        assert!(assembly.plan.dependencies[0].recursive);
        assert_eq!(assembly.compile_classpath.len(), 1);
    })
    .unwrap();
}

#[test]
fn mixed_local_and_remote_binary_specs_keep_first_edge_order() {
    jals_exec::tokio_rt::run(|exec| async move {
        let project = tempfile::tempdir().unwrap();
        write(project.path(), "lib/local.jar", b"local");
        let root = manifest(
            "[dependencies]\na-remote = { jar = \"https://example.invalid/a.jar\" }\n\
             b-local = { jar = \"lib/local.jar\" }\n\
             c-remote = { jar = \"https://example.invalid/c.jar\" }\n",
        );
        let mut cache = MemoryStorage::memory(CodeTree::default());
        let graph = NativeProjectGraph::discover(
            &root,
            project.path(),
            &exec,
            jals_classpath::NetworkPolicy::Online,
        )
        .await
        .unwrap()
        .preprocess(cache.artifacts_mut(), inert!())
        .await
        .unwrap();
        let assembly = graph.assemble(cache.artifacts_mut()).await;
        assert_eq!(
            assembly
                .plan
                .dependencies
                .iter()
                .map(|dependency| dependency.name.as_str())
                .collect::<Vec<_>>(),
            ["a-remote", "b-local", "c-remote"]
        );
    })
    .unwrap();
}

#[test]
fn native_compile_classpath_keeps_mixed_local_and_remote_order() {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for stream in listener.incoming().take(2) {
            let mut stream = stream.unwrap();
            let mut request = [0; 1024];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            let bytes: &[u8] = if request.starts_with("GET /a.jar ") {
                b"remote-a"
            } else if request.starts_with("GET /c.jar ") {
                b"remote-c"
            } else {
                panic!("unexpected request: {request}");
            };
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            )
            .unwrap();
            stream.write_all(bytes).unwrap();
        }
    });

    jals_exec::tokio_rt::run(|exec| async move {
        let project = tempfile::tempdir().unwrap();
        write(project.path(), "lib/local.jar", b"local");
        let root = manifest(&format!(
            "[dependencies]\na-remote = {{ jar = \"http://{address}/a.jar\" }}\n\
             b-local = {{ jar = \"lib/local.jar\" }}\n\
             c-remote = {{ jar = \"http://{address}/c.jar\" }}\n"
        ));
        let mut root_storage = storage(project.path(), &exec).await;
        let graph = NativeProjectGraph::discover(
            &root,
            project.path(),
            &exec,
            jals_classpath::NetworkPolicy::Online,
        )
        .await
        .unwrap()
        .preprocess(root_storage.artifacts_mut(), inert!())
        .await
        .unwrap();
        let assembly = ProjectScript::skipped()
            .project_native(
                &graph,
                &root,
                project.path(),
                &mut root_storage,
                ProjectInputOptions::Compile,
            )
            .await;
        assert!(assembly.errors.is_empty(), "{:?}", assembly.errors);
        let mut contents = Vec::new();
        for entry in &assembly.compile_classpath {
            let CompileClasspathEntry::File(file) = entry else {
                panic!("binary dependencies must materialize as files");
            };
            contents.push(
                root_storage
                    .artifacts()
                    .lookup(&file.key)
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        assert_eq!(
            contents,
            [
                b"remote-a".to_vec(),
                b"local".to_vec(),
                b"remote-c".to_vec()
            ]
        );
    })
    .unwrap();
    server.join().unwrap();
}

#[test]
fn every_node_kind_preprocesses_and_scripts_export_only_sources_and_classpath() {
    jals_exec::tokio_rt::run(|exec| async move {
        let project = tempfile::tempdir().unwrap();
        write(project.path(), "plain/src/P.java", "class P {}\n");
        write(
            project.path(),
            "scripted/jals.toml",
            "[build]\nsource-dirs = [\"src\"]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
        );
        write(
            project.path(),
            "scripted/build.rhai",
            r#"
                let generated = output.write_text("Generated.java", "class Generated {}");
                let generated_cp = output.write("generated.jar", [1, 2, 3]);
                build.add_source(generated);
                build.add_source("src/Existing.java");
                build.add_classpath(generated_cp);
                build.add_classpath("lib/existing.jar");
                build.add_javac_arg("-should-not-propagate");
                build.add_jvm_arg("-also-not-propagated");
                build.metadata("private", "value");
            "#,
        );
        write(
            project.path(),
            "scripted/src/Existing.java",
            "class Existing {}\n",
        );
        write(project.path(), "scripted/lib/existing.jar", b"existing");
        write(project.path(), "lib/binary.jar", b"binary");
        let root = manifest(
            "[dependencies]\nbinary = { jar = \"lib/binary.jar\" }\n\
             plain = { path = \"plain\" }\nscripted = { path = \"scripted\" }\n",
        );
        let mut root_storage = storage(project.path(), &exec).await;
        let graph = NativeProjectGraph::discover(&root, project.path(), &exec, jals_classpath::NetworkPolicy::Online)
            .await
            .unwrap();
        assert_eq!(
            graph
                .metadata()
                .nodes()
                .iter()
                .map(|node| node.kind)
                .collect::<Vec<_>>(),
            [NodeKind::Binary, NodeKind::PlainSource, NodeKind::JalsSource]
        );
        let graph = graph
            .preprocess(root_storage.artifacts_mut(), inert!())
            .await
            .unwrap();
        let assembly = graph.assemble(root_storage.artifacts_mut()).await;
        assert!(assembly.errors.is_empty(), "{:?}", assembly.errors);
        let source_paths: Vec<_> = assembly
            .plan
            .source_dependency_artifacts
            .iter()
            .map(|source| source.path.to_string())
            .collect();
        assert!(source_paths.iter().any(|path| path.ends_with("Generated.java")));
        assert!(source_paths.iter().any(|path| path.ends_with("Existing.java")));
        assert!(assembly
            .compile_classpath
            .iter()
            .any(|entry| classpath_contains(entry, "generated.jar")));
        assert!(assembly
            .compile_classpath
            .iter()
            .any(|entry| classpath_contains(entry, "existing.jar")));
        assert_eq!(
            fs::read_to_string(project.path().join("scripted/src/Existing.java")).unwrap(),
            "class Existing {}\n"
        );
        assert!(!project.path().join("scripted/target").exists());
    })
    .unwrap();
}

#[test]
fn node_tokens_isolate_identical_script_paths_and_outputs() {
    jals_exec::tokio_rt::run(|exec| async move {
        let project = tempfile::tempdir().unwrap();
        for (dependency, class_name) in [("one", "One"), ("two", "Two")] {
            write(
                project.path(),
                &format!("{dependency}/jals.toml"),
                "[build]\nsource-dirs = [\"src\"]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
            );
            write(
                project.path(),
                &format!("{dependency}/build.rhai"),
                format!(
                    "let source = output.write_text(\"Same.java\", \"class {class_name} {{}}\"); build.add_source(source);"
                ),
            );
        }
        let root = manifest(
            "[dependencies]\none = { path = \"one\" }\ntwo = { path = \"two\" }\n",
        );
        let mut root_storage = storage(project.path(), &exec).await;
        let graph = NativeProjectGraph::discover(&root, project.path(), &exec, jals_classpath::NetworkPolicy::Online)
            .await
            .unwrap()
            .preprocess(root_storage.artifacts_mut(), inert!())
            .await
            .unwrap();
        let assembly = graph.assemble(root_storage.artifacts_mut()).await;
        let generated: Vec<_> = assembly
            .plan
            .source_dependency_artifacts
            .iter()
            .filter(|source| source.path.to_string().ends_with("Same.java"))
            .collect();
        assert_eq!(generated.len(), 2);
        assert_ne!(generated[0].path, generated[1].path);
        assert_ne!(generated[0].key, generated[1].key);
        assert!(!project.path().join("one/target").exists());
        assert!(!project.path().join("two/target").exists());
        assert!(generated
            .iter()
            .all(|source| source.path.starts_with(&RelativePath::parse("dependencies").unwrap())));
    })
    .unwrap();
}

#[test]
fn git_identity_uses_head_not_checkout_path_and_local_children_stay_confined() {
    jals_exec::tokio_rt::run(|exec| async move {
        let project = tempfile::tempdir().unwrap();
        let repository = project.path().join("repository");
        fs::create_dir_all(&repository).unwrap();
        write(
            &repository,
            "jals.toml",
            "[dependencies]\nchild = { path = \"child\" }\n",
        );
        write(&repository, "child/src/Child.java", "class Child {}\n");
        assert!(
            Command::new("git")
                .current_dir(&repository)
                .args(["init", "--quiet"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .current_dir(&repository)
                .args(["add", "."])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .current_dir(&repository)
                .args([
                    "-c",
                    "user.name=jals",
                    "-c",
                    "user.email=jals@example.invalid",
                    "commit",
                    "--quiet",
                    "-m",
                    "fixture",
                ])
                .status()
                .unwrap()
                .success()
        );
        let root = manifest("[dependencies]\nrepo = { git = \"repository\" }\n");

        let first = NativeProjectGraph::discover(
            &root,
            project.path(),
            &exec,
            jals_classpath::NetworkPolicy::Online,
        )
        .await
        .unwrap();
        let second = NativeProjectGraph::discover(
            &root,
            project.path(),
            &exec,
            jals_classpath::NetworkPolicy::Online,
        )
        .await
        .unwrap();
        assert_eq!(first.metadata(), second.metadata());
        assert_eq!(first.metadata().nodes().len(), 2);
        assert!(
            first
                .metadata()
                .nodes()
                .iter()
                .all(|node| node.id.token().len() == 64)
        );
        // Both nodes live in a temporary checkout, so both are named by the repository they came
        // from — by the argument `git clone` was given, never by the identity framing beside it,
        // which is NUL-delimited and carries a commit no reader asked to see. The two reading the
        // same is the documented cost of naming a node by where it came from.
        let locations = first.locations();
        assert_eq!(locations.len(), 2, "{locations:?}");
        for location in &locations {
            assert!(!location.contains('\0'), "{location:?}");
            assert!(location.ends_with("repository"), "{location:?}");
        }

        let outside = project.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        write(&outside, "src/Outside.java", "class Outside {}\n");
        write(
            &repository,
            "jals.toml",
            format!(
                "[dependencies]\noutside = {{ path = {:?} }}\n",
                outside.to_string_lossy()
            ),
        );
        assert!(
            Command::new("git")
                .current_dir(&repository)
                .args(["add", "jals.toml"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .current_dir(&repository)
                .args([
                    "-c",
                    "user.name=jals",
                    "-c",
                    "user.email=jals@example.invalid",
                    "commit",
                    "--quiet",
                    "-m",
                    "escape fixture",
                ])
                .status()
                .unwrap()
                .success()
        );
        let graph = NativeProjectGraph::discover(
            &root,
            project.path(),
            &exec,
            jals_classpath::NetworkPolicy::Online,
        )
        .await
        .unwrap();
        assert_eq!(graph.metadata().nodes().len(), 1);
        assert!(
            graph
                .warnings()
                .iter()
                .any(|warning| warning.message.contains("leaves its checkout"))
        );
    })
    .unwrap();
}

#[test]
fn native_projection_returns_watch_paths_and_applies_mode_downstream() {
    jals_exec::tokio_rt::run(|exec| async move {
        let project = tempfile::tempdir().unwrap();
        write(project.path(), "dep/src/D.java", "class D {}\n");
        write(project.path(), "src/main/java/Root.java", "class Root {}\n");
        let root = manifest("[dependencies]\ndep = { path = \"dep\" }\n");
        let mut root_storage = storage(project.path(), &exec).await;
        let graph = NativeProjectGraph::discover(
            &root,
            project.path(),
            &exec,
            jals_classpath::NetworkPolicy::Online,
        )
        .await
        .unwrap()
        .preprocess(root_storage.artifacts_mut(), inert!())
        .await
        .unwrap();
        let analysis = ProjectScript::skipped()
            .project_native(
                &graph,
                &root,
                project.path(),
                &mut root_storage,
                ProjectInputOptions::Analysis,
            )
            .await;
        assert_eq!(
            canonicalized(&analysis.watch_paths),
            [fs::canonicalize(project.path().join("dep")).unwrap()]
        );
        assert!(analysis.inputs.source_dep_sources.is_empty());
        assert_eq!(analysis.plan.source_dependency_artifacts.len(), 1);

        let editor = ProjectScript::skipped()
            .project_native(
                &graph,
                &root,
                project.path(),
                &mut root_storage,
                ProjectInputOptions::Editor,
            )
            .await;
        assert_eq!(editor.inputs.source_dep_sources.len(), 1);
    })
    .unwrap();
}

/// The native graph phase as a host actually calls it: one `resolve_native`, no step named by the
/// caller.
///
/// Every other test here drives the steps individually, because that is how it can pin one of them.
/// This one exists because `resolve_native` is what `jals-cli` and `jals-lsp` both call, and nothing
/// in this crate exercised it — its only coverage was through the host crates, which is thin cover
/// for the composition itself: a step dropped from the sequence, or a task classpath that stopped
/// reaching the projection, would show up first in someone else's test suite.
#[test]
fn resolve_native_runs_the_whole_graph_phase_in_one_call() {
    const CLASS: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../jals-classpath/tests/fixtures/Box.class"
    ));

    jals_exec::tokio_rt::run(|exec| async move {
        let project = tempfile::tempdir().unwrap();
        write(project.path(), "dep/src/D.java", "class D {}\n");
        write(project.path(), "src/main/java/Root.java", "class Root {}\n");
        write(project.path(), "lib/Box.class", CLASS);
        let root = manifest(
            "[build]\nsource-dirs = [\"src/main/java\"]\nclasspath = [\"lib/Box.class\"]\n\
             [dependencies]\ndep = { path = \"dep\" }\n",
        );
        let mut root_storage = storage(project.path(), &exec).await;

        // Stand in for a task terminal's output: a verified jar on the root's classpath. Published
        // directly because what is under test is whether the phase carries it into the projection,
        // not how a terminal acquired it.
        let task_jar = jar(&[("Box.class", CLASS)]);
        let task_key = jals_storage::CacheKey::new(
            jals_storage::CacheNamespace::BuildTaskArtifact,
            jals_storage::ProvenanceFold::new(b"resolve-native-test\0").finish(),
            jals_storage::ContentDigest::of(&task_jar),
        );
        root_storage
            .artifacts_mut()
            .publish(&task_key, &task_jar)
            .await
            .unwrap();

        let assembly = ProjectScript::from_parts(None, vec![task_key.clone()])
            .resolve_native(
                &root,
                project.path(),
                &mut root_storage,
                inert!(),
                ProjectInputOptions::Editor,
            )
            .await
            .unwrap();

        assert!(assembly.errors.is_empty(), "{:?}", assembly.errors);
        // Discovery ran: the declared path dependency became a watched directory, which only
        // discovery produces.
        assert_eq!(
            canonicalized(&assembly.watch_paths),
            [fs::canonicalize(project.path().join("dep")).unwrap()]
        );
        // Preprocessing and projection ran: the dependency's sources are resolved inputs.
        assert_eq!(assembly.inputs.source_dep_sources.len(), 1);
        // The root plan was lowered through the host path pipeline, not skipped.
        assert_eq!(
            assembly.source_roots,
            [DirKey::parse("src/main/java").unwrap()]
        );
        // The script phase's task classpath reached the projection and leads the graph's own.
        // Asserted on the key rather than a materialized path: this host canonicalizes `/var` to
        // `/private/var`, so a path comparison here would be testing the temporary directory.
        assert_eq!(
            assembly.plan.classpath.first(),
            Some(&jals_classpath::ClasspathEntry::Artifact(task_key))
        );
    })
    .unwrap();
}

#[test]
fn dependency_snapshots_exclude_git_and_jals_cache_inputs() {
    jals_exec::tokio_rt::run(|exec| async move {
        let project = tempfile::tempdir().unwrap();
        write(
            project.path(),
            "dep/jals.toml",
            "[build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
        );
        write(
            project.path(),
            "dep/build.rhai",
            r#"
                if project.exists("target/jals/cache/secret") || project.exists(".git/secret") {
                    build.error("excluded host state leaked into the dependency snapshot");
                }
            "#,
        );
        write(project.path(), "dep/target/jals/cache/secret", b"cache");
        write(project.path(), "dep/.git/secret", b"git");
        let root = manifest("[dependencies]\ndep = { path = \"dep\" }\n");
        let mut cache = MemoryStorage::memory(CodeTree::default());
        let graph = NativeProjectGraph::discover(
            &root,
            project.path(),
            &exec,
            jals_classpath::NetworkPolicy::Online,
        )
        .await
        .unwrap();
        graph
            .preprocess(cache.artifacts_mut(), inert!())
            .await
            .unwrap();
    })
    .unwrap();
}

#[cfg(unix)]
#[test]
fn snapshot_diagnostics_warn_but_unreadable_manifest_is_hard() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;
    use std::os::unix::fs::symlink;

    jals_exec::tokio_rt::run(|exec| async move {
        let project = tempfile::tempdir().unwrap();
        write(project.path(), "warn/src/W.java", "class W {}\n");
        // A trailing `0xff` is not valid UTF-8. Linux filesystems store the bytes as given; APFS and
        // HFS+ reject the name outright (`EILSEQ`), so the entry is *attempted* and the warning it
        // drives is asserted only where the fixture could be created. The stand-down is loud,
        // because a fixture that silently failed to exist reads as a pass.
        let non_utf8_entry = std::fs::write(
            project
                .path()
                .join("warn")
                .join(OsString::from_vec(vec![b'b', b'a', b'd', 0xff])),
            b"ignored",
        )
        .is_ok();
        if !non_utf8_entry {
            eprintln!(
                "note: this filesystem rejects non-UTF-8 names; the `NonUtf8Entry` half of this \
                 test is checking nothing"
            );
        }
        let warning_root = manifest("[dependencies]\nwarn = { path = \"warn\" }\n");
        let graph = NativeProjectGraph::discover(
            &warning_root,
            project.path(),
            &exec,
            jals_classpath::NetworkPolicy::Online,
        )
        .await
        .unwrap();
        // Asserted through `Display`, not `.message`: what a host shows is the whole warning, and
        // the half these pin is the *subject*. The same byte drives two diagnostics — the root
        // snapshot walks into `warn/`, and `warn` snapshots itself — attributed differently, and
        // the root's is the case that carries no node at all.
        let rendered = graph
            .warnings()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert!(
            !non_utf8_entry
                || rendered.iter().any(|warning| {
                    warning.starts_with("project graph: snapshot: ")
                        && warning.contains("NonUtf8Entry")
                }),
            "{rendered:?}"
        );
        assert!(
            !non_utf8_entry
                || rendered.iter().any(|warning| {
                    warning.starts_with("dependency project `")
                        && warning.contains("warn`: snapshot: ")
                        && warning.contains("NonUtf8Entry")
                }),
            "{rendered:?}"
        );

        std::fs::create_dir(project.path().join("hard")).unwrap();
        write(
            project.path(),
            "outside/jals.toml",
            "[package]\nname = \"outside\"\n",
        );
        symlink(
            "../outside/jals.toml",
            project.path().join("hard/jals.toml"),
        )
        .unwrap();
        let hard_root = manifest("[dependencies]\nhard = { path = \"hard\" }\n");
        assert!(matches!(
            NativeProjectGraph::discover(
                &hard_root,
                project.path(),
                &exec,
                jals_classpath::NetworkPolicy::Online
            )
            .await,
            Err(GraphError::Acquisition { .. })
        ));
    })
    .unwrap();
}

#[test]
fn memory_and_native_resolve_sibling_inputs_relative_to_the_selected_project() {
    jals_exec::tokio_rt::run(|exec| async move {
        let dependency_manifest =
            "[build]\nsource-dirs = [\"../shared-src\"]\nclasspath = [\"../classes\"]\n\
             [dependencies]\nshared = { jar = \"../lib/shared.jar\" }\n";
        let root = manifest("[dependencies]\ndep = { path = \"dep\" }\n");
        let files: [(&str, &[u8]); 4] = [
            ("dep/jals.toml", dependency_manifest.as_bytes()),
            ("shared-src/Shared.java", b"class Shared {}"),
            ("classes/pkg/Api.class", b"api"),
            ("lib/shared.jar", b"jar"),
        ];
        let memory_storage = MemoryStorage::memory(
            CodeTree::new(files.iter().map(|(path, bytes)| {
                Entry::File(FileKey::parse(path).unwrap(), bytes.to_vec())
            }))
            .unwrap(),
        );
        let mut memory_cache = MemoryStorage::memory(CodeTree::default());
        let memory = MemoryProjectGraph::discover(&root, &memory_storage.view())
            .await
            .unwrap()
            .preprocess(memory_cache.artifacts_mut(), inert!())
            .await
            .unwrap()
            .assemble(memory_cache.artifacts_mut())
            .await;

        let project = tempfile::tempdir().unwrap();
        for (path, bytes) in files {
            write(project.path(), path, bytes);
        }
        let mut native_cache = storage(project.path(), &exec).await;
        let native = NativeProjectGraph::discover(&root, project.path(), &exec, jals_classpath::NetworkPolicy::Online)
            .await
            .unwrap()
            .preprocess(native_cache.artifacts_mut(), inert!())
            .await
            .unwrap()
            .assemble(native_cache.artifacts_mut())
            .await;

        for assembly in [&memory, &native] {
            assert_eq!(assembly.plan.dependencies.len(), 1);
            assert_eq!(assembly.plan.source_dependency_artifacts.len(), 1);
            let [CompileClasspathEntry::Tree(tree), CompileClasspathEntry::File(_)] =
                assembly.compile_classpath.as_slice()
            else {
                panic!("expected a classpath tree followed by the dependency jar");
            };
            assert_eq!(tree.members[0].path.to_string(), "pkg/Api.class");
        }
    })
    .unwrap();
}

/// A stored-only jar holding exactly `entries`.
fn jar(entries: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::{Cursor, Write};

    let mut bytes = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut bytes);
    for (name, contents) in entries {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(contents).unwrap();
    }
    zip.finish().unwrap();
    bytes.into_inner()
}

/// A root project with one path dependency that runs `script`, plus whatever extra `files` it needs.
///
/// An entry in `files` replaces the default at the same path, so a test that needs a different
/// dependency manifest just writes one.
fn task_dependency(script: &str, files: &[(&str, &[u8])]) -> (Manifest, MemoryStorage) {
    let defaults: [(&str, &[u8]); 3] = [
        (
            "dep/jals.toml",
            b"[build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
        ),
        ("dep/build.rhai", script.as_bytes()),
        // A source root the dependency actually has, so capturing it emits no warning.
        ("dep/src/main/java/Seed.java", b"class Seed {}"),
    ];
    let entries: std::collections::BTreeMap<_, _> = defaults
        .iter()
        .chain(files)
        .map(|(path, bytes)| ((*path).to_owned(), bytes.to_vec()))
        .collect();
    let storage = MemoryStorage::memory(
        CodeTree::new(
            entries
                .into_iter()
                .map(|(path, bytes)| Entry::File(FileKey::parse(&path).unwrap(), bytes)),
        )
        .unwrap(),
    );
    (
        manifest("[dependencies]\ndep = { path = \"dep\" }\n"),
        storage,
    )
}

/// Discover, preprocess and project a graph whose whole task plan is local, so nothing in it can
/// reach the network — `inert!` supplies a fetcher that panics if anything tries.
///
/// The cache is returned to the caller rather than owned here because a *consuming* assertion has
/// to read the published artifacts back out of the same one.
async fn local_assembly(
    root: &Manifest,
    storage: &MemoryStorage,
    cache: &mut MemoryStorage,
) -> crate::assemble::ProjectGraphAssembly {
    MemoryProjectGraph::discover(root, &storage.view())
        .await
        .unwrap()
        .preprocess(cache.artifacts_mut(), inert!())
        .await
        .unwrap()
        .assemble(cache.artifacts_mut())
        .await
}

/// Every node's publication coverage diagnosis, in discovery order.
///
/// Read off the *preprocessed* graph rather than the assembled warnings so an assertion can name
/// the facts the check found — owner, destination, prefix, intent — instead of pinning the sentence
/// they happen to be written into.
async fn publication_diagnoses(
    root: &Manifest,
    storage: &MemoryStorage,
) -> Vec<crate::graph::PublicationDiagnosis> {
    let mut cache = MemoryStorage::memory(CodeTree::default());
    publication_diagnoses_in(root, storage, &mut cache).await
}

/// The same against a caller-owned cache, so a test can preprocess twice into one.
async fn publication_diagnoses_in(
    root: &Manifest,
    storage: &MemoryStorage,
    cache: &mut MemoryStorage,
) -> Vec<crate::graph::PublicationDiagnosis> {
    let graph = MemoryProjectGraph::discover(root, &storage.view())
        .await
        .unwrap()
        .preprocess(cache.artifacts_mut(), inert!())
        .await
        .unwrap();
    graph
        .nodes
        .iter()
        .filter_map(|node| graph.exports.get(&node.id))
        .filter_map(|exports| exports.unbacked_publications.clone())
        .collect()
}

/// The one diagnosis a graph with a single publishing dependency is expected to produce.
async fn only_diagnosis(
    root: &Manifest,
    storage: &MemoryStorage,
) -> crate::graph::PublicationDiagnosis {
    let found = publication_diagnoses(root, storage).await;
    let [diagnosis] = found.as_slice() else {
        panic!("expected exactly one diagnosis, got {found:?}");
    };
    diagnosis.clone()
}

/// A dependency that publishes one tree of `net/example` sources with the given intent, out of a
/// jar it holds itself so the plan needs no network.
fn publishing_dependency(intent: &str, files: &[(&str, &[u8])]) -> (Manifest, MemoryStorage) {
    publishing_dependency_with(intent, "", files)
}

/// The same, with `extra` appended to the script — a second publication, a classpath entry, or both.
fn publishing_dependency_with(
    intent: &str,
    extra: &str,
    files: &[(&str, &[u8])],
) -> (Manifest, MemoryStorage) {
    let sources = jar(&[("net/example/Api.java", b"package net.example; class Api {}")]);
    let script = format!(
        r#"
            let archive = tasks.project_jar("vendor/sources.jar");
            let tree = tasks.extract_java(archive, "net/example");
            tasks.publish_tree("api", tree, "src/main/java/net/example", "replace-root", "{intent}");
            {extra}
        "#
    );
    let mut all: Vec<(&str, &[u8])> = vec![("dep/vendor/sources.jar", &sources)];
    all.extend_from_slice(files);
    task_dependency(&script, &all)
}

/// A dependency manifest whose `[build] classpath` names `entries`.
fn manifest_with_classpath(entries: &[&str]) -> Vec<u8> {
    let list = entries
        .iter()
        .map(|entry| format!("\"{entry}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "[build]\nscript = {{ type = \"rhai\", file = \"build.rhai\" }}\nclasspath = [{list}]\n"
    )
    .into_bytes()
}

/// Counts fetches so a test can tell a cache hit from a network round trip.
struct CountingFetcher {
    responses: std::collections::BTreeMap<String, Vec<u8>>,
    calls: std::sync::atomic::AtomicUsize,
}

impl CountingFetcher {
    fn new(responses: &[(&str, &[u8])]) -> Self {
        Self {
            responses: responses
                .iter()
                .map(|(url, bytes)| ((*url).to_owned(), bytes.to_vec()))
                .collect(),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl jals_classpath::Fetcher for CountingFetcher {
    async fn fetch(&self, locator: &str) -> Result<Vec<u8>, String> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.responses
            .get(locator)
            .cloned()
            .ok_or_else(|| format!("unexpected fetch `{locator}`"))
    }
}

#[test]
fn a_dependency_build_task_puts_its_jar_on_the_consumer_classpath() {
    jals_exec::block_on_inline(async {
        let game = jar(&[("pkg/Api.class", b"api")]);
        let script = format!(
            r#"
                let game = tasks.fetch_jar(
                    tasks.https_url("https://example.invalid/game.jar"),
                    tasks.sha256("{}"),
                    tasks.bytes(4096)
                );
                tasks.add_classpath(game);
            "#,
            jals_storage::ContentDigest::of(&game).to_hex()
        );
        let (root, view_storage) = task_dependency(&script, &[]);
        let mut cache = MemoryStorage::memory(CodeTree::default());
        let fetcher = CountingFetcher::new(&[("https://example.invalid/game.jar", &game)]);

        let assembly = MemoryProjectGraph::discover(&root, &view_storage.view())
            .await
            .unwrap()
            .preprocess(
                cache.artifacts_mut(),
                GraphPreprocess {
                    exec: &Exec::inline(),
                    fetcher: &fetcher,
                    environment: &BuildScriptEnvironment::new(),
                    root_features: &ResolvedBuildFeatures::default(),
                    limits: &BuildScriptLimits::default(),
                    network: jals_classpath::NetworkPolicy::Online,
                },
            )
            .await
            .unwrap()
            .assemble(cache.artifacts_mut())
            .await;

        assert!(assembly.errors.is_empty(), "{:?}", assembly.errors);
        assert_eq!(fetcher.calls(), 1);
        // The consumer compiles and analyses against the task's JAR, exactly as it would against a
        // `jar` dependency — that is the whole point of letting a dependency declare tasks.
        assert!(
            assembly
                .compile_classpath
                .iter()
                .any(|entry| classpath_contains(entry, "build-task/0.jar")),
            "{:?}",
            assembly.compile_classpath
        );
        assert!(assembly.plan.classpath.iter().any(|entry| matches!(
            entry,
            jals_classpath::ClasspathEntry::ArtifactFile { path, .. }
                if path.to_string().ends_with("build-task/0.jar")
        )));
    });
}

#[test]
fn a_dependency_publication_becomes_navigation_source_and_never_touches_the_snapshot() {
    jals_exec::block_on_inline(async {
        let sources = jar(&[("net/example/Api.java", b"package net.example; class Api {}")]);
        let script = format!(
            r#"
                let archive = tasks.fetch_jar(
                    tasks.https_url("https://example.invalid/sources.jar"),
                    tasks.sha256("{}"),
                    tasks.bytes(4096)
                );
                let tree = tasks.extract_java(archive, "net/example");
                tasks.publish_tree("api", tree, "src/main/java/net/example", "replace-root", "navigation");
            "#,
            jals_storage::ContentDigest::of(&sources).to_hex()
        );
        let (root, view_storage) = task_dependency(&script, &[]);
        let before = view_storage.view();
        let mut cache = MemoryStorage::memory(CodeTree::default());
        let fetcher = CountingFetcher::new(&[("https://example.invalid/sources.jar", &sources)]);

        let assembly = MemoryProjectGraph::discover(&root, &before)
            .await
            .unwrap()
            .preprocess(
                cache.artifacts_mut(),
                GraphPreprocess {
                    exec: &Exec::inline(),
                    fetcher: &fetcher,
                    environment: &BuildScriptEnvironment::new(),
                    root_features: &ResolvedBuildFeatures::default(),
                    limits: &BuildScriptLimits::default(),
                    network: jals_classpath::NetworkPolicy::Online,
                },
            )
            .await
            .unwrap()
            .assemble(cache.artifacts_mut())
            .await;

        assert!(assembly.errors.is_empty(), "{:?}", assembly.errors);
        // Package-relative, like every other library source, so one type resolves to one artifact
        // however many producers offer it.
        assert_eq!(
            assembly
                .plan
                .library_source_artifacts
                .iter()
                .map(|source| source.path.to_string())
                .collect::<Vec<_>>(),
            ["net/example/Api.java"]
        );
        // Navigation only: handing a decompiled skeleton to `javac` alongside the classpath JAR
        // that already defines the same types is how a working build turns into duplicate-class
        // errors.
        assert!(
            assembly
                .plan
                .source_dependency_artifacts
                .iter()
                .all(|source| !source.path.to_string().ends_with("Api.java"))
        );
        // The dependency is a snapshot, not a workspace: publication may not reach it.
        assert_eq!(view_storage.view().revision(), before.revision());
        assert!(
            view_storage
                .view()
                .file(&FileKey::parse("dep/src/main/java/net/example/Api.java").unwrap())
                .is_err()
        );
    });
}

#[test]
fn a_dependency_publication_outside_a_source_root_is_rejected() {
    jals_exec::block_on_inline(async {
        let sources = jar(&[("net/example/Api.java", b"package net.example; class Api {}")]);
        let script = format!(
            r#"
                let archive = tasks.fetch_jar(
                    tasks.https_url("https://example.invalid/sources.jar"),
                    tasks.sha256("{}"),
                    tasks.bytes(4096)
                );
                let tree = tasks.extract_java(archive, "net/example");
                tasks.publish_tree("api", tree, "generated/net/example", "replace-root", "navigation");
            "#,
            jals_storage::ContentDigest::of(&sources).to_hex()
        );
        let (root, view_storage) = task_dependency(&script, &[]);
        let mut cache = MemoryStorage::memory(CodeTree::default());
        let fetcher = CountingFetcher::new(&[("https://example.invalid/sources.jar", &sources)]);

        let error = MemoryProjectGraph::discover(&root, &view_storage.view())
            .await
            .unwrap()
            .preprocess(
                cache.artifacts_mut(),
                GraphPreprocess {
                    exec: &Exec::inline(),
                    fetcher: &fetcher,
                    environment: &BuildScriptEnvironment::new(),
                    root_features: &ResolvedBuildFeatures::default(),
                    limits: &BuildScriptLimits::default(),
                    network: jals_classpath::NetworkPolicy::Online,
                },
            )
            .await
            .unwrap_err();

        let GraphError::BuildScript {
            location, message, ..
        } = &error
        else {
            panic!("expected a build-script error, got {error:?}");
        };
        // The digest alone would not tell a reader which dependency to go and look at.
        assert_eq!(location, "dep");
        assert!(message.contains("source-dirs"), "{message}");
    });
}

#[test]
fn a_dependency_task_execution_is_memoized_across_preprocessing() {
    jals_exec::block_on_inline(async {
        // `project_jar` reads the dependency's own snapshot, so removing that file between runs
        // makes the plan impossible to execute a second time. If the second preprocess still
        // succeeds with the same result, it can only have come from the recorded execution.
        let script = r#"
            let vendor = tasks.project_jar("vendor/lib.jar");
            tasks.add_classpath(vendor);
        "#;
        let library = jar(&[("pkg/Api.class", b"api")]);
        let (root, with_jar) = task_dependency(script, &[("dep/vendor/lib.jar", &library)]);
        let mut cache = MemoryStorage::memory(CodeTree::default());

        let first = MemoryProjectGraph::discover(&root, &with_jar.view())
            .await
            .unwrap()
            .preprocess(cache.artifacts_mut(), inert!())
            .await
            .unwrap()
            .assemble(cache.artifacts_mut())
            .await;
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert_eq!(first.compile_classpath.len(), 1);

        let (root, without_jar) = task_dependency(script, &[]);
        let second = MemoryProjectGraph::discover(&root, &without_jar.view())
            .await
            .unwrap()
            .preprocess(cache.artifacts_mut(), inert!())
            .await
            .unwrap()
            .assemble(cache.artifacts_mut())
            .await;
        assert!(second.errors.is_empty(), "{:?}", second.errors);
        assert_eq!(second.compile_classpath, first.compile_classpath);
    });
}

#[test]
fn a_memoized_dependency_execution_is_keyed_on_its_build_features() {
    jals_exec::block_on_inline(async {
        // Two feature selections produce two plans. Sharing one record between them would serve
        // whichever ran first, silently building the wrong thing.
        let script = r#"
            let name = if build.feature("wide") { "wide" } else { "narrow" };
            let vendor = tasks.project_jar("vendor/" + name + ".jar");
            tasks.add_classpath(vendor);
        "#;
        let narrow = jar(&[("pkg/Narrow.class", b"narrow")]);
        let wide = jar(&[("pkg/Wide.class", b"wide")]);
        let files: [(&str, &[u8]); 3] = [
            (
                "dep/jals.toml",
                b"[features]\nwide = []\n\
                  [build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
            ),
            ("dep/vendor/narrow.jar", &narrow),
            ("dep/vendor/wide.jar", &wide),
        ];
        let (_, view_storage) = task_dependency(script, &files);
        let mut cache = MemoryStorage::memory(CodeTree::default());

        let mut keys = Vec::new();
        for entry in [
            "dep = { path = \"dep\" }",
            "dep = { path = \"dep\", features = [\"wide\"] }",
        ] {
            let root = manifest(&format!("[dependencies]\n{entry}\n"));
            let assembly = MemoryProjectGraph::discover(&root, &view_storage.view())
                .await
                .unwrap()
                .preprocess(cache.artifacts_mut(), inert!())
                .await
                .unwrap()
                .assemble(cache.artifacts_mut())
                .await;
            assert!(assembly.errors.is_empty(), "{:?}", assembly.errors);
            let [CompileClasspathEntry::File(file)] = assembly.compile_classpath.as_slice() else {
                panic!("expected exactly the task JAR");
            };
            keys.push(file.key.clone());
        }
        assert_ne!(keys[0], keys[1]);
    });
}

#[test]
fn a_dependency_publication_reaches_the_editor_but_not_the_compiler() {
    jals_exec::block_on_inline(async {
        // The producing half is asserted above on `plan.library_source_artifacts`; this is the
        // consuming half — that a host actually sees those sources, and only in the mode meant for
        // a reader.
        let sources = jar(&[("net/example/Api.java", b"package net.example; class Api {}")]);
        let script = format!(
            r#"
                let archive = tasks.fetch_jar(
                    tasks.https_url("https://example.invalid/sources.jar"),
                    tasks.sha256("{}"),
                    tasks.bytes(4096)
                );
                tasks.add_classpath(archive);
                let tree = tasks.extract_java(archive, "net/example");
                tasks.publish_tree("api", tree, "src/main/java/net/example", "replace-root", "navigation");
            "#,
            jals_storage::ContentDigest::of(&sources).to_hex()
        );
        let (root, view_storage) = task_dependency(&script, &[]);
        let mut cache = MemoryStorage::memory(CodeTree::default());
        let fetcher = CountingFetcher::new(&[("https://example.invalid/sources.jar", &sources)]);

        let assembly = MemoryProjectGraph::discover(&root, &view_storage.view())
            .await
            .unwrap()
            .preprocess(
                cache.artifacts_mut(),
                GraphPreprocess {
                    exec: &Exec::inline(),
                    fetcher: &fetcher,
                    environment: &BuildScriptEnvironment::new(),
                    root_features: &ResolvedBuildFeatures::default(),
                    limits: &BuildScriptLimits::default(),
                    network: jals_classpath::NetworkPolicy::Online,
                },
            )
            .await
            .unwrap()
            .assemble(cache.artifacts_mut())
            .await;
        assert!(assembly.errors.is_empty(), "{:?}", assembly.errors);

        let editor = jals_classpath::ProjectInputs::assemble(
            &fetcher,
            &mut cache,
            &assembly.plan,
            ProjectInputOptions::Editor,
        )
        .await;
        assert!(
            editor
                .library_sources
                .iter()
                .any(|source| source.path.to_string() == "net/example/Api.java"),
            "{:?}",
            editor
                .library_sources
                .iter()
                .map(|source| source.path.to_string())
                .collect::<Vec<_>>()
        );

        let compile = jals_classpath::ProjectInputs::assemble(
            &fetcher,
            &mut cache,
            &assembly.plan,
            ProjectInputOptions::Compile,
        )
        .await;
        assert!(compile.library_sources.is_empty());
        assert!(
            compile
                .source_dep_sources
                .iter()
                .all(|source| !format!("{source:?}").contains("Api.java"))
        );
    });
}

/// The other half of the routing. A tree its script declares as the only carrier of its package
/// joins the dependency's own sources on the way through the dependency's own frontend, so it is
/// addressed the way those are — project-relative, under the node token — and not the way a library
/// source is.
#[test]
fn a_compile_intent_publication_is_projected_as_a_source_dependency() {
    jals_exec::block_on_inline(async {
        let (root, view_storage) = publishing_dependency("compile", &[]);
        let before = view_storage.view();
        let mut cache = MemoryStorage::memory(CodeTree::default());
        let assembly = local_assembly(&root, &view_storage, &mut cache).await;
        assert!(assembly.errors.is_empty(), "{:?}", assembly.errors);

        // Routed, not fanned out: reaching both channels would mount one type twice in an editor.
        assert!(
            assembly.plan.library_source_artifacts.is_empty(),
            "{:?}",
            assembly.plan.library_source_artifacts
        );
        let published: Vec<_> = assembly
            .plan
            .source_dependency_artifacts
            .iter()
            .map(|source| source.path.to_string())
            .filter(|path| path.ends_with("Api.java"))
            .collect();
        let [path] = published.as_slice() else {
            panic!("expected exactly one published compile source, got {published:?}");
        };
        // The node token is the half a package address does not have, and the half two dependencies
        // publishing one package need.
        assert!(path.starts_with("dependencies/"), "{path}");
        assert!(
            path.ends_with("/sources/src/main/java/net/example/Api.java"),
            "{path}"
        );

        // The dependency is still a snapshot, not a workspace: the intent decides where the value
        // goes, never whether it may be written back.
        assert_eq!(view_storage.view().revision(), before.revision());
        assert!(
            view_storage
                .view()
                .file(&FileKey::parse("dep/src/main/java/net/example/Api.java").unwrap())
                .is_err()
        );
    });
}

/// The consuming half, against the mode `a_dependency_publication_reaches_the_editor_but_not_the
/// _compiler` pins the opposite of: a declared compile input is what the compiler is handed.
#[test]
fn a_compile_intent_publication_reaches_the_compiler() {
    jals_exec::block_on_inline(async {
        let (root, view_storage) = publishing_dependency("compile", &[]);
        let mut cache = MemoryStorage::memory(CodeTree::default());
        let assembly = local_assembly(&root, &view_storage, &mut cache).await;
        assert!(assembly.errors.is_empty(), "{:?}", assembly.errors);

        for options in [ProjectInputOptions::Compile, ProjectInputOptions::Editor] {
            let inputs = jals_classpath::ProjectInputs::assemble(
                &UnreachableFetcher,
                &mut cache,
                &assembly.plan,
                options,
            )
            .await;
            assert!(
                inputs
                    .source_dep_sources
                    .iter()
                    .any(|source| format!("{source:?}").contains("Api.java")),
                "{options:?}: {:?}",
                inputs.source_dep_sources
            );
            // Never as a library source, in either mode — that is the channel it was routed out of.
            assert!(
                inputs
                    .library_sources
                    .iter()
                    .all(|source| !source.path.to_string().ends_with("Api.java")),
                "{options:?}"
            );
        }
    });
}

/// Building a dependency in its own directory leaves its publications on disk, where the next
/// consumer's discovery captures them as ordinary authored sources. Whether a consumer compiled
/// therefore used to depend on whether somebody had ever run a build in a directory they may not
/// even have looked at.
///
/// `replace-root` owns its destination, so nothing found there is authored — running the plan as a
/// root would delete it before writing the tree — and assembly reads it the same way.
#[test]
fn a_publication_destination_is_owned_in_a_dependency_too() {
    jals_exec::block_on_inline(async {
        let (root, view_storage) = publishing_dependency(
            "navigation",
            &[
                // What a previous root build of this dependency left behind: the publication's own
                // file, plus one the tree no longer produces.
                (
                    "dep/src/main/java/net/example/Api.java",
                    b"package net.example; class Api {}",
                ),
                (
                    "dep/src/main/java/net/example/Removed.java",
                    b"package net.example; class Removed {}",
                ),
            ],
        );
        let mut cache = MemoryStorage::memory(CodeTree::default());
        let assembly = local_assembly(&root, &view_storage, &mut cache).await;
        assert!(assembly.errors.is_empty(), "{:?}", assembly.errors);

        let compiled: Vec<_> = assembly
            .plan
            .source_dependency_artifacts
            .iter()
            .map(|source| source.path.to_string())
            .collect();
        // Neither reaches the compiler: the publication is `navigation`, and what it left on disk
        // is not a second opinion about that.
        assert!(
            compiled.iter().all(|path| !path.contains("net/example")),
            "{compiled:?}"
        );
        // Scoped to the destination, not to the project: an authored file outside it is still an
        // input.
        assert!(
            compiled.iter().any(|path| path.ends_with("Seed.java")),
            "{compiled:?}"
        );
    });
}

/// The same ownership rule decides *which* copy a `compile` publication contributes, since both are
/// addressed identically and only one can survive the deduplication.
#[test]
fn a_compile_publication_supersedes_what_a_root_build_left_at_its_destination() {
    jals_exec::block_on_inline(async {
        let (root, view_storage) = publishing_dependency(
            "compile",
            &[(
                "dep/src/main/java/net/example/Api.java",
                b"package net.example; class Api { int stale; }",
            )],
        );
        let mut cache = MemoryStorage::memory(CodeTree::default());
        let assembly = local_assembly(&root, &view_storage, &mut cache).await;
        assert!(assembly.errors.is_empty(), "{:?}", assembly.errors);

        let published: Vec<_> = assembly
            .plan
            .source_dependency_artifacts
            .iter()
            .filter(|source| source.path.to_string().ends_with("net/example/Api.java"))
            .collect();
        let [api] = published.as_slice() else {
            panic!("expected exactly one `Api.java`, got {published:?}");
        };
        // The publication's bytes, not the ones sitting in the directory. Comparing the artifact is
        // the only assertion that can tell them apart — both are addressed the same.
        let bytes = cache.artifacts().lookup(&api.key).await.unwrap().unwrap();
        assert_eq!(bytes, b"package net.example; class Api {}");
    });
}

/// A destination outside every declared source root is a mistake whoever reads the tree, so the
/// check runs for both intents. The compile routing does not *use* the package prefix, which is
/// exactly why it would be easy to stop computing it — and then a dependency would reach the check
/// nowhere, since only the root host validates a destination again.
#[test]
fn a_compile_intent_publication_outside_a_source_root_is_rejected() {
    jals_exec::block_on_inline(async {
        let sources = jar(&[("net/example/Api.java", b"package net.example; class Api {}")]);
        let (root, view_storage) = task_dependency(
            r#"
                let archive = tasks.project_jar("vendor/sources.jar");
                let tree = tasks.extract_java(archive, "net/example");
                tasks.publish_tree("api", tree, "generated/net/example", "replace-root", "compile");
            "#,
            &[("dep/vendor/sources.jar", &sources)],
        );
        let mut cache = MemoryStorage::memory(CodeTree::default());

        let error = MemoryProjectGraph::discover(&root, &view_storage.view())
            .await
            .unwrap()
            .preprocess(cache.artifacts_mut(), inert!())
            .await
            .unwrap_err();

        let GraphError::BuildScript {
            location, message, ..
        } = &error
        else {
            panic!("expected a build-script error, got {error:?}");
        };
        assert_eq!(location, "dep");
        assert!(message.contains("source-dirs"), "{message}");
    });
}

/// The shape #189 is about: a publication that is the only carrier of its package, routed away from
/// the compiler because the classpath was assumed to define the same types, with nothing there that
/// does.
#[test]
fn a_publication_no_classpath_entry_backs_is_diagnosed() {
    jals_exec::block_on_inline(async {
        let (root, storage) = publishing_dependency("navigation", &[]);
        let diagnosis = only_diagnosis(&root, &storage).await;

        let [unbacked] = diagnosis.roots.as_slice() else {
            panic!(
                "expected exactly one unbacked root, got {:?}",
                diagnosis.roots
            );
        };
        assert_eq!(unbacked.owner, "api");
        assert_eq!(
            unbacked.destination.to_string(),
            "src/main/java/net/example"
        );
        assert_eq!(unbacked.prefix.to_string(), "net/example");
        assert_eq!(unbacked.intent, TaskPublishIntent::Navigation);
        // Nothing went unread and nothing was declared the check could not see, so it qualifies
        // itself in neither direction.
        assert!(diagnosis.unread.is_empty(), "{:?}", diagnosis.unread);
        assert!(!diagnosis.dependencies_unseen);
    });
}

/// Declaring the tree a compile input does not make the classpath carry it. The finding stands and
/// the intent travels with it, because the two cases are wrong in different ways and a reader has
/// to be told which one this is.
#[test]
fn an_unbacked_compile_publication_is_diagnosed_and_says_so_differently() {
    jals_exec::block_on_inline(async {
        let (navigation_root, navigation_storage) = publishing_dependency("navigation", &[]);
        let (compile_root, compile_storage) = publishing_dependency("compile", &[]);
        let navigation = only_diagnosis(&navigation_root, &navigation_storage).await;
        let compile = only_diagnosis(&compile_root, &compile_storage).await;

        assert_eq!(compile.roots[0].intent, TaskPublishIntent::Compile);
        assert_eq!(navigation.roots[0].intent, TaskPublishIntent::Navigation);
        // Same owner, same destination, same prefix — the intent is the whole difference, and it
        // has to reach the reader.
        assert_ne!(compile.to_string(), navigation.to_string());
    });
}

/// The shape the routing was written for: a JAR on the classpath and a readable tree beside it.
#[test]
fn a_publication_the_task_classpath_backs_is_silent() {
    jals_exec::block_on_inline(async {
        let classes = jar(&[("net/example/Api.class", b"class bytes")]);
        let (root, storage) = publishing_dependency_with(
            "navigation",
            r#"tasks.add_classpath(tasks.project_jar("vendor/lib.jar"));"#,
            &[("dep/vendor/lib.jar", &classes)],
        );

        assert!(publication_diagnoses(&root, &storage).await.is_empty());
    });
}

/// A `[build] classpath` jar backs a publication exactly as a task-registered one does. Discovery
/// captured its bytes, so this is also the cheap half of the fold — the one that settles the answer
/// before any cache key is opened.
#[test]
fn a_build_classpath_jar_backs_a_publication() {
    jals_exec::block_on_inline(async {
        let classes = jar(&[("net/example/Api.class", b"class bytes")]);
        let (root, storage) = publishing_dependency(
            "navigation",
            &[
                (
                    "dep/jals.toml",
                    &manifest_with_classpath(&["vendor/lib.jar"]),
                ),
                ("dep/vendor/lib.jar", &classes),
            ],
        );

        assert!(publication_diagnoses(&root, &storage).await.is_empty());
    });
}

/// A classpath *directory* is a package root, so a captured member's path already spells its binary
/// name and no class file has to be parsed to learn it.
#[test]
fn a_build_classpath_directory_backs_a_publication() {
    jals_exec::block_on_inline(async {
        let (root, storage) = publishing_dependency(
            "navigation",
            &[
                (
                    "dep/jals.toml",
                    &manifest_with_classpath(&["vendor/classes"]),
                ),
                ("dep/vendor/classes/net/example/Api.class", b"class bytes"),
            ],
        );

        assert!(publication_diagnoses(&root, &storage).await.is_empty());
    });
}

/// Per root, not per node: backing one published package says nothing about another.
#[test]
fn only_the_unbacked_root_of_a_multi_root_publication_is_diagnosed() {
    jals_exec::block_on_inline(async {
        let classes = jar(&[("net/example/Api.class", b"class bytes")]);
        let library = jar(&[("org/vendor/Tool.java", b"package org.vendor; class Tool {}")]);
        let (root, storage) = publishing_dependency_with(
            "navigation",
            r#"
                tasks.add_classpath(tasks.project_jar("vendor/lib.jar"));
                let extra = tasks.project_jar("vendor/library.jar");
                let second = tasks.extract_java(extra, "org/vendor");
                tasks.publish_tree("tool", second, "src/main/java/org/vendor", "replace-root",
                                   "navigation");
            "#,
            &[
                ("dep/vendor/lib.jar", &classes),
                ("dep/vendor/library.jar", &library),
            ],
        );
        let diagnosis = only_diagnosis(&root, &storage).await;

        let [unbacked] = diagnosis.roots.as_slice() else {
            panic!(
                "expected exactly one unbacked root, got {:?}",
                diagnosis.roots
            );
        };
        assert_eq!(unbacked.owner, "tool");
        assert_eq!(unbacked.prefix.to_string(), "org/vendor");
    });
}

/// Two publications under one package tree are covered by one class beneath both prefixes, and
/// uncovered together when there is none.
#[test]
fn two_publications_sharing_a_package_tree_are_covered_together() {
    jals_exec::block_on_inline(async {
        let deep = jar(&[(
            "net/example/deep/Deep.java",
            b"package net.example.deep; class Deep {}",
        )]);
        let extra = r#"
            let nested = tasks.project_jar("vendor/deep.jar");
            let tree = tasks.extract_java(nested, "net/example/deep");
            tasks.publish_tree("deep", tree, "src/main/java/net/example/deep", "replace-root",
                               "navigation");
        "#;

        let (root, storage) =
            publishing_dependency_with("navigation", extra, &[("dep/vendor/deep.jar", &deep)]);
        let diagnosis = only_diagnosis(&root, &storage).await;
        // Terminal order, which is also the order the report lists them in: `api` is declared
        // first, `deep` by the appended script.
        let prefixes: Vec<_> = diagnosis
            .roots
            .iter()
            .map(|unbacked| unbacked.prefix.to_string())
            .collect();
        assert_eq!(prefixes, ["net/example", "net/example/deep"]);

        // One class under the deeper package is under both prefixes, so it answers for both.
        let classes = jar(&[("net/example/deep/Deep.class", b"class bytes")]);
        let (root, storage) = publishing_dependency_with(
            "navigation",
            &format!(
                r#"{extra}
                   tasks.add_classpath(tasks.project_jar("vendor/lib.jar"));"#
            ),
            &[
                ("dep/vendor/deep.jar", &deep),
                ("dep/vendor/lib.jar", &classes),
            ],
        );
        assert!(publication_diagnoses(&root, &storage).await.is_empty());
    });
}

/// `[dependencies]` become graph nodes long before preprocessing and contribute at assembly, so the
/// check cannot see them. Staying silent would lose the warning for every root a project publishes
/// as soon as it gains one jar; saying what could not be seen is the honest half.
#[test]
fn a_publishing_dependency_with_dependencies_still_reports_and_says_what_it_could_not_see() {
    jals_exec::block_on_inline(async {
        let classes = jar(&[("net/example/Api.class", b"class bytes")]);
        let (root, storage) = publishing_dependency(
            "navigation",
            &[
                (
                    "dep/jals.toml",
                    b"[build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n\
                      [dependencies]\nlib = { jar = \"vendor/lib.jar\" }\n",
                ),
                ("dep/vendor/lib.jar", &classes),
            ],
        );
        let diagnosis = only_diagnosis(&root, &storage).await;

        assert_eq!(diagnosis.roots.len(), 1);
        assert!(diagnosis.dependencies_unseen);
    });
}

/// The caveat is a property of the project, not of each root — which is what makes it one report
/// with two roots rather than two reports each restating it.
#[test]
fn the_dependencies_caveat_belongs_to_the_report_and_not_to_each_root() {
    jals_exec::block_on_inline(async {
        let library = jar(&[("org/vendor/Tool.java", b"package org.vendor; class Tool {}")]);
        let (root, storage) = publishing_dependency_with(
            "navigation",
            r#"
                let extra = tasks.project_jar("vendor/library.jar");
                let second = tasks.extract_java(extra, "org/vendor");
                tasks.publish_tree("tool", second, "src/main/java/org/vendor", "replace-root",
                                   "navigation");
            "#,
            &[
                (
                    "dep/jals.toml",
                    b"[build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n\
                      [dependencies]\nlib = { jar = \"vendor/library.jar\" }\n",
                ),
                ("dep/vendor/library.jar", &library),
            ],
        );
        let diagnosis = only_diagnosis(&root, &storage).await;

        assert_eq!(diagnosis.roots.len(), 2);
        assert!(diagnosis.dependencies_unseen);
    });
}

/// An entry that could not be read is not an entry that defines nothing — but it is also not a
/// reason to throw away what the rest of the classpath *did* answer. The roots stay, the unread
/// entry rides along, and the reader gets both halves.
#[test]
fn an_unreadable_classpath_entry_is_reported_beside_the_roots_not_instead_of_them() {
    jals_exec::block_on_inline(async {
        let (root, storage) = publishing_dependency(
            "navigation",
            &[
                (
                    "dep/jals.toml",
                    &manifest_with_classpath(&["vendor/broken.jar"]),
                ),
                ("dep/vendor/broken.jar", b"not a zip archive"),
            ],
        );
        let diagnosis = only_diagnosis(&root, &storage).await;

        assert_eq!(diagnosis.roots.len(), 1);
        assert_eq!(diagnosis.unread.len(), 1);
    });
}

/// Two unread entries and two roots: the smallest shape where per-entry and per-report differ.
#[test]
fn every_unread_classpath_entry_is_named_in_one_report() {
    jals_exec::block_on_inline(async {
        let library = jar(&[("org/vendor/Tool.java", b"package org.vendor; class Tool {}")]);
        let (root, storage) = publishing_dependency_with(
            "navigation",
            r#"
                let extra = tasks.project_jar("vendor/library.jar");
                let second = tasks.extract_java(extra, "org/vendor");
                tasks.publish_tree("tool", second, "src/main/java/org/vendor", "replace-root",
                                   "navigation");
            "#,
            &[
                (
                    "dep/jals.toml",
                    &manifest_with_classpath(&["vendor/broken.jar", "vendor/junk.jar"]),
                ),
                ("dep/vendor/broken.jar", b"not a zip archive"),
                ("dep/vendor/junk.jar", b"also not a zip archive"),
                ("dep/vendor/library.jar", &library),
            ],
        );
        let diagnosis = only_diagnosis(&root, &storage).await;

        assert_eq!(diagnosis.roots.len(), 2);
        assert_eq!(diagnosis.unread.len(), 2);
    });
}

/// Discovery re-homes a classpath entry pointing outside the declaring project to a synthesized
/// `external-classpath-<n>/<name>`, which is where the bytes went and not a file anybody wrote. A
/// diagnostic names the entry the way its manifest spells it.
#[test]
fn an_unreadable_classpath_entry_is_named_as_the_manifest_spelled_it() {
    jals_exec::block_on_inline(async {
        let (root, storage) = publishing_dependency(
            "navigation",
            &[
                (
                    "dep/jals.toml",
                    &manifest_with_classpath(&["../shared/broken.jar"]),
                ),
                ("shared/broken.jar", b"not a zip archive"),
            ],
        );
        let diagnosis = only_diagnosis(&root, &storage).await;

        let [unread] = diagnosis.unread.as_slice() else {
            panic!(
                "expected exactly one unread entry, got {:?}",
                diagnosis.unread
            );
        };
        let jals_classpath::WarningOrigin::External(locator) = &unread.origin else {
            panic!(
                "expected the manifest's own spelling, got {:?}",
                unread.origin
            );
        };
        assert_eq!(locator.to_string(), "../shared/broken.jar");
    });
}

/// A destination *at* a source root rather than below it has no package to be addressed by, which
/// `package_prefix` rejects before the check is ever reached. Pinned so the coverage fold is never
/// what a reader is shown for it.
#[test]
fn a_publication_at_a_source_root_is_rejected_before_the_check() {
    jals_exec::block_on_inline(async {
        let sources = jar(&[("net/example/Api.java", b"package net.example; class Api {}")]);
        let (root, storage) = task_dependency(
            r#"
                let archive = tasks.project_jar("vendor/sources.jar");
                let tree = tasks.extract_java(archive, "net/example");
                tasks.publish_tree("api", tree, "src/main/java", "replace-root", "navigation");
            "#,
            &[("dep/vendor/sources.jar", &sources)],
        );
        let mut cache = MemoryStorage::memory(CodeTree::default());

        let error = MemoryProjectGraph::discover(&root, &storage.view())
            .await
            .unwrap()
            .preprocess(cache.artifacts_mut(), inert!())
            .await
            .unwrap_err();

        let GraphError::BuildScript { message, .. } = &error else {
            panic!("expected a build-script error, got {error:?}");
        };
        assert!(message.contains("source-dirs"), "{message}");
    });
}

/// This is a consumer-side check, and deliberately only that. Discovery gives the root project no
/// node — its script is the host's to run — so a library's author never meets this warning building
/// their own repository, only whoever depends on them does.
#[test]
fn the_root_project_is_never_diagnosed() {
    jals_exec::block_on_inline(async {
        let sources = jar(&[("net/example/Api.java", b"package net.example; class Api {}")]);
        let storage = MemoryStorage::memory(
            CodeTree::new(
                [
                    (
                        "build.rhai",
                        r#"
                            let archive = tasks.project_jar("vendor/sources.jar");
                            let tree = tasks.extract_java(archive, "net/example");
                            tasks.publish_tree("api", tree, "src/main/java/net/example",
                                               "replace-root", "navigation");
                        "#
                        .as_bytes()
                        .to_vec(),
                    ),
                    ("vendor/sources.jar", sources),
                    ("src/main/java/Seed.java", b"class Seed {}".to_vec()),
                ]
                .into_iter()
                .map(|(path, bytes)| Entry::File(FileKey::parse(path).unwrap(), bytes)),
            )
            .unwrap(),
        );
        // The same publication a dependency would be diagnosed for, declared by the root itself.
        let root = manifest("[build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n");

        assert!(publication_diagnoses(&root, &storage).await.is_empty());
    });
}

/// A coverage answer is recorded so an editor reload does not re-digest the classpath to re-derive
/// it — and the classpath is part of what the record is keyed on, so putting the missing jar there
/// answers differently rather than serving the old answer back.
#[test]
fn a_recorded_coverage_answer_is_reused_and_a_classpath_edit_invalidates_it() {
    jals_exec::block_on_inline(async {
        let mut cache = MemoryStorage::memory(CodeTree::default());
        let (root, unbacked) = publishing_dependency("navigation", &[]);

        // Twice into one cache: the second run reads the record the first one wrote, and has to
        // reach the same conclusion from it.
        let first = publication_diagnoses_in(&root, &unbacked, &mut cache).await;
        let second = publication_diagnoses_in(&root, &unbacked, &mut cache).await;
        assert_eq!(first.len(), 1);
        assert_eq!(first, second);

        // Same node, same plan, same features — only `[build] classpath` differs, and that is
        // enough for the recorded answer not to apply.
        let classes = jar(&[("net/example/Api.class", b"class bytes")]);
        let (root, backed) = publishing_dependency(
            "navigation",
            &[
                (
                    "dep/jals.toml",
                    &manifest_with_classpath(&["vendor/lib.jar"]),
                ),
                ("dep/vendor/lib.jar", &classes),
            ],
        );
        assert!(
            publication_diagnoses_in(&root, &backed, &mut cache)
                .await
                .is_empty()
        );
    });
}
