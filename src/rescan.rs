//! The rescan timer: re-walks the configured media directories on a
//! schedule and replaces the shared index in place, so new/removed files
//! show up without a restart. A full rescan rebuilds the whole `Index`
//! from scratch and swaps it in wholesale (`SharedIndex::replace`) rather
//! than incrementally patching the old one — simpler, and matches "MVP:
//! in-memory index, rebuilt on scan" from the private planning notes.
//! Object IDs are reassigned each rescan as a result; nothing currently
//! depends on an ID staying stable across rescans (Browse is always a
//! fresh round-trip), so this is an accepted MVP tradeoff, not an
//! oversight.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::config::{MediaConfig, TagCacheConfig};
use crate::core::metadata_provider::MetadataProvider;
use crate::index::SharedIndex;
use crate::metadata::tag_cache::CachedTagMetadata;
use crate::metadata::tags::TagMetadata;
use crate::scanner;

/// Builds the `MetadataProvider` the rescan loop uses, per
/// `[tag_cache]`: a persistent `CachedTagMetadata` when enabled and its
/// database opens successfully, a plain `TagMetadata` otherwise. Opening
/// the cache is an optional speedup, never a correctness requirement -
/// a failure here is logged and degrades to the uncached path, not a
/// startup failure. `MetadataProvider` is `pub(crate)` (see
/// `core::metadata_provider`), so this lives here rather than in
/// `main.rs`: the caller only ever holds the opaque `Arc` this returns,
/// inferred through this function's own return type, and never needs
/// to name the trait itself.
pub fn build_tags(
    tag_cache: &TagCacheConfig,
    media_roots: Vec<PathBuf>,
) -> Arc<dyn MetadataProvider> {
    if !tag_cache.enabled {
        return Arc::new(TagMetadata::new(media_roots));
    }
    match CachedTagMetadata::open(&tag_cache.path, TagMetadata::new(media_roots.clone())) {
        Ok(cached) => {
            log::info!("tag cache enabled at {}", tag_cache.path.display());
            Arc::new(cached)
        }
        Err(err) => {
            log::warn!(
                "failed to open tag cache at {}: {err} - continuing without it",
                tag_cache.path.display()
            );
            Arc::new(TagMetadata::new(media_roots))
        }
    }
}

/// Runs forever: scans once immediately if `on_startup`, then re-scans
/// every `interval`. Each scan runs on a blocking thread — directory
/// walking is real filesystem I/O, and a large library shouldn't stall
/// the async runtime while it's being walked. `tags` is decided once by
/// the caller (a plain `TagMetadata`, or a `CachedTagMetadata` wrapping
/// one, per `[tag_cache]`) and shared via `Arc` across every tick, so a
/// persistent cache's database file is opened once for the process's
/// whole lifetime, not reopened on every scan.
pub async fn run(
    media: MediaConfig,
    index: SharedIndex,
    interval: Duration,
    on_startup: bool,
    tags: Arc<dyn MetadataProvider>,
) {
    if on_startup {
        rescan_once(&media, &index, &tags).await;
    }
    loop {
        tokio::time::sleep(interval).await;
        rescan_once(&media, &index, &tags).await;
    }
}

async fn rescan_once(media: &MediaConfig, index: &SharedIndex, tags: &Arc<dyn MetadataProvider>) {
    let media = media.clone();
    let tags = Arc::clone(tags);
    let result = tokio::task::spawn_blocking(move || {
        let new_index = scanner::scan(&media, tags.as_ref());
        // Drops any cache entry for a file no longer in the library - a
        // no-op for a stateless provider (FilenameMetadata/TagMetadata),
        // real work for a persistent cache. See
        // `core::metadata_provider::MetadataProvider::retain_only`.
        tags.retain_only(&new_index.item_paths());
        new_index
    })
    .await;
    match result {
        Ok(new_index) => {
            log::info!("rescan complete: {} entries indexed", new_index.len());
            index.replace(new_index);
        }
        Err(err) => log::error!("rescan task panicked: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MediaDirectory, MediaKind};
    use crate::index::{IndexBuilder, ObjectId};
    use crate::metadata::filename::FilenameMetadata;

    fn media_config(root: &std::path::Path) -> MediaConfig {
        MediaConfig {
            directories: vec![MediaDirectory {
                path: root.to_path_buf(),
                kind: MediaKind::Audio,
            }],
            follow_symlinks: false,
            exclude_patterns: vec![],
        }
    }

    #[tokio::test]
    async fn rescan_once_replaces_the_shared_index_with_a_fresh_scan() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("one.mp3"), b"x").unwrap();
        let media = media_config(dir.path());

        // Starts from an unrelated, empty index - rescan_once should
        // wholesale replace it with what's actually on disk.
        let shared = SharedIndex::new(IndexBuilder::new().build());
        let tags: Arc<dyn MetadataProvider> = Arc::new(FilenameMetadata);
        rescan_once(&media, &shared, &tags).await;

        let root_children = shared.children(&ObjectId::root()).unwrap();
        assert_eq!(
            root_children.len(),
            1,
            "expected one top-level container for the configured directory"
        );

        let top_id = root_children[0].id().clone();
        let files = shared.children(&top_id).unwrap();
        assert_eq!(files.len(), 1);
    }

    #[tokio::test]
    async fn run_scans_immediately_when_on_startup_is_true() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("one.mp3"), b"x").unwrap();
        let media = media_config(dir.path());
        let shared = SharedIndex::new(IndexBuilder::new().build());
        let tags: Arc<dyn MetadataProvider> = Arc::new(FilenameMetadata);

        let handle = tokio::spawn(run(
            media,
            shared.clone(),
            Duration::from_secs(3600),
            true,
            tags,
        ));

        // Give the spawned task a moment to run its first (on_startup)
        // scan - it runs immediately, no sleep to wait out.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(shared.children(&ObjectId::root()).unwrap().len(), 1);

        handle.abort();
    }

    /// Records every `retain_only` call it receives, so a test can
    /// assert exactly what `rescan_once` passes it - without needing a
    /// real persistent cache to observe the wiring.
    #[derive(Default)]
    struct RetainSpy {
        seen: std::sync::Mutex<Vec<Vec<std::path::PathBuf>>>,
    }

    impl MetadataProvider for RetainSpy {
        fn metadata(&self, path: &std::path::Path) -> crate::core::metadata_provider::Metadata {
            FilenameMetadata.metadata(path)
        }

        fn retain_only(&self, live_paths: &[std::path::PathBuf]) {
            self.seen.lock().unwrap().push(live_paths.to_vec());
        }
    }

    #[tokio::test]
    async fn rescan_once_prunes_the_cache_to_only_the_files_still_present() {
        let dir = tempfile::tempdir().unwrap();
        let kept = dir.path().join("kept.mp3");
        let removed = dir.path().join("removed.mp3");
        std::fs::write(&kept, b"x").unwrap();
        std::fs::write(&removed, b"x").unwrap();
        let media = media_config(dir.path());

        let spy = Arc::new(RetainSpy::default());
        let tags: Arc<dyn MetadataProvider> = spy.clone();
        let shared = SharedIndex::new(IndexBuilder::new().build());

        rescan_once(&media, &shared, &tags).await;
        std::fs::remove_file(&removed).unwrap();
        rescan_once(&media, &shared, &tags).await;

        let seen = spy.seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "retain_only should run after every scan");

        let mut first_call = seen[0].clone();
        first_call.sort();
        let mut both_files = vec![kept.clone(), removed.clone()];
        both_files.sort();
        assert_eq!(first_call, both_files, "the first scan found both files");

        assert_eq!(
            seen[1],
            vec![kept.clone()],
            "the second scan's retain_only call must exclude the deleted file"
        );
    }

    #[tokio::test]
    async fn run_does_not_scan_before_the_first_interval_when_on_startup_is_false() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("one.mp3"), b"x").unwrap();
        let media = media_config(dir.path());
        let shared = SharedIndex::new(IndexBuilder::new().build());
        let tags: Arc<dyn MetadataProvider> = Arc::new(FilenameMetadata);

        let handle = tokio::spawn(run(
            media,
            shared.clone(),
            Duration::from_secs(3600),
            false,
            tags,
        ));

        tokio::time::sleep(Duration::from_millis(50)).await;
        // Still the original empty index - the (long) interval hasn't
        // elapsed, and on_startup=false means no immediate scan either.
        assert_eq!(shared.children(&ObjectId::root()).unwrap(), Vec::new());

        handle.abort();
    }
}
