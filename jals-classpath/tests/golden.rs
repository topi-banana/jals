//! The round trip a golden screenshot set makes: packaged into a stored zip on one side, unpacked
//! into a tree of cache artifacts on the other.
//!
//! Both halves are exercised together on purpose. `jals test --update-golden` writes the archive
//! and `jals test --target` reads it, so the two are one contract with a digest in the middle —
//! and a test that only checked the writer would pass while the reader could not open what it
//! produced.

use jals_classpath::{ArchivePackage, FileTreeExtraction, SourceTreeLimits};
use jals_exec::{Exec, block_on_inline};
use jals_storage::{
    ArtifactCache, CacheKey, CacheNamespace, ContentDigest, MemoryCache, RelativePath,
};

fn path(value: &str) -> RelativePath {
    RelativePath::parse(value).expect("a relative path")
}

async fn publish(cache: &mut ArtifactCache<MemoryCache>, bytes: &[u8]) -> CacheKey {
    let key = CacheKey::new(
        CacheNamespace::GoldenScreenshots,
        ContentDigest::of(b"golden fixture"),
        ContentDigest::of(bytes),
    );
    cache.publish(&key, bytes).await.expect("publish");
    key
}

const fn limits() -> SourceTreeLimits {
    SourceTreeLimits {
        max_files: 64,
        max_file_bytes: 1 << 20,
        max_total_bytes: 1 << 22,
    }
}

#[test]
fn an_archive_this_crate_wrote_unpacks_to_what_went_into_it() {
    block_on_inline(async {
        // Bytes that are not text and are not all alike, so a member mix-up cannot pass.
        let entries = vec![
            (path("title.png"), vec![0x89, b'P', b'N', b'G', 1, 2, 3]),
            (path("hud.png"), vec![0xFF, 0x00, 0xFF, 0x00]),
            (path("nested/inventory.png"), vec![7; 300]),
        ];
        let archive = ArchivePackage::write(&entries).expect("packages");

        let exec = Exec::inline();
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let key = publish(&mut cache, &archive).await;
        let tree = FileTreeExtraction::all(&exec, &mut cache, &key, &RelativePath::ROOT, limits())
            .await
            .expect("unpacks");

        // Sorted by path, whatever order they were written in — a golden set is compared by name.
        let names: Vec<String> = tree
            .files
            .iter()
            .map(|file| file.path.to_string())
            .collect();
        assert_eq!(names, ["hud.png", "nested/inventory.png", "title.png"]);

        for file in &tree.files {
            let stored = cache
                .lookup(&file.key)
                .await
                .expect("lookup")
                .expect("every member is published");
            let expected = entries
                .iter()
                .find(|(candidate, _)| candidate == &file.path)
                .map(|(_, bytes)| bytes.clone())
                .expect("the tree names only members that went in");
            assert_eq!(stored, expected, "`{}` came back changed", file.path);
        }
    });
}

#[test]
fn an_archive_with_no_manifest_is_what_a_golden_set_is() {
    block_on_inline(async {
        // The distinction from `JarPackage`: no `META-INF/MANIFEST.MF` is invented, so a consumer
        // comparing by member name has nothing to know to ignore.
        let entries = vec![(path("only.png"), vec![1, 2, 3])];
        let archive = ArchivePackage::write(&entries).expect("packages");
        let exec = Exec::inline();
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let key = publish(&mut cache, &archive).await;
        let tree = FileTreeExtraction::all(&exec, &mut cache, &key, &RelativePath::ROOT, limits())
            .await
            .expect("unpacks");
        assert_eq!(tree.files.len(), 1);
        assert_eq!(tree.files[0].path.to_string(), "only.png");
    });
}

#[test]
fn a_prefix_selects_a_subtree_and_strips_it() {
    block_on_inline(async {
        let entries = vec![
            (path("1.21.11/title.png"), vec![1]),
            (path("1.21.11/hud.png"), vec![2]),
            (path("1.20.1/title.png"), vec![3]),
        ];
        let archive = ArchivePackage::write(&entries).expect("packages");
        let exec = Exec::inline();
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let key = publish(&mut cache, &archive).await;
        let tree = FileTreeExtraction::all(&exec, &mut cache, &key, &path("1.21.11"), limits())
            .await
            .expect("unpacks");
        let names: Vec<String> = tree
            .files
            .iter()
            .map(|file| file.path.to_string())
            .collect();
        assert_eq!(names, ["hud.png", "title.png"]);
    });
}

#[test]
fn an_archive_larger_than_its_limit_is_refused_rather_than_unpacked() {
    block_on_inline(async {
        let entries = vec![(path("big.png"), vec![0; 4096])];
        let archive = ArchivePackage::write(&entries).expect("packages");
        let exec = Exec::inline();
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let key = publish(&mut cache, &archive).await;
        let tight = SourceTreeLimits {
            max_files: 64,
            max_file_bytes: 16,
            max_total_bytes: 16,
        };
        assert!(
            FileTreeExtraction::all(&exec, &mut cache, &key, &RelativePath::ROOT, tight)
                .await
                .is_err(),
            "a member past the cap must not be unpacked"
        );
    });
}
