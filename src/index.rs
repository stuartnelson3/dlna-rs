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

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
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
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub track_number: Option<u32>,
    pub has_art: bool,
    pub duration_millis: Option<u64>,
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub bits_per_sample: Option<u8>,
    pub channels: Option<u8>,
}

/// The tag-derived facts an item carries, read once at scan time by a
/// `MetadataProvider` and cached here — never re-read per Browse call.
/// `title` isn't part of this: the scanner derives it its own way. The
/// track number is, since it decides display order (tag first, filename
/// as the fallback, both resolved by the `MetadataProvider`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackTags {
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub track_number: Option<u32>,
    pub has_art: bool,
    pub duration_millis: Option<u64>,
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub bits_per_sample: Option<u8>,
    pub channels: Option<u8>,
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
        tags: TrackTags,
    },
}

#[derive(Debug, Default)]
pub struct Index {
    nodes: HashMap<ObjectId, Node>,
    /// The container for each configured media directory. These are
    /// folders the user chose as library roots, not folders that name an
    /// artist or an album.
    media_roots: HashSet<ObjectId>,
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
                tags,
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
                artist: tags.artist.clone(),
                album_artist: tags.album_artist.clone(),
                album: tags.album.clone(),
                genre: tags.genre.clone(),
                track_number: tags.track_number,
                has_art: tags.has_art,
                duration_millis: tags.duration_millis,
                bitrate: tags.bitrate,
                sample_rate: tags.sample_rate,
                bits_per_sample: tags.bits_per_sample,
                channels: tags.channels,
            }),
        })
    }

    /// Every item's real path, in no particular order. Used only to
    /// prune a persistent tag cache of entries for files no longer in
    /// the library - see `core::metadata_provider::MetadataProvider::retain_only`.
    /// A flat scan of every node, not a tree walk: this index already
    /// stores every node in one map, so there's no need to reach for
    /// `content::music_library`'s recursive `all_items` (that one
    /// walks `children()` specifically to produce Browse-ordered
    /// results, a different job).
    pub fn item_paths(&self) -> Vec<PathBuf> {
        self.nodes
            .values()
            .filter_map(|node| match &node.kind {
                NodeKind::Item { path, .. } => Some(path.clone()),
                NodeKind::Container { .. } => None,
            })
            .collect()
    }

    /// True if `id` is the container of a configured media directory.
    pub fn is_media_root(&self, id: &ObjectId) -> bool {
        self.media_roots.contains(id)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

/// Builds an [`Index`] one container/item at a time, giving each an ID
/// and linking it into its parent's child list.
///
/// An ID depends only on where an entry is: a media directory's full
/// path, or an entry's parent ID and its file or folder name. A client
/// keeps stream URLs, which hold IDs, so a rescan or a restart must give
/// the same file the same ID. This is where
/// the "never a dangling parent, never a duplicate ID" invariant actually
/// gets enforced — every caller (right now, just `scanner`) goes through
/// here rather than constructing `Index` by hand.
pub struct IndexBuilder {
    index: Index,
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
            index: Index {
                nodes,
                media_roots: HashSet::new(),
            },
        }
    }

    /// Adds a container as a child of `parent`. Panics if `parent` isn't
    /// already a container in this builder — that's a bug in the caller
    /// (`scanner`, walking depth-first, should never be able to trigger
    /// this), not a condition to handle gracefully.
    pub fn add_container(&mut self, parent: &ObjectId, title: String) -> ObjectId {
        let key = child_key(parent, title.as_bytes());
        self.insert(
            parent,
            &key,
            title,
            NodeKind::Container {
                children: Vec::new(),
            },
        )
    }

    /// Adds the container for a configured media directory, directly
    /// under the root, and records that it is a media directory.
    pub fn add_media_root(&mut self, path: &Path, title: String) -> ObjectId {
        let id = self.insert(
            &ObjectId::root(),
            path.as_os_str().as_encoded_bytes(),
            title,
            NodeKind::Container {
                children: Vec::new(),
            },
        );
        self.index.media_roots.insert(id.clone());
        id
    }

    pub fn add_item(
        &mut self,
        parent: &ObjectId,
        title: String,
        path: PathBuf,
        size: u64,
        modified: SystemTime,
    ) -> ObjectId {
        self.add_item_with_tags(parent, title, path, size, modified, TrackTags::default())
    }

    /// Same as [`add_item`](Self::add_item), plus the tag data a
    /// `MetadataProvider` read from the file at scan time.
    pub fn add_item_with_tags(
        &mut self,
        parent: &ObjectId,
        title: String,
        path: PathBuf,
        size: u64,
        modified: SystemTime,
        tags: TrackTags,
    ) -> ObjectId {
        // The file name, not the title: the title drops the track number
        // and the extension, so two files can share one.
        let name = path.file_name().unwrap_or(path.as_os_str());
        let key = child_key(parent, name.as_encoded_bytes());
        self.insert(
            parent,
            &key,
            title,
            NodeKind::Item {
                path,
                size,
                modified,
                tags,
            },
        )
    }

    fn insert(&mut self, parent: &ObjectId, key: &[u8], title: String, kind: NodeKind) -> ObjectId {
        let id = self.unused_id(key);

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
                kind,
            },
        );
        id
    }

    /// The ID for `key`. If another entry holds it, hashes the key again
    /// with a counter, so no entry replaces another. This happens when
    /// the config lists one media directory twice, or when two folder
    /// names that are not UTF-8 convert to the same title. Only then does
    /// an ID depend on the order of the walk.
    fn unused_id(&self, key: &[u8]) -> ObjectId {
        let mut id = hashed_id(key);
        let mut attempt: u32 = 0;
        while self.index.nodes.contains_key(&id) {
            attempt += 1;
            // No path holds a NUL byte, so this can't equal a real key.
            id = hashed_id(&[key, b"\0", &attempt.to_le_bytes()].concat());
        }
        if attempt > 0 {
            log::warn!(
                "two entries share the ID key {:?}; gave the later one ID {id}",
                String::from_utf8_lossy(key)
            );
        }
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

/// The key for an entry inside `parent`. A parent ID has no `/`, so the
/// first `/` always ends it.
fn child_key(parent: &ObjectId, name: &[u8]) -> Vec<u8> {
    [parent.as_str().as_bytes(), b"/", name].concat()
}

/// 16 hex digits: pure ASCII, and free of the `$` that
/// `CompositeContentSource` splits on.
fn hashed_id(key: &[u8]) -> ObjectId {
    ObjectId(format!("{:016x}", fnv1a_64(key)))
}

/// 64-bit FNV-1a. Written here because the output must never change
/// between builds, and `std`'s `DefaultHasher` does not promise that.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
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
    /// scan," so there's no diffing logic to get subtly wrong. An unmoved
    /// file keeps its ID across the swap; see [`IndexBuilder`].
    pub fn replace(&self, index: Index) {
        *self.0.write().expect("index lock poisoned") = index;
    }

    pub fn children(&self, id: &ObjectId) -> Option<Vec<Entry>> {
        self.0.read().expect("index lock poisoned").children(id)
    }

    pub fn entry(&self, id: &ObjectId) -> Option<Entry> {
        self.0.read().expect("index lock poisoned").entry(id)
    }

    pub fn is_media_root(&self, id: &ObjectId) -> bool {
        self.0
            .read()
            .expect("index lock poisoned")
            .is_media_root(id)
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
    fn fnv1a_64_matches_the_published_test_vectors() {
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    /// Pinned values: a change to the key format or the hash breaks every
    /// URL a client holds, so it must not happen by accident.
    #[test]
    fn ids_keep_the_same_value_between_builds() {
        let mut builder = IndexBuilder::new();
        let music = builder.add_media_root(Path::new("/music"), "music".to_string());
        let artist = builder.add_container(&music, "Artist".to_string());

        assert_eq!(music.as_str(), "75dc8a6a38687865");
        assert_eq!(artist.as_str(), "7db80c17a89604bd");
    }

    #[test]
    fn an_items_id_comes_from_its_file_name_not_its_title() {
        let mut builder = IndexBuilder::new();
        let [one, two] = ["01 - Intro.flac", "02 - Intro.flac"].map(|file| {
            builder.add_item(
                &ObjectId::root(),
                "Intro".to_string(),
                Path::new("/music").join(file),
                1,
                SystemTime::UNIX_EPOCH,
            )
        });

        assert_eq!(one, hashed_id(b"0/01 - Intro.flac"));
        assert_eq!(two, hashed_id(b"0/02 - Intro.flac"));
    }

    #[test]
    fn a_media_directory_listed_twice_keeps_both_trees() {
        let mut builder = IndexBuilder::new();
        let ids = [(); 3].map(|()| {
            let root = builder.add_media_root(Path::new("/music"), "music".to_string());
            let album = builder.add_container(&root, "Album".to_string());
            (root, album)
        });
        let index = builder.build();

        assert_eq!(index.children(&ObjectId::root()).unwrap().len(), 3);
        for (root, album) in &ids {
            let children = index.children(root).unwrap();
            assert_eq!(children.len(), 1);
            assert_eq!(children[0].id(), album);
        }
        assert_eq!(index.len(), 7, "the root, three trees of two");
    }

    #[test]
    #[should_panic(expected = "isn't in the index")]
    fn add_container_with_unknown_parent_panics() {
        let mut builder = IndexBuilder::new();
        builder.add_container(&ObjectId("999".to_string()), "orphan".to_string());
    }
}
