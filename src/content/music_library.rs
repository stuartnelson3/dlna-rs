//! `MusicLibraryView`: Albums, Artists, All Songs, and Recently Added.
//! Reads the same `SharedIndex` `FolderMirror` reads, and groups its
//! items into a different tree shape. No separate data store, so a
//! rescan updates every view at once, with nothing to invalidate by hand.
//!
//! Albums and Artists group by real folder structure, not by a parsed
//! tag: an album is the folder that directly holds a track, and an
//! artist is that folder's parent. This is the heuristic the spec's
//! §5.1 describes for MVP, before any tag reading exists. A track with
//! no real artist folder above it (for example, one placed directly in a
//! configured media directory) is left out of the Artists view — it is
//! still visible under Folders, so nothing is hidden, only ungrouped.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime};

use crate::core::content_source::ContentSource;
use crate::core::metadata_provider::MetadataProvider;
use crate::index::{Container, Entry, Item, ObjectId, SharedIndex};
use crate::metadata::filename::FilenameMetadata;

#[derive(Debug, Clone)]
pub enum Mode {
    AllSongs,
    Albums,
    Artists,
    RecentlyAddedSongs { count: u32, max_age_days: u32 },
    RecentlyAddedAlbums { count: u32, max_age_days: u32 },
}

pub struct MusicLibraryView {
    index: SharedIndex,
    mode: Mode,
}

impl MusicLibraryView {
    pub fn new(index: SharedIndex, mode: Mode) -> MusicLibraryView {
        MusicLibraryView { index, mode }
    }

    /// The top-level listing for this view's mode. Every entry here sits
    /// directly under this view's own root, no matter how deep its real
    /// folder is in the index — an album three directories down is still
    /// a *direct* child of the "Albums" container. So every entry's
    /// `parent_id` is set to this view's root here, overwriting whatever
    /// real parent the index recorded. `entry()` below must match: an ID
    /// that names a top-level entry has to report the same parent.
    fn top_level(&self) -> Vec<Entry> {
        let snapshot = self.snapshot();
        let entries = match &self.mode {
            Mode::AllSongs => Self::all_songs(&snapshot),
            Mode::Albums => self.albums(&snapshot),
            Mode::Artists => self.artists(&snapshot),
            Mode::RecentlyAddedSongs {
                count,
                max_age_days,
            } => Self::recently_added_songs(&snapshot, *count, *max_age_days),
            Mode::RecentlyAddedAlbums {
                count,
                max_age_days,
            } => self.recently_added_albums(&snapshot, *count, *max_age_days),
        };
        entries.into_iter().map(reparent_to_root).collect()
    }

    /// Walks every container from the root down once, and derives every
    /// grouping this view needs from that single pass: every item, which
    /// containers are albums (they hold a track directly), and which are
    /// artists (they hold an album directly). Everything else in this
    /// type takes a `&Snapshot` instead of re-deriving these - a real
    /// library can hold many thousands of tracks, and re-walking the
    /// whole tree once per album or per artist turns one Browse call
    /// into a browse-call-shaped denial of service against itself.
    fn snapshot(&self) -> Snapshot {
        let items = self.all_items();
        let album_ids: HashSet<ObjectId> =
            items.iter().map(|item| item.parent_id.clone()).collect();
        let artist_ids: HashSet<ObjectId> = album_ids
            .iter()
            .filter_map(|album_id| match self.index.entry(album_id) {
                Some(Entry::Container(album)) => album.parent_id,
                _ => None,
            })
            .filter(|id| *id != ObjectId::root())
            .collect();
        Snapshot {
            items,
            album_ids,
            artist_ids,
        }
    }

    /// Every item in the tree, found by walking every container from the
    /// root down. Callers go through `snapshot()`, which runs this once
    /// per Browse call and shares the result - see its doc comment.
    fn all_items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        let mut stack = vec![ObjectId::root()];
        while let Some(id) = stack.pop() {
            let Some(children) = self.index.children(&id) else {
                continue;
            };
            for child in children {
                match child {
                    Entry::Container(c) => stack.push(c.id),
                    Entry::Item(item) => items.push(item),
                }
            }
        }
        items
    }

    /// Groups the snapshot's items by their containing folder - the real
    /// `parent_id` each item already carries in the index.
    fn group_by_album(snapshot: &Snapshot) -> HashMap<ObjectId, Vec<Item>> {
        let mut groups: HashMap<ObjectId, Vec<Item>> = HashMap::new();
        for item in &snapshot.items {
            groups
                .entry(item.parent_id.clone())
                .or_default()
                .push(item.clone());
        }
        groups
    }

    fn all_songs(snapshot: &Snapshot) -> Vec<Entry> {
        let mut items = snapshot.items.clone();
        items.sort_by(|a, b| a.title.cmp(&b.title));
        items.into_iter().map(Entry::Item).collect()
    }

    /// An album's displayed `childCount` must match what browsing into it
    /// actually returns: its track count, not the real folder's full
    /// child count (which can also include sub-folders — separate
    /// albums, not this one's content; see `tracks_of_album`).
    fn albums(&self, snapshot: &Snapshot) -> Vec<Entry> {
        containers_for(&self.index, snapshot.album_ids.clone())
            .into_iter()
            .map(|entry| self.with_real_child_count(entry, snapshot, Self::tracks_of_album))
            .collect()
    }

    /// Same correction as `albums`, but counting an artist's albums
    /// instead of an album's tracks.
    fn artists(&self, snapshot: &Snapshot) -> Vec<Entry> {
        containers_for(&self.index, snapshot.artist_ids.clone())
            .into_iter()
            .map(|entry| self.with_real_child_count(entry, snapshot, Self::albums_under_artist))
            .collect()
    }

    fn with_real_child_count(
        &self,
        entry: Entry,
        snapshot: &Snapshot,
        count_children: impl Fn(&Self, &Snapshot, &ObjectId) -> Option<Vec<Entry>>,
    ) -> Entry {
        match entry {
            Entry::Container(mut container) => {
                container.child_count = count_children(self, snapshot, &container.id)
                    .map(|children| children.len())
                    .unwrap_or(0);
                Entry::Container(container)
            }
            other => other,
        }
    }

    /// The tracks directly inside album `id` — real children of `id`,
    /// with any sub-folder dropped. A sub-folder under an album is
    /// itself a separate album (see this module's doc comment), not more
    /// of this one's content. `None` if `id` isn't a real album.
    fn tracks_of_album(&self, snapshot: &Snapshot, id: &ObjectId) -> Option<Vec<Entry>> {
        if !snapshot.album_ids.contains(id) {
            return None;
        }
        let mut tracks: Vec<Entry> = self
            .index
            .children(id)?
            .into_iter()
            .filter(|entry| matches!(entry, Entry::Item(_)))
            .collect();
        sort_children(&mut tracks);
        Some(tracks)
    }

    /// The albums directly under artist `id` — real children of `id`
    /// that are themselves albums. `None` if `id` isn't a real artist.
    ///
    /// A folder can, in principle, qualify as both an album (it holds a
    /// track directly) and an artist (one of its own sub-folders holds
    /// a track too). Such a folder gets its own top-level artist entry,
    /// so it's excluded here — listing it twice, in two different
    /// roles, is the kind of contradiction `entry()` can't answer for
    /// both roles at once. Its own directly-held track is the cost: it
    /// won't appear in this view. Same kind of trade-off as the
    /// orphan-track exclusion this module's doc comment describes.
    fn albums_under_artist(&self, snapshot: &Snapshot, id: &ObjectId) -> Option<Vec<Entry>> {
        if !snapshot.artist_ids.contains(id) {
            return None;
        }
        let mut albums: Vec<Container> = self
            .index
            .children(id)?
            .into_iter()
            .filter_map(|entry| match entry {
                Entry::Container(c)
                    if snapshot.album_ids.contains(&c.id)
                        && !snapshot.artist_ids.contains(&c.id) =>
                {
                    Some(c)
                }
                _ => None,
            })
            .collect();
        for album in &mut albums {
            album.child_count = self
                .tracks_of_album(snapshot, &album.id)
                .map(|tracks| tracks.len())
                .unwrap_or(0);
        }
        albums.sort_by(|a, b| a.title.cmp(&b.title));
        Some(albums.into_iter().map(Entry::Container).collect())
    }

    fn recently_added_songs(snapshot: &Snapshot, count: u32, max_age_days: u32) -> Vec<Entry> {
        let cutoff = age_cutoff(max_age_days);
        let mut items: Vec<Item> = snapshot
            .items
            .iter()
            .filter(|item| is_recent_enough(item.modified, cutoff))
            .cloned()
            .collect();
        items.sort_by_key(|item| std::cmp::Reverse(item.modified));
        items.truncate(count as usize);
        items.into_iter().map(Entry::Item).collect()
    }

    /// An album counts as "recently added" by its newest track's date —
    /// adding a few new tracks to an existing album should bring the
    /// whole album back to the top, not just the new tracks.
    fn recently_added_albums(
        &self,
        snapshot: &Snapshot,
        count: u32,
        max_age_days: u32,
    ) -> Vec<Entry> {
        let cutoff = age_cutoff(max_age_days);
        let mut dated: Vec<(Container, SystemTime)> = Self::group_by_album(snapshot)
            .into_iter()
            .filter_map(|(album_id, items)| {
                let Entry::Container(mut container) = self.index.entry(&album_id)? else {
                    return None;
                };
                container.child_count = self
                    .tracks_of_album(snapshot, &album_id)
                    .map(|tracks| tracks.len())
                    .unwrap_or(0);
                let newest = items.iter().map(|item| item.modified).max()?;
                Some((container, newest))
            })
            .filter(|(_, newest)| is_recent_enough(*newest, cutoff))
            .collect();
        dated.sort_by_key(|(_, newest)| std::cmp::Reverse(*newest));
        dated.truncate(count as usize);
        dated
            .into_iter()
            .map(|(container, _)| Entry::Container(container))
            .collect()
    }
}

/// Everything derived from one walk of the whole tree - computed once per
/// Browse call by `snapshot()`, then shared. See that method's doc
/// comment for why this exists.
struct Snapshot {
    items: Vec<Item>,
    album_ids: HashSet<ObjectId>,
    artist_ids: HashSet<ObjectId>,
}

impl ContentSource for MusicLibraryView {
    fn children(&self, id: &ObjectId) -> Option<Vec<Entry>> {
        if *id == ObjectId::root() {
            return Some(self.top_level());
        }
        match &self.mode {
            // Both the plain Albums view and each album inside Recently
            // Added Albums show the same thing one level down: that
            // album's own tracks.
            Mode::Albums | Mode::RecentlyAddedAlbums { .. } => {
                self.tracks_of_album(&self.snapshot(), id)
            }
            Mode::Artists => {
                let snapshot = self.snapshot();
                self.albums_under_artist(&snapshot, id)
                    .or_else(|| self.tracks_of_album(&snapshot, id))
            }
            Mode::AllSongs | Mode::RecentlyAddedSongs { .. } => None,
        }
    }

    fn entry(&self, id: &ObjectId) -> Option<Entry> {
        if *id == ObjectId::root() {
            return Some(Entry::Container(Container {
                id: ObjectId::root(),
                parent_id: None,
                title: String::new(),
                child_count: self.top_level().len(),
            }));
        }
        // A top-level entry (an album, an artist, a recently-added song)
        // must report the same reparented `parent_id` here that
        // `children(&root)` already gave out for it - so check there
        // first, and only fall back to the index's real entry once we
        // know `id` names something deeper than the top level.
        if let Some(entry) = self.top_level().into_iter().find(|entry| entry.id() == id) {
            return Some(entry);
        }
        // A second-level album (one browsed into by way of an artist,
        // in Artists mode) needs the same `child_count` correction
        // `albums`/`albums_under_artist` already apply, for the same
        // reason: the real folder's full child count can include a
        // sub-folder that isn't this album's own track.
        if matches!(
            self.mode,
            Mode::Albums | Mode::Artists | Mode::RecentlyAddedAlbums { .. }
        ) {
            let snapshot = self.snapshot();
            if snapshot.album_ids.contains(id) {
                let mut entry = self.index.entry(id)?;
                if let Entry::Container(container) = &mut entry {
                    container.child_count = self
                        .tracks_of_album(&snapshot, id)
                        .map(|tracks| tracks.len())
                        .unwrap_or(0);
                }
                return Some(entry);
            }
        }
        self.index.entry(id)
    }
}

/// Overwrites `parent_id` to this view's own root - see `top_level`'s
/// doc comment for why every top-level entry needs this.
fn reparent_to_root(entry: Entry) -> Entry {
    match entry {
        Entry::Container(mut container) => {
            container.parent_id = Some(ObjectId::root());
            Entry::Container(container)
        }
        Entry::Item(mut item) => {
            item.parent_id = ObjectId::root();
            Entry::Item(item)
        }
    }
}

/// Resolves a set of container IDs to their real `Container` entries,
/// dropping any ID that no longer resolves (the index changed under us —
/// a rescan can replace it mid-walk) rather than failing the whole list.
/// Sorted by title, since a `HashSet`'s own order is not stable across
/// runs.
fn containers_for(index: &SharedIndex, ids: HashSet<ObjectId>) -> Vec<Entry> {
    let mut containers: Vec<Container> = ids
        .into_iter()
        .filter_map(|id| match index.entry(&id) {
            Some(Entry::Container(container)) => Some(container),
            _ => None,
        })
        .collect();
    containers.sort_by(|a, b| a.title.cmp(&b.title));
    containers.into_iter().map(Entry::Container).collect()
}

/// Sorts a container's children for display: tracks by parsed track
/// number (numbered tracks first, in order; unnumbered ones after, by
/// title), sub-containers by title. `sort_by` is stable, so ties keep
/// their original order.
fn sort_children(children: &mut [Entry]) {
    if children.iter().all(|entry| matches!(entry, Entry::Item(_))) {
        children.sort_by_key(track_sort_key);
    } else {
        children.sort_by(|a, b| title_of(a).cmp(title_of(b)));
    }
}

fn track_sort_key(entry: &Entry) -> (bool, Option<u32>, String) {
    match entry {
        Entry::Item(item) => {
            let metadata = FilenameMetadata.metadata(&item.path);
            (
                metadata.track_number.is_none(),
                metadata.track_number,
                metadata.title,
            )
        }
        Entry::Container(c) => (false, None, c.title.clone()),
    }
}

fn title_of(entry: &Entry) -> &str {
    match entry {
        Entry::Container(c) => &c.title,
        Entry::Item(i) => &i.title,
    }
}

/// `None` means no cutoff at all (`max_age_days == 0`, per config).
fn age_cutoff(max_age_days: u32) -> Option<SystemTime> {
    if max_age_days == 0 {
        return None;
    }
    let age = Duration::from_secs(u64::from(max_age_days) * 86400);
    SystemTime::now().checked_sub(age)
}

fn is_recent_enough(modified: SystemTime, cutoff: Option<SystemTime>) -> bool {
    match cutoff {
        Some(cutoff) => modified >= cutoff,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::IndexBuilder;
    use std::path::PathBuf;

    /// Builds a small tree:
    /// ```text
    /// Artist/
    ///   Album/
    ///     01 - First.mp3
    ///     02 - Second.mp3
    /// Loose/
    ///   Track.mp3          (album with no artist folder above it)
    /// ```
    fn fixture() -> SharedIndex {
        let mut builder = IndexBuilder::new();
        let artist = builder.add_container(&ObjectId::root(), "Artist".to_string());
        let album = builder.add_container(&artist, "Album".to_string());
        builder.add_item(
            &album,
            "02 - Second.mp3".to_string(),
            PathBuf::from("/m/Artist/Album/02 - Second.mp3"),
            2,
            SystemTime::UNIX_EPOCH,
        );
        builder.add_item(
            &album,
            "01 - First.mp3".to_string(),
            PathBuf::from("/m/Artist/Album/01 - First.mp3"),
            1,
            SystemTime::UNIX_EPOCH,
        );
        let loose = builder.add_container(&ObjectId::root(), "Loose".to_string());
        builder.add_item(
            &loose,
            "Track.mp3".to_string(),
            PathBuf::from("/m/Loose/Track.mp3"),
            3,
            SystemTime::UNIX_EPOCH,
        );
        SharedIndex::new(builder.build())
    }

    fn titles(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(title_of).collect()
    }

    #[test]
    fn all_songs_lists_every_item_alphabetically() {
        let view = MusicLibraryView::new(fixture(), Mode::AllSongs);
        let songs = view.children(&ObjectId::root()).unwrap();
        assert_eq!(
            titles(&songs),
            vec!["01 - First.mp3", "02 - Second.mp3", "Track.mp3"]
        );
    }

    #[test]
    fn albums_groups_by_containing_folder() {
        let view = MusicLibraryView::new(fixture(), Mode::Albums);
        let albums = view.children(&ObjectId::root()).unwrap();
        assert_eq!(titles(&albums), vec!["Album", "Loose"]);
    }

    #[test]
    fn browsing_into_an_album_sorts_tracks_by_track_number() {
        let view = MusicLibraryView::new(fixture(), Mode::Albums);
        let albums = view.children(&ObjectId::root()).unwrap();
        let album_id = albums[0].id().clone();

        let tracks = view.children(&album_id).unwrap();
        assert_eq!(titles(&tracks), vec!["01 - First.mp3", "02 - Second.mp3"]);
    }

    #[test]
    fn artists_groups_by_the_albums_parent_and_excludes_orphans() {
        let view = MusicLibraryView::new(fixture(), Mode::Artists);
        let artists = view.children(&ObjectId::root()).unwrap();
        // "Loose" has no artist folder above it - excluded, not crashed on.
        assert_eq!(titles(&artists), vec!["Artist"]);
    }

    #[test]
    fn browsing_into_an_artist_shows_their_albums() {
        let view = MusicLibraryView::new(fixture(), Mode::Artists);
        let artists = view.children(&ObjectId::root()).unwrap();
        let artist_id = artists[0].id().clone();

        let albums = view.children(&artist_id).unwrap();
        assert_eq!(titles(&albums), vec!["Album"]);
    }

    #[test]
    fn recently_added_songs_respects_the_configured_count() {
        let mut builder = IndexBuilder::new();
        let now = SystemTime::now();
        for n in 0u32..10 {
            let when = now - Duration::from_secs(u64::from(n) * 60);
            builder.add_item(
                &ObjectId::root(),
                format!("track-{n}.mp3"),
                PathBuf::from(format!("/m/track-{n}.mp3")),
                1,
                when,
            );
        }
        let index = SharedIndex::new(builder.build());

        // More than the configured count exist - the view must still
        // return exactly `count`, not break or return everything. This
        // is the MiniDLNA bug (docs/PLAN.md Phase 8) this feature exists
        // to avoid: a "more than 50 items" case, at a test-friendly scale.
        let view = MusicLibraryView::new(
            index,
            Mode::RecentlyAddedSongs {
                count: 3,
                max_age_days: 0,
            },
        );
        let songs = view.children(&ObjectId::root()).unwrap();
        assert_eq!(songs.len(), 3);
        assert_eq!(
            titles(&songs),
            vec!["track-0.mp3", "track-1.mp3", "track-2.mp3"]
        );
    }

    #[test]
    fn recently_added_songs_excludes_items_past_the_age_cutoff() {
        let mut builder = IndexBuilder::new();
        builder.add_item(
            &ObjectId::root(),
            "new.mp3".to_string(),
            PathBuf::from("/m/new.mp3"),
            1,
            SystemTime::now(),
        );
        builder.add_item(
            &ObjectId::root(),
            "old.mp3".to_string(),
            PathBuf::from("/m/old.mp3"),
            1,
            SystemTime::UNIX_EPOCH,
        );
        let index = SharedIndex::new(builder.build());

        let view = MusicLibraryView::new(
            index,
            Mode::RecentlyAddedSongs {
                count: 50,
                max_age_days: 1,
            },
        );
        let songs = view.children(&ObjectId::root()).unwrap();
        assert_eq!(titles(&songs), vec!["new.mp3"]);
    }

    #[test]
    fn recently_added_albums_uses_the_albums_newest_track() {
        let mut builder = IndexBuilder::new();
        let old_album = builder.add_container(&ObjectId::root(), "Old Album".to_string());
        builder.add_item(
            &old_album,
            "track.mp3".to_string(),
            PathBuf::from("/m/old/track.mp3"),
            1,
            SystemTime::UNIX_EPOCH,
        );

        let new_album = builder.add_container(&ObjectId::root(), "New Album".to_string());
        builder.add_item(
            &new_album,
            "track.mp3".to_string(),
            PathBuf::from("/m/new/track.mp3"),
            1,
            SystemTime::now(),
        );

        let index = SharedIndex::new(builder.build());
        let view = MusicLibraryView::new(
            index,
            Mode::RecentlyAddedAlbums {
                count: 50,
                max_age_days: 0,
            },
        );
        let albums = view.children(&ObjectId::root()).unwrap();
        assert_eq!(titles(&albums), vec!["New Album", "Old Album"]);
    }

    #[test]
    fn empty_index_produces_no_panics_in_any_mode() {
        let index = SharedIndex::new(IndexBuilder::new().build());
        for mode in [
            Mode::AllSongs,
            Mode::Albums,
            Mode::Artists,
            Mode::RecentlyAddedSongs {
                count: 10,
                max_age_days: 0,
            },
            Mode::RecentlyAddedAlbums {
                count: 10,
                max_age_days: 0,
            },
        ] {
            let view = MusicLibraryView::new(index.clone(), mode);
            assert_eq!(view.children(&ObjectId::root()), Some(Vec::new()));
        }
    }

    /// Walks every reachable entry in `view`, from the root down. Checks
    /// two things at each step: a child's own `entry` lookup returns the
    /// same value the parent's `children` call already gave us, and a
    /// child's `parent_id` names the container we just browsed.
    fn walk_and_check_consistency(view: &MusicLibraryView) {
        let mut stack = vec![ObjectId::root()];
        while let Some(id) = stack.pop() {
            let Some(children) = view.children(&id) else {
                continue;
            };
            for child in children {
                let resolved = view
                    .entry(child.id())
                    .expect("a listed child must resolve through entry");
                assert_eq!(
                    resolved,
                    child,
                    "entry() disagrees with children() for {}",
                    child.id()
                );
                match &child {
                    Entry::Container(c) => {
                        assert_eq!(c.parent_id.as_ref(), Some(&id));
                        stack.push(c.id.clone());
                    }
                    Entry::Item(i) => assert_eq!(i.parent_id, id),
                }
            }
        }
    }

    fn scanned_index(dir: &std::path::Path) -> SharedIndex {
        let media = crate::config::MediaConfig {
            directories: vec![crate::config::MediaDirectory {
                path: dir.to_path_buf(),
                kind: crate::config::MediaKind::Audio,
            }],
            follow_symlinks: false,
            exclude_patterns: vec!["*.tmp".to_string(), ".*".to_string()],
        };
        SharedIndex::new(crate::scanner::scan(&media))
    }

    proptest::proptest! {
        /// An arbitrary real directory tree, scanned for real, then browsed
        /// through Albums and Artists. Neither mode should ever panic, and
        /// every entry a `children` call returns must check out under
        /// `walk_and_check_consistency` above — this is the property test
        /// docs/PLAN.md's Phase 8 asks for.
        #[test]
        fn albums_and_artists_never_panic_on_an_arbitrary_real_tree(
            entries in proptest::collection::vec(
                (proptest::collection::vec("[a-zA-Z0-9_]{1,8}", 1..4), proptest::bool::ANY),
                1..20,
            )
        ) {
            let dir = tempfile::tempdir().unwrap();
            for (segments, is_audio) in &entries {
                let mut full = dir.path().to_path_buf();
                for segment in &segments[..segments.len() - 1] {
                    full.push(segment);
                }
                std::fs::create_dir_all(&full).unwrap();
                let extension = if *is_audio { "mp3" } else { "txt" };
                full.push(format!("{}.{extension}", segments.last().unwrap()));
                std::fs::write(&full, b"").unwrap();
            }

            let index = scanned_index(dir.path());
            for mode in [Mode::Albums, Mode::Artists] {
                let view = MusicLibraryView::new(index.clone(), mode);
                walk_and_check_consistency(&view);
            }
        }
    }
}
