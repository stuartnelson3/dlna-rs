//! `FolderMirror`: a 1:1 mirror of the filesystem, exactly as scanned.
//! The true minimal baseline `ContentSource` — no grouping, no
//! heuristics, just what's on disk.

use crate::core::content_source::ContentSource;
use crate::index::{Entry, ObjectId, SharedIndex};

pub struct FolderMirror {
    index: SharedIndex,
}

impl FolderMirror {
    /// Takes a `SharedIndex`, not an owned `Index` — the rescan timer
    /// (`rescan::run`) replaces its contents wholesale on a schedule, and
    /// this is how `FolderMirror` sees that without needing to be
    /// reconstructed or handed a new value each time.
    pub fn new(index: SharedIndex) -> FolderMirror {
        FolderMirror { index }
    }
}

impl ContentSource for FolderMirror {
    fn children(&self, id: &ObjectId) -> Option<Vec<Entry>> {
        self.index.children(id)
    }

    fn entry(&self, id: &ObjectId) -> Option<Entry> {
        self.index.entry(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::IndexBuilder;

    #[test]
    fn delegates_to_the_underlying_index() {
        let mut builder = IndexBuilder::new();
        let id = builder.add_container(&ObjectId::root(), "Music".to_string());
        let mirror = FolderMirror::new(SharedIndex::new(builder.build()));

        assert_eq!(mirror.entry(&id).unwrap().id(), &id);
        assert_eq!(mirror.children(&ObjectId::root()).unwrap().len(), 1);
    }

    #[test]
    fn root_of_an_empty_index_is_an_empty_container() {
        let mirror = FolderMirror::new(SharedIndex::new(IndexBuilder::new().build()));
        assert_eq!(mirror.children(&ObjectId::root()), Some(Vec::new()));
    }

    #[test]
    fn reflects_a_rescan_replacing_the_shared_index() {
        let shared = SharedIndex::new(IndexBuilder::new().build());
        let mirror = FolderMirror::new(shared.clone());
        assert_eq!(mirror.children(&ObjectId::root()), Some(Vec::new()));

        let mut builder = IndexBuilder::new();
        builder.add_container(&ObjectId::root(), "New".to_string());
        shared.replace(builder.build());

        assert_eq!(mirror.children(&ObjectId::root()).unwrap().len(), 1);
    }
}
