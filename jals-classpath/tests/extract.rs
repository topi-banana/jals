//! Extracting dependency **sources** jars to disk: only `.java` members are inflated (into a per-jar
//! subdir), extraction is idempotent (a second run reuses the files already on disk), and a corrupt
//! jar is a warning, not a failure.

use std::io::Write;
use std::path::Path;

use jals_classpath::SourcesExtraction;

/// Build a tiny but real (deflated) jar at `path` whose members are the given `(name, content)` pairs,
/// mirroring `write_jar` in `resolve.rs` but for source jars.
fn write_sources_jar(path: &Path, entries: &[(&str, &str)]) {
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, content) in entries {
        zip.start_file(*name, options).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn extracts_only_java_members_to_disk() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("lib-sources.jar");
    write_sources_jar(
        &jar,
        &[
            (
                "java/util/List.java",
                "package java.util; public interface List<E> {}",
            ),
            ("META-INF/MANIFEST.MF", "Manifest-Version: 1.0\n"),
            ("java/util/Map.class", "not actually java"),
        ],
    );

    let dest = dir.path().join("out");
    let extraction = SourcesExtraction::extract_sources(std::slice::from_ref(&jar), &dest);
    assert!(extraction.warnings.is_empty(), "{:?}", extraction.warnings);

    // Only the `.java` member is extracted; the manifest and `.class` are ignored.
    assert_eq!(extraction.java_files.len(), 1);
    let extracted = &extraction.java_files[0];
    assert!(
        extracted.ends_with("java/util/List.java"),
        "unexpected path {}",
        extracted.display()
    );
    assert!(extracted.starts_with(&dest));
    let content = std::fs::read_to_string(extracted).unwrap();
    assert!(content.contains("interface List"));
}

#[test]
fn extraction_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("lib-sources.jar");
    write_sources_jar(&jar, &[("a/B.java", "class B {}")]);
    let dest = dir.path().join("out");

    let first = SourcesExtraction::extract_sources(std::slice::from_ref(&jar), &dest);
    assert_eq!(first.java_files.len(), 1);
    // A second extraction reuses the file already on disk (skip-if-exists), yielding the same path.
    let second = SourcesExtraction::extract_sources(std::slice::from_ref(&jar), &dest);
    assert_eq!(first.java_files, second.java_files);
    assert!(second.warnings.is_empty(), "{:?}", second.warnings);
    assert_eq!(
        std::fs::read_to_string(&second.java_files[0]).unwrap(),
        "class B {}"
    );
}

#[test]
fn corrupt_jar_is_a_warning_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let bogus = dir.path().join("bad-sources.jar");
    std::fs::write(&bogus, b"not a zip archive").unwrap();

    let extraction =
        SourcesExtraction::extract_sources(std::slice::from_ref(&bogus), &dir.path().join("out"));
    assert!(extraction.java_files.is_empty());
    assert_eq!(extraction.warnings.len(), 1);
    assert!(
        extraction.warnings[0].message.contains("sources jar"),
        "{:?}",
        extraction.warnings
    );
}
