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

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub title: String,
    pub track_number: Option<u32>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub has_art: bool,
}

pub trait MetadataProvider: Send + Sync {
    fn metadata(&self, path: &Path) -> Metadata;
}
