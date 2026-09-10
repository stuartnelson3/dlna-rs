//! The in-memory index: what the last scan found on disk. Rebuilt whole
//! on each scan (see docs/PLAN.md Phase 7) rather than mutated in place —
//! per the spec, this trades a little rescan CPU for zero
//! persistence-consistency bugs by construction.
//!
//! Two shapes of data live here on purpose: [`Node`]/[`NodeKind`] are the
//! index's own storage (a container's real child-ID list, an item's real
//! path), and [`Entry`]/[`Container`]/[`Item`] are what gets handed back
//! to a `ContentSource` caller (a container's *count* of children, not
//! the list — Browse only needs a count at that level, not what all the
//! grandchildren are called). Collapsing these into one type would mean
//! either leaking storage details outward or re-deriving a "view" shape
//! every time something reads the index.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectId(String);

impl ObjectId {
    /// Wraps any string as an `ObjectId` — always succeeds, because
    /// construction never fails: an `ObjectId` is just an opaque key, and
    /// whether it actually names anything is a lookup question
    /// (`Index::entry`/`children` returning `None`), not a construction
    /// question. This is the constructor a Browse request's
    /// attacker-supplied `ObjectID` argument goes through — see
    /// docs/THREAT_MODEL.md.
    pub fn new(raw: impl Into<String>) -> ObjectId {
        ObjectId(raw.into())
    }

    /// Reserved by the UPnP ContentDirectory spec: browsing this ID means
    /// "the root of the tree."
    pub fn root() -> ObjectId {
        ObjectId("0".to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Container(Container),
    Item(Item),
}

impl Entry {
    pub fn id(&self) -> &ObjectId {
        match self {
            Entry::Container(c) => &c.id,
            Entry::Item(i) => &i.id,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Container {
    pub id: ObjectId,
    /// `None` only for the root container itself.
    pub parent_id: Option<ObjectId>,
    pub title: String,
    pub child_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: ObjectId,
    pub parent_id: ObjectId,
    pub title: String,
    pub path: PathBuf,
    pub size: u64,
    pub modified: SystemTime,
}

#[derive(Debug)]
struct Node {
    parent_id: Option<ObjectId>,
    title: String,
    kind: NodeKind,
}

#[derive(Debug)]
enum NodeKind {
    Container {
        children: Vec<ObjectId>,
    },
    Item {
        path: PathBuf,
        size: u64,
        modified: SystemTime,
    },
}

#[derive(Debug, Default)]
pub struct Index {
    nodes: HashMap<ObjectId, Node>,
}

impl Index {
    /// Children of `id`, in the order the scan found them. `None` if `id`
    /// isn't a container in this index — including if it isn't in the
    /// index at all. Callers must treat that as "not found," not panic or
    /// guess (see docs/THREAT_MODEL.md on the ObjectID namespace).
    pub fn children(&self, id: &ObjectId) -> Option<Vec<Entry>> {
        let node = self.nodes.get(id)?;
        match &node.kind {
            NodeKind::Container { children } => Some(
                children
                    .iter()
                    .filter_map(|child| self.entry(child))
                    .collect(),
            ),
            NodeKind::Item { .. } => None,
        }
    }

    /// The entry `id` itself refers to, container or item. `None` if `id`
    /// isn't in this index.
    pub fn entry(&self, id: &ObjectId) -> Option<Entry> {
        let node = self.nodes.get(id)?;
        Some(match &node.kind {
            NodeKind::Container { children } => Entry::Container(Container {
                id: id.clone(),
                parent_id: node.parent_id.clone(),
                title: node.title.clone(),
                child_count: children.len(),
            }),
            NodeKind::Item {
                path,
                size,
                modified,
            } => Entry::Item(Item {
                id: id.clone(),
                parent_id: node
                    .parent_id
                    .clone()
                    .expect("an item's parent_id is only ever None for the root, and the root is always a container"),
                title: node.title.clone(),
                path: path.clone(),
                size: *size,
                modified: *modified,
            }),
        })
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

/// Builds an [`Index`] one container/item at a time, assigning each a
/// fresh ID and linking it into its parent's child list. This is where
/// the "never a dangling parent, never a duplicate ID" invariant actually
/// gets enforced — every caller (right now, just `scanner`) goes through
/// here rather than constructing `Index` by hand.
pub struct IndexBuilder {
    index: Index,
    next_id: u64,
}

impl IndexBuilder {
    pub fn new() -> IndexBuilder {
        let mut nodes = HashMap::new();
        nodes.insert(
            ObjectId::root(),
            Node {
                parent_id: None,
                title: String::new(),
                kind: NodeKind::Container {
                    children: Vec::new(),
                },
            },
        );
        IndexBuilder {
            index: Index { nodes },
            next_id: 1,
        }
    }

    /// Adds a container as a child of `parent`. Panics if `parent` isn't
    /// already a container in this builder — that's a bug in the caller
    /// (`scanner`, walking depth-first, should never be able to trigger
    /// this), not a condition to handle gracefully.
    pub fn add_container(&mut self, parent: &ObjectId, title: String) -> ObjectId {
        self.insert(parent, title, |_| NodeKind::Container {
            children: Vec::new(),
        })
    }

    pub fn add_item(
        &mut self,
        parent: &ObjectId,
        title: String,
        path: PathBuf,
        size: u64,
        modified: SystemTime,
    ) -> ObjectId {
        self.insert(parent, title, |_| NodeKind::Item {
            path,
            size,
            modified,
        })
    }

    fn insert(
        &mut self,
        parent: &ObjectId,
        title: String,
        kind: impl FnOnce(&ObjectId) -> NodeKind,
    ) -> ObjectId {
        let id = ObjectId(self.next_id.to_string());
        self.next_id += 1;

        match self.index.nodes.get_mut(parent) {
            Some(Node {
                kind: NodeKind::Container { children },
                ..
            }) => children.push(id.clone()),
            Some(_) => {
                panic!("add_* called with a parent ({parent}) that is an item, not a container")
            }
            None => panic!("add_* called with a parent ({parent}) that isn't in the index"),
        }

        self.index.nodes.insert(
            id.clone(),
            Node {
                parent_id: Some(parent.clone()),
                title,
                kind: kind(&id),
            },
        );
        id
    }

    pub fn build(self) -> Index {
        self.index
    }
}

impl Default for IndexBuilder {
    fn default() -> Self {
        IndexBuilder::new()
    }
}

/// A shared, swappable [`Index`] — cheap to clone (an `Arc` underneath),
/// so every `ContentSource` that reads through the same media directories
/// can hold its own clone and all see the same data, including after a
/// rescan (`rescan::run`) replaces it wholesale. Today that's just
/// `FolderMirror`; per the spec, Phase 8's `MusicLibraryView` "queries the
/// same underlying Index `FolderMirror` reads from" — this is that shared
/// resource, not something private to `FolderMirror`.
#[derive(Clone)]
pub struct SharedIndex(std::sync::Arc<std::sync::RwLock<Index>>);

impl SharedIndex {
    pub fn new(index: Index) -> SharedIndex {
        SharedIndex(std::sync::Arc::new(std::sync::RwLock::new(index)))
    }

    /// Replaces the whole index. A full rebuild-and-swap, not an
    /// incremental patch — matches "MVP: in-memory index, rebuilt on
    /// scan," so there's no diffing logic to get subtly wrong. Object IDs
    /// are reassigned each call as a result; see docs/DESIGN.md for why
    /// that's an accepted MVP tradeoff.
    pub fn replace(&self, index: Index) {
        *self.0.write().expect("index lock poisoned") = index;
    }

    pub fn children(&self, id: &ObjectId) -> Option<Vec<Entry>> {
        self.0.read().expect("index lock poisoned").children(id)
    }

    pub fn entry(&self, id: &ObjectId) -> Option<Entry> {
        self.0.read().expect("index lock poisoned").entry(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_index_replace_changes_what_subsequent_reads_see() {
        let shared = SharedIndex::new(IndexBuilder::new().build());
        assert_eq!(shared.children(&ObjectId::root()), Some(Vec::new()));

        let mut builder = IndexBuilder::new();
        builder.add_container(&ObjectId::root(), "New".to_string());
        shared.replace(builder.build());

        assert_eq!(shared.children(&ObjectId::root()).unwrap().len(), 1);
    }

    #[test]
    fn shared_index_clones_share_the_same_underlying_index() {
        let shared = SharedIndex::new(IndexBuilder::new().build());
        let handle = shared.clone();

        let mut builder = IndexBuilder::new();
        builder.add_container(&ObjectId::root(), "New".to_string());
        shared.replace(builder.build());

        // The clone sees the replacement too - they're the same storage.
        assert_eq!(handle.children(&ObjectId::root()).unwrap().len(), 1);
    }

    #[test]
    fn root_starts_as_an_empty_container() {
        let index = IndexBuilder::new().build();
        assert_eq!(index.children(&ObjectId::root()), Some(Vec::new()));
    }

    #[test]
    fn add_container_links_into_parents_children() {
        let mut builder = IndexBuilder::new();
        let id = builder.add_container(&ObjectId::root(), "Music".to_string());
        let index = builder.build();

        let children = index.children(&ObjectId::root()).unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id(), &id);
    }

    #[test]
    fn add_item_records_full_metadata() {
        let mut builder = IndexBuilder::new();
        let id = builder.add_item(
            &ObjectId::root(),
            "Track.flac".to_string(),
            PathBuf::from("/music/Track.flac"),
            12345,
            SystemTime::UNIX_EPOCH,
        );
        let index = builder.build();

        match index.entry(&id).unwrap() {
            Entry::Item(item) => {
                assert_eq!(item.title, "Track.flac");
                assert_eq!(item.path, PathBuf::from("/music/Track.flac"));
                assert_eq!(item.size, 12345);
                assert_eq!(item.parent_id, ObjectId::root());
            }
            Entry::Container(_) => panic!("expected an item"),
        }
    }

    #[test]
    fn container_child_count_reflects_children() {
        let mut builder = IndexBuilder::new();
        let album = builder.add_container(&ObjectId::root(), "Album".to_string());
        builder.add_item(
            &album,
            "01.mp3".to_string(),
            PathBuf::from("/a/01.mp3"),
            1,
            SystemTime::UNIX_EPOCH,
        );
        builder.add_item(
            &album,
            "02.mp3".to_string(),
            PathBuf::from("/a/02.mp3"),
            1,
            SystemTime::UNIX_EPOCH,
        );
        let index = builder.build();

        match index.entry(&album).unwrap() {
            Entry::Container(c) => assert_eq!(c.child_count, 2),
            Entry::Item(_) => panic!("expected a container"),
        }
    }

    #[test]
    fn unknown_id_is_none_not_a_panic() {
        let index = IndexBuilder::new().build();
        let bogus = ObjectId("does-not-exist".to_string());
        assert!(index.entry(&bogus).is_none());
        assert!(index.children(&bogus).is_none());
    }

    #[test]
    fn children_of_an_item_is_none() {
        let mut builder = IndexBuilder::new();
        let id = builder.add_item(
            &ObjectId::root(),
            "Track.flac".to_string(),
            PathBuf::from("/music/Track.flac"),
            1,
            SystemTime::UNIX_EPOCH,
        );
        let index = builder.build();
        assert!(index.children(&id).is_none());
    }

    #[test]
    #[should_panic(expected = "isn't in the index")]
    fn add_container_with_unknown_parent_panics() {
        let mut builder = IndexBuilder::new();
        builder.add_container(&ObjectId("999".to_string()), "orphan".to_string());
    }
}
