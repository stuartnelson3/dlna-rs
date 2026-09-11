//! `MusicLibraryView`: Albums, Artists, All Songs, and Recently Added.
//! Reads the same `SharedIndex` `FolderMirror` reads, and groups its
//! items into a different tree shape. No separate data store, so a
//! rescan updates every view at once, with nothing to invalidate by hand.
//!
//! An album is still always the folder that directly holds a track —
//! the spec's §5.1 heuristic for MVP, from before any tag reading
//! existed. But its *displayed title*, and which artist it groups
//! under, now prefer a real tag over the folder when one is trustworthy:
//! specifically, when every track directly in that folder that carries
//! the tag agrees on the same value (see `consistent_tag_value`). A real
//! compilation, where tracks in one folder disagree on artist, falls
//! back to the plain folder heuristic rather than guessing — building a
//! genuine "Various Artists" grouping is a separate, deferred feature
//! (`docs/PLAN.md`'s After MVP list), not this one.
//!
//! This is also how the same real artist gets grouped into one Artists
//! entry even when their albums are scattered across different real
//! folder shapes — a proper `Artist/Album` tree plus several flat
//! `Artist - Album (Year)` folders directly under the media root, say —
//! since a consistent artist tag is used as the grouping key regardless
//! of where its album folder physically sits. A folder with no
//! consistent artist tag, or no artist folder above it at all, is left
//! out of the Artists view exactly as before — still visible under
//! Folders and Albums, so nothing is hidden, only ungrouped.

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
    /// grouping this view needs from that single pass: every item, every
    /// album's resolved display title and artist, and every artist (real
    /// folder or tag-derived) with its own resolved list of albums.
    /// Everything else in this type takes a `&Snapshot` instead of
    /// re-deriving these - a real library can hold many thousands of
    /// tracks, and re-walking the whole tree once per album or per
    /// artist turns one Browse call into a browse-call-shaped denial of
    /// service against itself.
    fn snapshot(&self) -> Snapshot {
        let items = self.all_items();
        let album_ids: HashSet<ObjectId> =
            items.iter().map(|item| item.parent_id.clone()).collect();
        let by_album = Self::group_items_by_parent(&items);

        // Per album: its real container, its tag-preferred display
        // title, and which artist it resolves under. `artist_key` is
        // `None` for a true orphan (no consistent artist tag, and no
        // real artist folder above it) - excluded from Artists, same as
        // always. `artist_raw_name` keeps the tag's original casing,
        // only for the `Tag` case, for picking a deterministic display
        // title once albums are grouped by artist below.
        struct Resolved {
            display_title: String,
            artist_key: Option<ArtistKey>,
            artist_raw_name: Option<String>,
        }

        let mut resolved: HashMap<ObjectId, Resolved> = HashMap::new();
        for (album_id, tracks) in &by_album {
            let Some(Entry::Container(real_container)) = self.index.entry(album_id) else {
                continue;
            };
            let display_title = consistent_tag_value(tracks, |i| i.album.as_deref())
                .unwrap_or_else(|| real_container.title.clone());
            let (artist_key, artist_raw_name) =
                match consistent_tag_value(tracks, |i| i.artist.as_deref()) {
                    Some(name) => (
                        Some(ArtistKey::Tag(normalize_artist_key(&name))),
                        Some(name),
                    ),
                    None => {
                        let folder_key = real_container
                            .parent_id
                            .clone()
                            .filter(|parent| *parent != ObjectId::root())
                            .map(ArtistKey::Folder);
                        (folder_key, None)
                    }
                };
            resolved.insert(
                album_id.clone(),
                Resolved {
                    display_title,
                    artist_key,
                    artist_raw_name,
                },
            );
        }

        let mut by_artist_key: HashMap<ArtistKey, Vec<ObjectId>> = HashMap::new();
        for (album_id, r) in &resolved {
            if let Some(key) = &r.artist_key {
                by_artist_key
                    .entry(key.clone())
                    .or_default()
                    .push(album_id.clone());
            }
        }

        let mut artists: HashMap<ObjectId, ArtistEntry> = HashMap::new();
        for (key, album_ids_for_artist) in by_artist_key {
            let container = match &key {
                // A real folder-artist: re-fetch it fresh, since its own
                // real title/parent are unaffected by this feature.
                ArtistKey::Folder(id) => match self.index.entry(id) {
                    Some(Entry::Container(mut c)) => {
                        c.child_count = album_ids_for_artist.len();
                        c
                    }
                    _ => continue,
                },
                // A tag-derived artist has no backing real container -
                // it may span several real folders, or none
                // consistently - so it's hand-built. Its display title
                // is the lexicographically smallest raw variant across
                // every album that resolved to it: deterministic, not
                // dependent on `HashMap` iteration order.
                ArtistKey::Tag(normalized) => {
                    let display_title = album_ids_for_artist
                        .iter()
                        .filter_map(|id| resolved.get(id).and_then(|r| r.artist_raw_name.clone()))
                        .min()
                        .unwrap_or_else(|| normalized.clone());
                    Container {
                        id: synthetic_artist_id(normalized),
                        parent_id: None,
                        title: display_title,
                        child_count: album_ids_for_artist.len(),
                    }
                }
            };
            artists.insert(
                container.id.clone(),
                ArtistEntry {
                    container,
                    album_ids: album_ids_for_artist,
                },
            );
        }

        // A folder can, in principle, both hold a track directly (an
        // album) and be some other album's resolved artist (real folder
        // or - now - a tag match). Such a folder gets its own top-level
        // artist entry, so its album role is dropped wherever it would
        // otherwise be listed under one - listing it twice, in two
        // different roles, is a contradiction `entry()` can't answer for
        // both at once. Same trade-off as the orphan-track exclusion
        // this module's doc comment describes. This has to run globally,
        // after every artist's albums are known, since a tag can now
        // pull an album out from under its real folder-parent's own
        // artist bucket into an unrelated tag-derived one.
        let artist_id_set: HashSet<ObjectId> = artists.keys().cloned().collect();
        for entry in artists.values_mut() {
            entry
                .album_ids
                .retain(|album_id| !artist_id_set.contains(album_id));
            entry.container.child_count = entry.album_ids.len();
        }

        let album_facts: HashMap<ObjectId, AlbumFacts> = resolved
            .iter()
            .map(|(album_id, r)| {
                let artist_id = match &r.artist_key {
                    Some(ArtistKey::Folder(id)) => id.clone(),
                    Some(ArtistKey::Tag(normalized)) => synthetic_artist_id(normalized),
                    None => ObjectId::root(),
                };
                (
                    album_id.clone(),
                    AlbumFacts {
                        display_title: r.display_title.clone(),
                        artist_id,
                    },
                )
            })
            .collect();

        Snapshot {
            items,
            album_ids,
            album_facts,
            artists,
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

    /// Groups items by their containing folder - the real `parent_id`
    /// each item already carries in the index.
    fn group_items_by_parent(items: &[Item]) -> HashMap<ObjectId, Vec<Item>> {
        let mut groups: HashMap<ObjectId, Vec<Item>> = HashMap::new();
        for item in items {
            groups
                .entry(item.parent_id.clone())
                .or_default()
                .push(item.clone());
        }
        groups
    }

    fn group_by_album(snapshot: &Snapshot) -> HashMap<ObjectId, Vec<Item>> {
        Self::group_items_by_parent(&snapshot.items)
    }

    fn all_songs(snapshot: &Snapshot) -> Vec<Entry> {
        let mut items = snapshot.items.clone();
        items.sort_by(|a, b| a.title.cmp(&b.title));
        items.into_iter().map(Entry::Item).collect()
    }

    fn albums(&self, snapshot: &Snapshot) -> Vec<Entry> {
        let mut containers: Vec<Container> = snapshot
            .album_ids
            .iter()
            .filter_map(|id| self.album_container(snapshot, id))
            .collect();
        containers.sort_by(|a, b| a.title.cmp(&b.title));
        containers.into_iter().map(Entry::Container).collect()
    }

    fn artists(&self, snapshot: &Snapshot) -> Vec<Entry> {
        let mut containers: Vec<Container> = snapshot
            .artists
            .values()
            .map(|entry| entry.container.clone())
            .collect();
        containers.sort_by(|a, b| a.title.cmp(&b.title));
        containers.into_iter().map(Entry::Container).collect()
    }

    /// The one place every album `Container` gets built: fetches the
    /// real container, overwrites its title with the tag-preferred
    /// display title from `album_facts` (falling back to the real
    /// folder name when no tag applies - `album_facts` only has an
    /// entry once `snapshot()` successfully resolved that album), and
    /// sets `child_count` to this album's actual track count. Used by
    /// every call site that lists an album, so none of them can ever
    /// disagree about its title.
    fn album_container(&self, snapshot: &Snapshot, id: &ObjectId) -> Option<Container> {
        let Entry::Container(mut container) = self.index.entry(id)? else {
            return None;
        };
        if let Some(facts) = snapshot.album_facts.get(id) {
            container.title = facts.display_title.clone();
        }
        container.child_count = self
            .tracks_of_album(snapshot, id)
            .map(|tracks| tracks.len())
            .unwrap_or(0);
        Some(container)
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

    /// The albums grouped under artist `id` - either a real folder
    /// artist's resolved albums, or every album that shares one
    /// consistent tag artist name, wherever it physically sits in the
    /// tree. `None` if `id` isn't a real artist (real or tag-derived).
    /// Each returned album's `parent_id` is set to `id`, overwriting
    /// whatever real parent the index recorded - necessary the moment an
    /// album's tag redirects it to an artist other than its real
    /// folder-parent.
    fn albums_under_artist(&self, snapshot: &Snapshot, id: &ObjectId) -> Option<Vec<Entry>> {
        let artist = snapshot.artists.get(id)?;
        let mut albums: Vec<Container> = artist
            .album_ids
            .iter()
            .filter_map(|album_id| self.album_container(snapshot, album_id))
            .map(|mut container| {
                container.parent_id = Some(id.clone());
                container
            })
            .collect();
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
                let container = self.album_container(snapshot, &album_id)?;
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

/// One album's tag-resolved facts: its display title (tag-preferred,
/// falling back to its real folder name) and which artist it groups
/// under (a real folder-artist id, a synthetic tag-artist id, or root
/// for a true orphan - see `snapshot()`).
struct AlbumFacts {
    display_title: String,
    artist_id: ObjectId,
}

/// One resolved artist, real or tag-derived, with the final (dual-role
/// exclusion already applied) list of albums under it.
struct ArtistEntry {
    container: Container,
    album_ids: Vec<ObjectId>,
}

/// The key `snapshot()` groups albums by to find their artist: either a
/// consistent tag artist name (normalized for comparison), or today's
/// exact folder-parent heuristic.
#[derive(Clone, PartialEq, Eq, Hash)]
enum ArtistKey {
    Tag(String),
    Folder(ObjectId),
}

/// Everything derived from one walk of the whole tree - computed once per
/// Browse call by `snapshot()`, then shared. See that method's doc
/// comment for why this exists.
struct Snapshot {
    items: Vec<Item>,
    album_ids: HashSet<ObjectId>,
    album_facts: HashMap<ObjectId, AlbumFacts>,
    artists: HashMap<ObjectId, ArtistEntry>,
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
        // in Artists mode) needs the same title/child-count resolution
        // `albums`/`albums_under_artist` already apply, via the same
        // `album_container` helper - so all three can never disagree.
        // In Artists mode specifically, its `parent_id` must also match
        // whatever artist it's actually grouped under, which can differ
        // from its real folder-parent once a tag redirects it.
        if matches!(
            self.mode,
            Mode::Albums | Mode::Artists | Mode::RecentlyAddedAlbums { .. }
        ) {
            let snapshot = self.snapshot();
            if snapshot.album_ids.contains(id) {
                let mut container = self.album_container(&snapshot, id)?;
                if matches!(self.mode, Mode::Artists)
                    && let Some(facts) = snapshot.album_facts.get(id)
                {
                    container.parent_id = Some(facts.artist_id.clone());
                }
                return Some(Entry::Container(container));
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

/// `None` if no track has a non-empty value for `field`, or if two
/// tracks disagree (case/whitespace-folded). A missing tag is a
/// non-vote, not a disagreement - deliberately conservative: a real
/// compilation folder (inconsistent artist tags) falls back to
/// folder-based grouping rather than guessing. See this module's doc
/// comment.
fn consistent_tag_value<'a>(
    tracks: &'a [Item],
    field: impl Fn(&'a Item) -> Option<&'a str>,
) -> Option<String> {
    let mut chosen: Option<&str> = None;
    let mut normalized_key: Option<String> = None;
    for item in tracks {
        let Some(raw) = field(item).map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let key = raw.to_lowercase();
        match &normalized_key {
            None => {
                normalized_key = Some(key);
                chosen = Some(raw);
            }
            Some(k) if *k == key => {}
            Some(_) => return None,
        }
    }
    chosen.map(str::to_string)
}

/// The comparison/dedup key for a tag artist name - trimmed and
/// lower-cased, so "AC/DC" and " ac/dc " group as the same artist. The
/// *display* title never goes through this; see `snapshot()`.
fn normalize_artist_key(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// A tag-derived artist has no backing real `Index` container, so it
/// needs its own id - a plain non-numeric string, which can never
/// collide with a real `Index`-allocated id (always a plain integer
/// string; see `IndexBuilder`'s `next_id`, `src/index.rs`).
fn synthetic_artist_id(normalized_key: &str) -> ObjectId {
    ObjectId::new(format!("tag-artist:{normalized_key}"))
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
    use crate::index::{IndexBuilder, TrackTags};
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

    fn tagged_item(
        builder: &mut IndexBuilder,
        parent: &ObjectId,
        title: &str,
        path: &str,
        artist: Option<&str>,
        album: Option<&str>,
    ) {
        builder.add_item_with_tags(
            parent,
            title.to_string(),
            PathBuf::from(path),
            1,
            SystemTime::UNIX_EPOCH,
            TrackTags {
                artist: artist.map(str::to_string),
                album: album.map(str::to_string),
                ..Default::default()
            },
        );
    }

    #[test]
    fn tag_artist_unifies_a_nested_folder_album_and_a_flat_top_level_album() {
        // The real-world case this feature exists for: the same artist
        // split across a proper nested tree and a flat top-level album
        // folder, unified by a consistent artist tag rather than folder
        // shape.
        let mut builder = IndexBuilder::new();
        let ac_dc = builder.add_container(&ObjectId::root(), "AC_DC".to_string());
        let tnt = builder.add_container(&ac_dc, "TNT".to_string());
        tagged_item(
            &mut builder,
            &tnt,
            "01 - Track.mp3",
            "/m/AC_DC/TNT/01.mp3",
            Some("AC/DC"),
            None,
        );
        let back_in_black =
            builder.add_container(&ObjectId::root(), "AC-DC - Back in Black".to_string());
        tagged_item(
            &mut builder,
            &back_in_black,
            "01 - Track.mp3",
            "/m/AC-DC - Back in Black/01.mp3",
            Some("AC/DC"),
            None,
        );

        let view = MusicLibraryView::new(SharedIndex::new(builder.build()), Mode::Artists);
        let artists = view.children(&ObjectId::root()).unwrap();
        assert_eq!(titles(&artists), vec!["AC/DC"]);
        let Entry::Container(artist) = &artists[0] else {
            panic!("expected a container");
        };
        assert_eq!(artist.child_count, 2);

        let albums = view.children(&artist.id).unwrap();
        let mut album_titles = titles(&albums);
        album_titles.sort_unstable();
        assert_eq!(album_titles, vec!["AC-DC - Back in Black", "TNT"]);
    }

    #[test]
    fn artist_tags_differing_only_in_case_or_whitespace_are_the_same_artist() {
        let mut builder = IndexBuilder::new();
        let album_a = builder.add_container(&ObjectId::root(), "Album A".to_string());
        tagged_item(
            &mut builder,
            &album_a,
            "track.mp3",
            "/m/a/track.mp3",
            Some("AC/DC"),
            None,
        );
        let album_b = builder.add_container(&ObjectId::root(), "Album B".to_string());
        tagged_item(
            &mut builder,
            &album_b,
            "track.mp3",
            "/m/b/track.mp3",
            Some(" ac/dc "),
            None,
        );

        let view = MusicLibraryView::new(SharedIndex::new(builder.build()), Mode::Artists);
        let artists = view.children(&ObjectId::root()).unwrap();
        assert_eq!(
            artists.len(),
            1,
            "differently-cased/spaced tags must merge into one artist"
        );
        let Entry::Container(artist) = &artists[0] else {
            panic!("expected a container");
        };
        assert_eq!(artist.child_count, 2);
        // Deterministic tie-break: the lexicographically smallest raw
        // variant wins, not whichever the HashMap happens to iterate
        // first. Uppercase sorts before lowercase in ASCII, so "AC/DC"
        // wins over the trimmed "ac/dc".
        assert_eq!(artist.title, "AC/DC");
    }

    #[test]
    fn inconsistent_artist_tags_fall_back_to_folder_grouping_not_a_crash() {
        let mut builder = IndexBuilder::new();
        let compilation = builder.add_container(&ObjectId::root(), "Compilation".to_string());
        tagged_item(
            &mut builder,
            &compilation,
            "01.mp3",
            "/m/c/01.mp3",
            Some("Alice"),
            None,
        );
        tagged_item(
            &mut builder,
            &compilation,
            "02.mp3",
            "/m/c/02.mp3",
            Some("Bob"),
            None,
        );
        let index = SharedIndex::new(builder.build());

        let albums_view = MusicLibraryView::new(index.clone(), Mode::Albums);
        assert_eq!(
            titles(&albums_view.children(&ObjectId::root()).unwrap()),
            vec!["Compilation"]
        );

        let artists_view = MusicLibraryView::new(index, Mode::Artists);
        // The compilation's own folder-parent is root - a true orphan
        // under the folder heuristic - and disagreeing artist tags must
        // not invent a fabricated single-artist bucket either. Real
        // "Various Artists" handling is a separate, deferred feature.
        assert_eq!(
            artists_view.children(&ObjectId::root()).unwrap(),
            Vec::new()
        );
    }

    #[test]
    fn consistent_album_tag_overrides_the_raw_folder_name_for_display() {
        let mut builder = IndexBuilder::new();
        let folder = builder.add_container(&ObjectId::root(), "raw-folder-name".to_string());
        tagged_item(
            &mut builder,
            &folder,
            "01.mp3",
            "/m/f/01.mp3",
            None,
            Some("Real Album Title"),
        );
        tagged_item(
            &mut builder,
            &folder,
            "02.mp3",
            "/m/f/02.mp3",
            None,
            Some("Real Album Title"),
        );

        let view = MusicLibraryView::new(SharedIndex::new(builder.build()), Mode::Albums);
        let albums = view.children(&ObjectId::root()).unwrap();
        assert_eq!(titles(&albums), vec!["Real Album Title"]);
        let album_id = albums[0].id().clone();

        // Mode::Albums's listing and entry()'s direct by-id lookup must
        // agree - the whole reason album_container() is the single
        // place every album Container gets built.
        let Some(Entry::Container(direct)) = view.entry(&album_id) else {
            panic!("expected a container");
        };
        assert_eq!(direct.title, "Real Album Title");
    }

    #[test]
    fn inconsistent_album_tag_falls_back_to_the_folder_name() {
        let mut builder = IndexBuilder::new();
        let folder = builder.add_container(&ObjectId::root(), "raw-folder-name".to_string());
        tagged_item(
            &mut builder,
            &folder,
            "01.mp3",
            "/m/f/01.mp3",
            None,
            Some("Title One"),
        );
        tagged_item(
            &mut builder,
            &folder,
            "02.mp3",
            "/m/f/02.mp3",
            None,
            Some("Title Two"),
        );

        let view = MusicLibraryView::new(SharedIndex::new(builder.build()), Mode::Albums);
        let albums = view.children(&ObjectId::root()).unwrap();
        assert_eq!(titles(&albums), vec!["raw-folder-name"]);
    }

    #[test]
    fn tag_driven_artist_grouping_stays_consistent_under_entry_and_children() {
        // The strongest guard on the parent_id-reparenting correctness
        // fix: walk_and_check_consistency already exists for the
        // untagged proptest below, but that one never exercises real
        // tags. Run it here against a fixture that forces tag-driven
        // artist grouping across two differently-shaped real folders.
        let mut builder = IndexBuilder::new();
        let ac_dc = builder.add_container(&ObjectId::root(), "AC_DC".to_string());
        let tnt = builder.add_container(&ac_dc, "TNT".to_string());
        tagged_item(
            &mut builder,
            &tnt,
            "01.mp3",
            "/m/AC_DC/TNT/01.mp3",
            Some("AC/DC"),
            None,
        );
        let flat = builder.add_container(&ObjectId::root(), "Flat Album".to_string());
        tagged_item(
            &mut builder,
            &flat,
            "01.mp3",
            "/m/flat/01.mp3",
            Some("AC/DC"),
            None,
        );

        let view = MusicLibraryView::new(SharedIndex::new(builder.build()), Mode::Artists);
        walk_and_check_consistency(&view);
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
        SharedIndex::new(crate::scanner::scan(&media, &FilenameMetadata))
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

    /// 200 artists, 5 albums each, 10 tracks each: 10,000 tracks, 1,000
    /// albums. The same shape that caught bug 4 in docs/PLAN.md's Phase 8
    /// section - a real media server hung on a plain root Browse because
    /// `tracks_of_album`/`albums_under_artist` each re-walked the whole
    /// index once per album or artist, turning a Browse of 1,000 albums
    /// into roughly 1,000 full re-walks of the index. Built through
    /// `IndexBuilder` directly, not the real scanner: this test is about
    /// `MusicLibraryView`'s own complexity, not disk I/O.
    fn large_library_fixture() -> SharedIndex {
        let mut builder = IndexBuilder::new();
        for artist_n in 0..200 {
            let artist = builder.add_container(&ObjectId::root(), format!("Artist {artist_n}"));
            for album_n in 0..5 {
                let album = builder.add_container(&artist, format!("Album {album_n}"));
                for track_n in 0..10 {
                    builder.add_item(
                        &album,
                        format!("{track_n:02} - Track.mp3"),
                        PathBuf::from(format!(
                            "/m/artist-{artist_n}/album-{album_n}/track-{track_n}.mp3"
                        )),
                        1,
                        SystemTime::UNIX_EPOCH,
                    );
                }
            }
        }
        SharedIndex::new(builder.build())
    }

    /// Fails if browsing a 10,000-track library takes more than a
    /// generous fraction of a second - not a tight benchmark, a tripwire.
    /// Measured on ordinary dev hardware right after the bug-4 fix: a
    /// release-mode root Browse for Albums or Artists took ~30ms, and
    /// browsing one artist's five albums took ~5ms. This threshold gives
    /// roughly two orders of magnitude of headroom over that, so it stays
    /// green on a slower or loaded machine in debug mode, but still fails
    /// hard the moment someone reintroduces an O(albums) or O(tracks)
    /// re-walk per item - the exact shape of bug 4. A change that
    /// legitimately needs more time than this should raise that as a
    /// real, visible tradeoff, not slip in unnoticed.
    const MAX_BROWSE_TIME: Duration = Duration::from_millis(1500);

    fn assert_fast<T>(label: &str, f: impl FnOnce() -> T) -> T {
        let started = std::time::Instant::now();
        let result = f();
        let elapsed = started.elapsed();
        assert!(
            elapsed < MAX_BROWSE_TIME,
            "{label} took {elapsed:?}, expected under {MAX_BROWSE_TIME:?} - \
             see docs/PLAN.md Phase 8 bug 4 (a re-walk of the whole index \
             per album/artist snuck back in?)"
        );
        result
    }

    #[test]
    fn browsing_a_large_library_stays_fast() {
        let index = large_library_fixture();

        let albums_view = MusicLibraryView::new(index.clone(), Mode::Albums);
        let albums = assert_fast("Albums root browse (1,000 albums)", || {
            albums_view.children(&ObjectId::root()).unwrap()
        });
        assert_eq!(albums.len(), 1000);

        let artists_view = MusicLibraryView::new(index.clone(), Mode::Artists);
        let artists = assert_fast("Artists root browse (200 artists)", || {
            artists_view.children(&ObjectId::root()).unwrap()
        });
        assert_eq!(artists.len(), 200);

        let one_artist_id = artists[0].id().clone();
        let albums_for_one_artist = assert_fast("browsing one artist's albums", || {
            artists_view.children(&one_artist_id).unwrap()
        });
        assert_eq!(albums_for_one_artist.len(), 5);

        let one_album_id = albums[0].id().clone();
        let tracks_for_one_album = assert_fast("browsing one album's tracks", || {
            albums_view.children(&one_album_id).unwrap()
        });
        assert_eq!(tracks_for_one_album.len(), 10);
    }
}
