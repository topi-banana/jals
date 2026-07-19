//! The streaming side of the codec: parsing through a portable reader must behave exactly like
//! parsing a slice, and source failures must stay distinct from malformed input.

use std::fs::File;
use std::io::BufReader;

use jals_classfile::{ClassFile, ClassfileError};
use jals_exec::block_on_inline;
use jals_storage::io::{IoError, Read, StdReader};

/// Drive the async parse to completion on the calling thread; only the source ever suspends,
/// and every source here completes inline.
fn parse<R: Read>(source: R) -> jals_classfile::Result<ClassFile> {
    block_on_inline(ClassFile::read(source))
}

fn fixture_paths() -> Vec<std::path::PathBuf> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .expect("list fixtures")
        .map(|entry| entry.expect("fixture entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "class"))
        .collect();
    paths.sort();
    paths
}

#[test]
fn buffered_file_reads_match_slice_reads() {
    for path in fixture_paths() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(&path).expect("read fixture");
        let from_slice =
            parse(bytes.as_slice()).unwrap_or_else(|error| panic!("slice parse {name}: {error}"));
        let file = File::open(&path).expect("open fixture");
        let from_stream = parse(StdReader(BufReader::new(file)))
            .unwrap_or_else(|error| panic!("stream parse {name}: {error}"));
        assert_eq!(
            from_slice, from_stream,
            "stream/slice divergence for {name}"
        );
    }
}

#[test]
fn truncation_reports_unexpected_eof() {
    let path = &fixture_paths()[0];
    let bytes = std::fs::read(path).expect("read fixture");
    for len in [0, 3, 8, bytes.len() / 2, bytes.len() - 1] {
        assert!(
            matches!(
                parse(&bytes[..len]),
                Err(ClassfileError::UnexpectedEof { .. })
            ),
            "no EOF error at prefix length {len}"
        );
    }
}

#[test]
fn appended_garbage_reports_trailing_bytes() {
    let path = &fixture_paths()[0];
    let mut bytes = std::fs::read(path).expect("read fixture");
    bytes.push(0);
    assert!(matches!(
        parse(bytes.as_slice()),
        Err(ClassfileError::TrailingBytes)
    ));
}

/// Fails after handing out a valid prefix, as a decompressing archive member does on a corrupt
/// stream.
struct FailAfter {
    prefix: Vec<u8>,
}

impl Read for FailAfter {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        if self.prefix.is_empty() {
            return Err(IoError::Failed(String::from("simulated source failure")));
        }
        let n = self.prefix.len().min(buf.len());
        buf[..n].copy_from_slice(&self.prefix[..n]);
        self.prefix.drain(..n);
        Ok(n)
    }
}

#[test]
fn source_failure_is_not_reported_as_malformed_input() {
    let path = &fixture_paths()[0];
    let bytes = std::fs::read(path).expect("read fixture");
    let source = FailAfter {
        prefix: bytes[..bytes.len() / 2].to_vec(),
    };
    assert!(matches!(
        parse(source),
        Err(ClassfileError::Source(IoError::Failed(_)))
    ));
}

/// An adversarial `u32` attribute length far beyond the actual stream must fail with a bounded
/// allocation, not attempt a multi-gigabyte buffer up front.
#[test]
fn huge_declared_attribute_length_fails_without_a_huge_allocation() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
    bytes.extend_from_slice(&[0, 0, 0, 69]); // minor, major
    bytes.extend_from_slice(&1u16.to_be_bytes()); // constant pool: empty
    bytes.extend_from_slice(&[0, 0]); // access_flags
    bytes.extend_from_slice(&[0, 0, 0, 0]); // this_class, super_class
    bytes.extend_from_slice(&0u16.to_be_bytes()); // interfaces
    bytes.extend_from_slice(&0u16.to_be_bytes()); // fields
    bytes.extend_from_slice(&0u16.to_be_bytes()); // methods
    bytes.extend_from_slice(&1u16.to_be_bytes()); // one attribute...
    bytes.extend_from_slice(&1u16.to_be_bytes()); // ...whose name index is arbitrary
    bytes.extend_from_slice(&u32::MAX.to_be_bytes()); // ...declaring 4 GiB of body
    assert!(matches!(
        parse(bytes.as_slice()),
        Err(ClassfileError::UnexpectedEof { .. })
    ));
}
