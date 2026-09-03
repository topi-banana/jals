//! Packaging compiled class files into a jar.
//!
//! A jar is a zip whose first member is `META-INF/MANIFEST.MF`. The zip half is already here and
//! already hardened — [`StoredZip`] computes crc32s, zeroes the DOS timestamps so identical inputs
//! produce identical bytes, and refuses unsafe or duplicate member names — and the manifest half is
//! [`crate::manifest`]'s, so what this module owns is the *assembly*: which member leads, and that
//! an entry cannot supply a manifest the packager writes itself.
//!
//! It is also the crate's only route to the zip writer. [`JarPackage::write`] packages a compile's
//! output; [`JarPackage::write_members`] serializes members somebody else assembled — a remap's
//! rewritten archive, a merge's union — and both put the manifest first, because
//! `JarInputStream::getManifest` reads the first member and no other. A second caller reaching
//! `StoredZip` directly is a second place that has to remember that.
//!
//! Portable like the rest of `archive`: an in-browser compile packages its own output with no host
//! archiver involved.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use jals_storage::RelativePath;

use crate::manifest::{Manifest, MetaInf};
use crate::zip::{StoredZip, WriteMember};

/// Namespace for packaging compiled classes into a jar.
pub struct JarPackage;

impl JarPackage {
    /// Package compiled class files into a deterministic, stored-only jar.
    ///
    /// `entries` are `(project-relative path, bytes)` — the shape a compile backend hands its
    /// output over in — packaged in the order given. `META-INF/MANIFEST.MF` is written first
    /// because `java -jar` and `java.util.jar.JarInputStream` both expect to find it before the
    /// classes, and `main_class`, when given, becomes its `Main-Class`. Without one the result is a
    /// library jar, which is the honest output for a project that declares no entry point.
    ///
    /// # Errors
    /// Returns a message when an entry claims the manifest's name, and passes through the writer's
    /// own refusals — an unsafe member name, two entries sharing a path, or an archive beyond what
    /// the stored encoding's 32-bit fields can describe.
    pub fn write(
        entries: &[(RelativePath, Vec<u8>)],
        main_class: Option<&str>,
    ) -> Result<Vec<u8>, String> {
        let mut members = Vec::with_capacity(entries.len() + 1);
        members.push(WriteMember {
            name: MetaInf::MANIFEST_PATH.to_owned(),
            bytes: Manifest::packaged(main_class),
        });
        for (path, bytes) in entries {
            let name = path.to_string();
            // Caught here rather than left to the writer's duplicate check so the message names the
            // actual cause: the manifest is generated, not something a caller supplies. Recognised
            // the way a JVM recognises it, so an entry spelling it `meta-inf/manifest.mf` is the
            // same conflict — the writer would take it as a distinct path, and `JarFile` would then
            // have two manifests to choose between.
            if MetaInf::is_manifest(&name) {
                return Err(format!(
                    "`{name}` is the jar manifest, which the packager writes itself \
                     (`{}`) and an entry cannot also supply",
                    MetaInf::MANIFEST_PATH
                ));
            }
            members.push(WriteMember {
                name,
                bytes: bytes.clone(),
            });
        }
        Self::write_members(members)
    }

    /// Serialize members somebody else assembled as a jar, with the manifest first.
    ///
    /// The hoist is why this exists rather than a second `StoredZip::write` call site. A remap
    /// preserves its input's member order and a merge walks the overlay and then the base, so
    /// neither ends up with the manifest first by construction — and a base with no manifest of its
    /// own takes the overlay's out of the union's tail. `JarInputStream::getManifest` reads none but
    /// the first member, so a streaming reader would then see no manifest at all, and with it none
    /// of what a merge writes into one.
    ///
    /// Only the *first* manifest moves. A single input carrying two of its own keeps both, exactly
    /// as it did: deduplicating them would be this writer inventing a conflict its caller did not
    /// have.
    ///
    /// # Errors
    /// Passes through the writer's refusals: an unsafe member name, two members sharing a path, or
    /// an archive beyond what the stored encoding's 32-bit fields can describe.
    pub(crate) fn write_members(mut members: Vec<WriteMember>) -> Result<Vec<u8>, String> {
        if let Some(position) = members
            .iter()
            .position(|member| MetaInf::is_manifest(&member.name))
            && position != 0
        {
            members[..=position].rotate_right(1);
        }
        StoredZip::write(&members)
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

    fn member(name: &str, bytes: &[u8]) -> WriteMember {
        WriteMember {
            name: name.to_owned(),
            bytes: bytes.to_vec(),
        }
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

    fn member_names(archive: &[u8]) -> Vec<String> {
        block_on_inline(CentralDirectory::parse(&mut Cursor::new(archive)))
            .expect("the jar parses")
            .members
            .iter()
            .map(|member| member.name.clone())
            .collect()
    }

    fn manifest_text(archive: &[u8]) -> String {
        let mut oracle =
            zip::ZipArchive::new(std::io::Cursor::new(archive)).expect("oracle opens the jar");
        let mut reader = oracle
            .by_name(MetaInf::MANIFEST_PATH)
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
        let archive =
            JarPackage::write(&entries, Some("com.example.Main")).expect("packaging succeeds");

        let directory = block_on_inline(CentralDirectory::parse(&mut Cursor::new(&archive)))
            .expect("the jar parses");
        assert_eq!(directory.members.len(), entries.len() + 1);
        assert_eq!(directory.members[0].name, MetaInf::MANIFEST_PATH);
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

    /// A project with no entry point still packages — as a library jar, without `Main-Class`.
    #[test]
    fn a_library_jar_omits_main_class() {
        let archive = JarPackage::write(&[(path("A.class"), b"x".to_vec())], None)
            .expect("packaging succeeds");
        assert_eq!(manifest_text(&archive), "Manifest-Version: 1.0\r\n\r\n");
    }

    /// The same classes always package to the same bytes: nothing here reads a clock.
    #[test]
    fn packaging_is_deterministic() {
        let entries = vec![
            (path("com/example/Main.class"), b"main".to_vec()),
            (path("com/example/Greeter.class"), b"greeter".to_vec()),
        ];
        let first = JarPackage::write(&entries, Some("com.example.Main")).expect("first write");
        let second = JarPackage::write(&entries, Some("com.example.Main")).expect("second write");
        assert_eq!(first, second);
    }

    /// The packager writes the manifest, so an entry claiming that name is a conflict — however the
    /// entry spells it, because a JVM finds a manifest either way and the writer would keep both.
    #[test]
    fn an_entry_named_like_the_manifest_is_rejected() {
        for name in [MetaInf::MANIFEST_PATH, "meta-inf/manifest.mf"] {
            let entries = vec![(path(name), b"forged".to_vec())];
            let error = JarPackage::write(&entries, None).expect_err("the conflict is reported");
            assert!(error.contains(name), "message must name the entry: {error}");
        }
    }

    /// Two sources declaring the same type produce the same artifact path; the writer's duplicate
    /// refusal has to reach the caller rather than silently keeping one.
    #[test]
    fn duplicate_entry_paths_are_rejected() {
        let entries = vec![
            (path("com/example/Main.class"), b"one".to_vec()),
            (path("com/example/Main.class"), b"two".to_vec()),
        ];
        assert!(JarPackage::write(&entries, None).is_err());
    }

    /// A remap and a merge hand over members in *their* order, and the manifest need not be first
    /// in it. `JarInputStream::getManifest` reads none but the first member, so a jar assembled
    /// that way would answer a streaming reader with no manifest at all.
    #[test]
    fn assembled_members_are_written_with_the_manifest_first() {
        let archive = JarPackage::write_members(vec![
            member("a/B.class", b"one"),
            member("meta-inf/manifest.mf", b"Manifest-Version: 1.0\r\n\r\n"),
            member("a/C.class", b"two"),
        ])
        .expect("assembly succeeds");
        assert_eq!(
            member_names(&archive),
            ["meta-inf/manifest.mf", "a/B.class", "a/C.class"]
        );

        // Only the hoist: every other member keeps the order it was handed over in, and a jar whose
        // manifest is already first is written exactly as it came.
        let unchanged = vec![
            member(MetaInf::MANIFEST_PATH, b"Manifest-Version: 1.0\r\n\r\n"),
            member("a/B.class", b"one"),
        ];
        assert_eq!(
            JarPackage::write_members(unchanged.clone()).expect("assembly succeeds"),
            JarPackage::write_members(unchanged).expect("assembly succeeds")
        );

        // A single input carrying two manifests keeps both, in order: deduplicating them would be
        // this writer inventing a conflict its caller did not have.
        let archive = JarPackage::write_members(vec![
            member("a/B.class", b"one"),
            member("META-INF/MANIFEST.MF", b"first"),
            member("meta-inf/manifest.mf", b"second"),
        ])
        .expect("assembly succeeds");
        assert_eq!(
            member_names(&archive),
            ["META-INF/MANIFEST.MF", "a/B.class", "meta-inf/manifest.mf"]
        );
    }
}
