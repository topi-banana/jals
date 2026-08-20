//! Packaging compiled class files into a jar.
//!
//! A jar is a zip whose first member is `META-INF/MANIFEST.MF`. The zip half is already here and
//! already hardened — [`StoredZip`] computes crc32s, zeroes the DOS timestamps so identical inputs
//! produce identical bytes, and refuses unsafe or duplicate member names — so this module owns only
//! the manifest: its required first attribute, its CRLF lines, its 72-byte line cap, and the blank
//! line that terminates the main section.
//!
//! Portable like the rest of `archive`: an in-browser compile packages its own output with no host
//! archiver involved.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use jals_storage::RelativePath;

use crate::zip::WriteMember;

/// The manifest's physical line cap, in bytes, counted here *including* the `\r\n` terminator.
///
/// The jar specification caps a line at 72 bytes and is read both ways on whether the terminator
/// counts against it. Counting it is the strict reading, so a manifest this writer produces is
/// legal under either.
const MAX_LINE: usize = 72;

/// Manifest lines end with CRLF, not the host's line ending — the format is not text-mode.
const EOL: &str = "\r\n";

pub use api::write;

/// Namespace for packaging compiled classes into a jar.
mod api {
    use super::{EOL, MAX_LINE, RelativePath, String, ToOwned, ToString, Vec, WriteMember, format};

    /// The manifest member every jar carries, and the first member this writer emits.
    ///
    /// Private: a caller never supplies this path — it is generated — and the one place a caller
    /// could collide with it names it in the error message instead.
    pub(super) const MANIFEST_PATH: &str = "META-INF/MANIFEST.MF";

    /// Package compiled class files into a deterministic, stored-only jar.
    ///
    /// `entries` are `(project-relative path, bytes)` — the shape a compile backend hands its
    /// output over in — packaged in the order given. `META-INF/MANIFEST.MF` is written first
    /// because `java -jar` and `java.util.jar.JarInputStream` both expect to find it before the
    /// classes, and `main_class`, when given, becomes its `Main-Class`. Without one the result is a
    /// library jar, which is the honest output for a project that declares no entry point.
    ///
    /// No `Created-By`: naming the writing tool's version would change the bytes on every release,
    /// and the archive is otherwise reproducible from its inputs alone.
    ///
    /// # Errors
    /// Returns a message when an entry claims `META-INF/MANIFEST.MF`, and passes through the
    /// writer's own refusals — an unsafe member name, two entries sharing a path, or an archive
    /// beyond what the stored encoding's 32-bit fields can describe.
    pub fn write(
        entries: &[(RelativePath, Vec<u8>)],
        main_class: Option<&str>,
    ) -> Result<Vec<u8>, String> {
        let mut members = Vec::with_capacity(entries.len() + 1);
        members.push(WriteMember {
            name: MANIFEST_PATH.to_owned(),
            bytes: main_section(main_class).into_bytes(),
        });
        for (path, bytes) in entries {
            let name = path.to_string();
            // Caught here rather than left to the writer's duplicate check so the message names the
            // actual cause: the manifest is generated, not something a caller supplies.
            if name == MANIFEST_PATH {
                return Err(format!(
                    "`{MANIFEST_PATH}` is written by the packager and cannot also be a packaged entry"
                ));
            }
            members.push(WriteMember {
                name,
                bytes: bytes.clone(),
            });
        }
        crate::zip::write(&members)
    }

    /// Render the jar manifest's main section.
    ///
    /// `Manifest-Version` comes first because the specification requires the version to be the
    /// main section's first attribute; a blank line terminates the section.
    pub(crate) fn main_section(main_class: Option<&str>) -> String {
        let mut out = String::new();
        write_attribute(&mut out, "Manifest-Version", "1.0");
        if let Some(main_class) = main_class {
            write_attribute(&mut out, "Main-Class", main_class);
        }
        out.push_str(EOL);
        out
    }

    /// Append `name: value` to `out` as one or more physical manifest lines.
    ///
    /// A value too long for one line continues on following lines that each begin with exactly one
    /// space. Every physical line, terminator included, stays within [`MAX_LINE`] bytes, and the
    /// split never lands inside a UTF-8 sequence — `str::floor_char_boundary` is unstable and this
    /// crate is `no_std`, so the boundary is walked back by hand. Only a deeply nested `Main-Class`
    /// ever reaches the wrapping path, but an unwrapped over-long line is an invalid manifest, not
    /// a cosmetic issue.
    pub(crate) fn write_attribute(out: &mut String, name: &str, value: &str) {
        let mut line = String::with_capacity(name.len() + 2 + value.len());
        line.push_str(name);
        line.push_str(": ");
        line.push_str(value);

        let mut rest = line.as_str();
        // The first physical line spends its whole budget on content; a continuation gives one
        // byte back to the leading space that marks it as one.
        let mut budget = MAX_LINE - EOL.len();
        loop {
            if rest.len() <= budget {
                out.push_str(rest);
                out.push_str(EOL);
                return;
            }
            let mut take = budget;
            while take > 0 && !rest.is_char_boundary(take) {
                take -= 1;
            }
            if take == 0 {
                // Unreachable while the budget exceeds four bytes, but emitting one whole
                // character keeps the loop total rather than spinning on a zero-length split.
                take = rest.chars().next().map_or(rest.len(), char::len_utf8);
            }
            let (head, tail) = rest.split_at(take);
            out.push_str(head);
            out.push_str(EOL);
            out.push(' ');
            rest = tail;
            budget = MAX_LINE - EOL.len() - 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use jals_exec::block_on_inline;
    use jals_storage::io::{Cursor, Read as _};

    use super::*;
    use crate::zip::{CentralDirectory, MemberRecord, MemberStream};

    fn path(text: &str) -> RelativePath {
        RelativePath::parse(text).expect("test path is valid")
    }

    fn read_member(archive: &[u8], member: &MemberRecord) -> Vec<u8> {
        block_on_inline(async {
            let mut stream = MemberStream::open(Cursor::new(archive), member)
                .await
                .expect("member opens");
            let mut out = Vec::new();
            let mut chunk = [0u8; 173]; // odd size to exercise partial reads
            loop {
                match stream.read(&mut chunk).await.expect("member reads") {
                    0 => return out,
                    n => out.extend_from_slice(&chunk[..n]),
                }
            }
        })
    }

    fn manifest_text(archive: &[u8]) -> String {
        let mut oracle =
            zip::ZipArchive::new(std::io::Cursor::new(archive)).expect("oracle opens the jar");
        let mut reader = oracle
            .by_name(api::MANIFEST_PATH)
            .expect("the jar carries a manifest");
        let mut text = String::new();
        std::io::Read::read_to_string(&mut reader, &mut text).expect("the manifest is text");
        text
    }

    /// The manifest leads, every class round-trips through the in-house reader, and the `zip`
    /// oracle independently agrees on the manifest's exact bytes.
    #[test]
    fn a_jar_leads_with_its_manifest_and_reads_back() {
        let entries = vec![
            (
                path("com/example/Main.class"),
                b"\xca\xfe\xba\xbemain".to_vec(),
            ),
            (
                path("com/example/Greeter.class"),
                b"\xca\xfe\xba\xbegreeter".to_vec(),
            ),
        ];
        let archive = api::write(&entries, Some("com.example.Main")).expect("packaging succeeds");

        let directory = block_on_inline(CentralDirectory::parse(&mut Cursor::new(&archive)))
            .expect("the jar parses");
        assert_eq!(directory.members.len(), entries.len() + 1);
        assert_eq!(directory.members[0].name, api::MANIFEST_PATH);
        // Input order is preserved after the manifest, so the members line up with `entries`.
        for (member, (entry_path, bytes)) in directory.members[1..].iter().zip(&entries) {
            assert_eq!(member.name, entry_path.to_string());
            assert_eq!(&read_member(&archive, member), bytes);
        }

        assert_eq!(
            manifest_text(&archive),
            "Manifest-Version: 1.0\r\nMain-Class: com.example.Main\r\n\r\n"
        );
    }

    /// A `Main-Class` longer than one manifest line has to continue on wrapped lines, or the
    /// manifest is invalid and `java -jar` refuses the archive.
    #[test]
    fn a_long_main_class_wraps_within_the_line_budget() {
        let mut fqcn = String::from("com.example");
        while fqcn.len() < 200 {
            fqcn.push_str(".deeply");
        }
        fqcn.push_str(".Main");
        let archive = api::write(&[], Some(&fqcn)).expect("packaging succeeds");
        let text = manifest_text(&archive);

        // Drop the blank line that terminates the main section; what remains is the attributes.
        let body = text.strip_suffix(EOL).expect("the section is terminated");
        let lines: Vec<&str> = body.split_terminator(EOL).collect();
        for line in &lines {
            assert!(
                line.len() + EOL.len() <= MAX_LINE,
                "physical line exceeds the budget: {line:?}"
            );
        }
        // Everything after `Manifest-Version` belongs to the wrapped attribute: the first of those
        // lines starts it, each later one continues it with exactly one leading space.
        let attribute = &lines[1..];
        let mut joined = String::from(attribute[0]);
        for line in &attribute[1..] {
            assert!(
                line.starts_with(' ') && !line[1..].starts_with(' '),
                "continuation must start with exactly one space: {line:?}"
            );
            joined.push_str(&line[1..]);
        }
        assert_eq!(joined, format!("Main-Class: {fqcn}"));
    }

    /// The wrap walks back to a character boundary, so a multibyte value cannot be split into
    /// invalid UTF-8 — the manifest is read as text on the other side.
    #[test]
    fn a_wrapped_value_never_splits_a_utf8_sequence() {
        // Three-byte characters land astride every candidate split point.
        let value: String = core::iter::repeat_n('あ', 60).collect();
        let mut out = String::new();
        api::write_attribute(&mut out, "Main-Class", &value);

        let mut joined = String::new();
        for (index, line) in out.split_terminator(EOL).enumerate() {
            joined.push_str(if index == 0 { line } else { &line[1..] });
        }
        assert_eq!(joined, format!("Main-Class: {value}"));
    }

    /// A project with no entry point still packages — as a library jar, without `Main-Class`.
    #[test]
    fn a_library_jar_omits_main_class() {
        let archive =
            api::write(&[(path("A.class"), b"x".to_vec())], None).expect("packaging succeeds");
        assert_eq!(manifest_text(&archive), "Manifest-Version: 1.0\r\n\r\n");
    }

    /// The same classes always package to the same bytes: nothing here reads a clock.
    #[test]
    fn packaging_is_deterministic() {
        let entries = vec![
            (path("com/example/Main.class"), b"main".to_vec()),
            (path("com/example/Greeter.class"), b"greeter".to_vec()),
        ];
        let first = api::write(&entries, Some("com.example.Main")).expect("first write");
        let second = api::write(&entries, Some("com.example.Main")).expect("second write");
        assert_eq!(first, second);
    }

    /// The packager writes the manifest, so an entry claiming that path is a conflict — and the
    /// message has to name it rather than surface as a generic duplicate.
    #[test]
    fn an_entry_named_like_the_manifest_is_rejected() {
        let entries = vec![(path(api::MANIFEST_PATH), b"forged".to_vec())];
        let error = api::write(&entries, None).expect_err("the conflict is reported");
        assert!(
            error.contains(api::MANIFEST_PATH),
            "message must name the manifest: {error}"
        );
    }

    /// Two sources declaring the same type produce the same artifact path; the writer's duplicate
    /// refusal has to reach the caller rather than silently keeping one.
    #[test]
    fn duplicate_entry_paths_are_rejected() {
        let entries = vec![
            (path("com/example/Main.class"), b"one".to_vec()),
            (path("com/example/Main.class"), b"two".to_vec()),
        ];
        assert!(api::write(&entries, None).is_err());
    }
}
