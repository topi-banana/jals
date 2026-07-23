#![cfg(feature = "native")]

use std::fs;
use std::path::Path;
use std::process::Command;

use jals_build::build_script::{BuildScriptEnvironment, BuildScriptLimits};
use jals_classpath::{DependencyLocation, ProjectInputOptions};
use jals_config::{Manifest, ResolvedBuildFeatures};
use jals_exec::Exec;
use jals_project::{
    CompileClasspathEntry, GraphError, GraphPreprocess, MemoryProjectGraph, NativeProjectGraph,
    NodeKind,
};
use jals_storage::{CodeTree, Entry, FileKey, MemoryStorage, NativeStorage, RelativePath};

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
        let assembly = graph
            .assemble_native(
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
        let analysis = graph
            .assemble_native(
                &root,
                project.path(),
                &mut root_storage,
                ProjectInputOptions::Analysis,
            )
            .await;
        assert_eq!(
            analysis.watch_paths,
            [fs::canonicalize(project.path().join("dep")).unwrap()]
        );
        assert!(analysis.inputs.source_dep_sources.is_empty());
        assert_eq!(analysis.plan.source_dependency_artifacts.len(), 1);

        let editor = graph
            .assemble_native(
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
        std::fs::write(
            project
                .path()
                .join("warn")
                .join(OsString::from_vec(vec![b'b', b'a', b'd', 0xff])),
            b"ignored",
        )
        .unwrap();
        let warning_root = manifest("[dependencies]\nwarn = { path = \"warn\" }\n");
        let graph = NativeProjectGraph::discover(
            &warning_root,
            project.path(),
            &exec,
            jals_classpath::NetworkPolicy::Online,
        )
        .await
        .unwrap();
        assert!(
            graph
                .warnings()
                .iter()
                .any(|warning| warning.message.contains("NonUtf8Entry"))
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
                tasks.publish_tree("api", tree, "src/main/java/net/example", "replace-root");
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
                tasks.publish_tree("api", tree, "generated/net/example", "replace-root");
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
                tasks.publish_tree("api", tree, "src/main/java/net/example", "replace-root");
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
