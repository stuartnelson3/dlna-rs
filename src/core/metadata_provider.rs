//! The `MetadataProvider` extension point: given a file path, extract
//! metadata from the name alone. MVP uses this only for one thing: a
//! leading track number, for sort order inside an album. Note what this
//! trait does *not* do: group tracks into albums or artists. The spec
//! describes folder-structure grouping as the actual MVP mechanism for
//! that (see `content::music_library`), not a metadata field. A future
//! tag-based provider could add real artist/album/genre fields; the
//! struct below leaves room for them but MVP does not fill them in.

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub title: String,
    pub track_number: Option<u32>,
}

pub trait MetadataProvider: Send + Sync {
    fn metadata(&self, path: &Path) -> Metadata;
}
