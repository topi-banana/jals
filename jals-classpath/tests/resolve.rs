//! Resolving `[dependencies]` to local jar paths: `file://`/path sources resolve directly, a missing
//! local jar is a warning (not a failure), and an already-cached remote jar is reused without any
//! network access. The actual download path is exercised by the `#[ignore]`d localhost test, kept out
//! of CI so the suite stays hermetic.

use std::io::Write;
use std::path::{Path, PathBuf};

use jals_build::DependencySource;
use jals_classpath::DepsCache;

/// Build a tiny but real (deflated) jar at `path`, like `load.rs` does, so resolved jars are
/// loadable end-to-end.
fn write_jar(path: &Path) {
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("com/example/Box.class", options).unwrap();
    zip.write_all(b"\xca\xfe\xba\xbe placeholder").unwrap();
    zip.finish().unwrap();
}

#[test]
fn file_url_resolves_to_existing_jar() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("dep.jar");
    write_jar(&jar);

    // A `file://`-classified source resolves to a `Path`; resolution confirms it exists and passes
    // it through verbatim (loading it is `load_classpath`'s job).
    let sources = vec![("dep".to_owned(), DependencySource::Path(jar.clone()))];
    let resolved = DepsCache::resolve_dependencies(&sources, &dir.path().join("cache"));
    assert_eq!(resolved.jars, vec![jar]);
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
}

#[test]
fn missing_file_jar_is_a_warning_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let missing = PathBuf::from("/no/such/dep.jar");
    let sources = vec![("dep".to_owned(), DependencySource::Path(missing))];

    let resolved = DepsCache::resolve_dependencies(&sources, &dir.path().join("cache"));
    assert!(resolved.jars.is_empty());
    assert_eq!(resolved.warnings.len(), 1);
    assert!(resolved.warnings[0].message.contains("does not exist"));
}

#[test]
fn cache_hit_skips_download() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("cache");
    let url = "https://example.invalid/lib.jar";

    // Pre-seed the cache at exactly the path the resolver computes, with non-empty contents. The
    // resolver must return that path through the skip-if-exists branch, never touching the network
    // (the URL is unreachable, so a real request would error and warn instead).
    let cached = DepsCache::cached_jar_path("lib", url, &cache);
    std::fs::create_dir_all(&cache).unwrap();
    write_jar(&cached);

    let sources = vec![("lib".to_owned(), DependencySource::Url(url.to_owned()))];
    let resolved = DepsCache::resolve_dependencies(&sources, &cache);
    assert_eq!(resolved.jars, vec![cached]);
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
}

/// A real download from a localhost server. `#[ignore]`d so CI stays network-free; run locally with
/// `cargo test -p jals-classpath -- --ignored`. Uses a hand-rolled `TcpListener` to avoid a dev-dep.
#[test]
#[ignore = "needs a local TCP socket; run with --ignored"]
fn downloads_from_localhost() {
    use std::io::Read;
    use std::net::TcpListener;

    let body = b"jar-bytes";
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        // Drain the request line/headers enough to respond; we don't parse them.
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
    });

    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("cache");
    let url = format!("http://{addr}/lib.jar");
    let sources = vec![("lib".to_owned(), DependencySource::Url(url))];

    let resolved = DepsCache::resolve_dependencies(&sources, &cache);
    handle.join().unwrap();
    assert_eq!(resolved.jars.len(), 1, "{:?}", resolved.warnings);
    assert_eq!(std::fs::read(&resolved.jars[0]).unwrap(), body);
}
