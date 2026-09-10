//! `FolderMirror`: a 1:1 mirror of the filesystem, exactly as scanned.
//! The true minimal baseline `ContentSource` — no grouping, no
//! heuristics, just what's on disk.

use crate::core::content_source::ContentSource;
use crate::index::{Entry, Index, ObjectId};

pub struct FolderMirror {
    index: Index,
}

impl FolderMirror {
    pub fn new(index: Index) -> FolderMirror {
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
        let mirror = FolderMirror::new(builder.build());

        assert_eq!(mirror.entry(&id).unwrap().id(), &id);
        assert_eq!(mirror.children(&ObjectId::root()).unwrap().len(), 1);
    }

    #[test]
    fn root_of_an_empty_index_is_an_empty_container() {
        let mirror = FolderMirror::new(IndexBuilder::new().build());
        assert_eq!(mirror.children(&ObjectId::root()), Some(Vec::new()));
    }
}
