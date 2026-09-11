//! The `MetadataProvider` extension point: given a file path, extract
//! whatever metadata that implementation knows how to read. Note what
//! this trait does *not* do: group tracks into albums or artists. The
//! spec describes folder-structure grouping as the actual MVP mechanism
//! for that (see `content::music_library`), not a metadata field.
//!
//! Two implementations exist, for two different jobs. `FilenameMetadata`
//! parses only the filename — cheap, no file I/O — and is called on
//! every Browse to sort tracks within an album. `TagMetadata` reads real
//! tags and embedded art with `lofty`, real file I/O and binary parsing,
//! and is called exactly once per file, at scan time, with its result
//! cached in the `Index`. Calling `TagMetadata` per Browse instead of
//! per scan would repeat Phase 8's bug 4: real work, done once, turned
//! into real work done on every request.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub title: String,
    pub track_number: Option<u32>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub has_art: bool,
    pub duration_millis: Option<u64>,
    /// Bytes/sec, the DLNA `res@bitrate` convention - already
    /// converted here from whatever unit the backend reports, so
    /// nothing downstream needs to know or care what that was.
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub bits_per_sample: Option<u8>,
    pub channels: Option<u8>,
}

pub trait MetadataProvider: Send + Sync {
    fn metadata(&self, path: &Path) -> Metadata;

    /// Called once per full scan, after every file has been visited,
    /// with the complete set of paths the scan actually found. A
    /// stateful provider (a persistent cache, `metadata::tag_cache`)
    /// uses this to drop any record for a path no longer part of the
    /// library. The default does nothing - `FilenameMetadata`/
    /// `TagMetadata` hold no state to prune.
    fn retain_only(&self, live_paths: &[PathBuf]) {
        let _ = live_paths;
    }
}
