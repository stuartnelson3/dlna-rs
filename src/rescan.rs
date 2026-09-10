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

use std::time::Duration;

use crate::config::MediaConfig;
use crate::index::SharedIndex;
use crate::scanner;

/// Runs forever: scans once immediately if `on_startup`, then re-scans
/// every `interval`. Each scan runs on a blocking thread — directory
/// walking is real filesystem I/O, and a large library shouldn't stall
/// the async runtime while it's being walked.
pub async fn run(media: MediaConfig, index: SharedIndex, interval: Duration, on_startup: bool) {
    if on_startup {
        rescan_once(&media, &index).await;
    }
    loop {
        tokio::time::sleep(interval).await;
        rescan_once(&media, &index).await;
    }
}

async fn rescan_once(media: &MediaConfig, index: &SharedIndex) {
    let media = media.clone();
    match tokio::task::spawn_blocking(move || scanner::scan(&media)).await {
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
        rescan_once(&media, &shared).await;

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

        let handle = tokio::spawn(run(media, shared.clone(), Duration::from_secs(3600), true));

        // Give the spawned task a moment to run its first (on_startup)
        // scan - it runs immediately, no sleep to wait out.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(shared.children(&ObjectId::root()).unwrap().len(), 1);

        handle.abort();
    }

    #[tokio::test]
    async fn run_does_not_scan_before_the_first_interval_when_on_startup_is_false() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("one.mp3"), b"x").unwrap();
        let media = media_config(dir.path());
        let shared = SharedIndex::new(IndexBuilder::new().build());

        let handle = tokio::spawn(run(media, shared.clone(), Duration::from_secs(3600), false));

        tokio::time::sleep(Duration::from_millis(50)).await;
        // Still the original empty index - the (long) interval hasn't
        // elapsed, and on_startup=false means no immediate scan either.
        assert_eq!(shared.children(&ObjectId::root()).unwrap(), Vec::new());

        handle.abort();
    }
}
