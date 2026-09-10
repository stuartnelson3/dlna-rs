//! `CompositeContentSource`: mounts several `ContentSource`s as siblings
//! under one root, the way MiniDLNA mounts "Music/Folders",
//! "Music/Albums", and so on side by side.
//!
//! Each mount gets a short prefix, such as `"albums"`. Every ID that
//! crosses this source carries its mount's prefix, joined with `$`:
//! `"albums$42"` means "ID 42 inside the albums mount." A Browse call for
//! an unknown prefix, or for a plain ID with no mount at all, returns
//! `None` — the same fail-closed rule every other `ContentSource` in this
//! project follows (see docs/THREAT_MODEL.md).

use crate::config::{LibraryConfig, RecentlyAddedConfig, View};
use crate::content::folder::FolderMirror;
use crate::content::music_library::{Mode, MusicLibraryView};
use crate::core::content_source::ContentSource;
use crate::index::{Container, Entry, ObjectId, SharedIndex};

struct Mount {
    prefix: &'static str,
    title: &'static str,
    source: Box<dyn ContentSource>,
}

impl Mount {
    fn new(
        prefix: &'static str,
        title: &'static str,
        source: impl ContentSource + 'static,
    ) -> Mount {
        Mount {
            prefix,
            title,
            source: Box::new(source),
        }
    }
}

pub struct CompositeContentSource {
    mounts: Vec<Mount>,
}

impl CompositeContentSource {
    /// Builds one mount per entry in `library.views`, each reading the
    /// same `index`. A rescan replaces that index once, and every mount
    /// sees the change right away — see `SharedIndex`.
    pub fn from_config(library: &LibraryConfig, index: SharedIndex) -> CompositeContentSource {
        let mounts = library
            .views
            .iter()
            .map(|view| mount_for(*view, &library.recently_added, index.clone()))
            .collect();
        CompositeContentSource { mounts }
    }

    fn top_level(&self) -> Vec<Entry> {
        self.mounts
            .iter()
            .map(|mount| {
                let child_count = mount
                    .source
                    .children(&ObjectId::root())
                    .map(|children| children.len())
                    .unwrap_or(0);
                Entry::Container(Container {
                    id: prefixed(mount.prefix, &ObjectId::root()),
                    parent_id: Some(ObjectId::root()),
                    title: mount.title.to_string(),
                    child_count,
                })
            })
            .collect()
    }

    fn mount_and_local_id(&self, id: &ObjectId) -> Option<(&Mount, ObjectId)> {
        let (prefix, rest) = id.as_str().split_once('$')?;
        let mount = self.mounts.iter().find(|mount| mount.prefix == prefix)?;
        Some((mount, ObjectId::new(rest)))
    }
}

impl ContentSource for CompositeContentSource {
    fn children(&self, id: &ObjectId) -> Option<Vec<Entry>> {
        if *id == ObjectId::root() {
            return Some(self.top_level());
        }
        let (mount, local_id) = self.mount_and_local_id(id)?;
        let children = mount.source.children(&local_id)?;
        Some(
            children
                .into_iter()
                .map(|entry| rewrap(mount.prefix, entry))
                .collect(),
        )
    }

    fn entry(&self, id: &ObjectId) -> Option<Entry> {
        if *id == ObjectId::root() {
            return Some(Entry::Container(Container {
                id: ObjectId::root(),
                parent_id: None,
                title: String::new(),
                child_count: self.mounts.len(),
            }));
        }
        let (mount, local_id) = self.mount_and_local_id(id)?;
        Some(rewrap(mount.prefix, mount.source.entry(&local_id)?))
    }
}

fn mount_for(view: View, recently_added: &RecentlyAddedConfig, index: SharedIndex) -> Mount {
    match view {
        View::Folders => Mount::new("folders", "Folders", FolderMirror::new(index)),
        View::AllSongs => Mount::new(
            "songs",
            "All Songs",
            MusicLibraryView::new(index, Mode::AllSongs),
        ),
        View::Albums => Mount::new(
            "albums",
            "Albums",
            MusicLibraryView::new(index, Mode::Albums),
        ),
        View::Artists => Mount::new(
            "artists",
            "Artists",
            MusicLibraryView::new(index, Mode::Artists),
        ),
        View::RecentlyAddedSongs => Mount::new(
            "recent_songs",
            "Recently Added Songs",
            MusicLibraryView::new(
                index,
                Mode::RecentlyAddedSongs {
                    count: recently_added.songs_count,
                    max_age_days: recently_added.max_age_days,
                },
            ),
        ),
        View::RecentlyAddedAlbums => Mount::new(
            "recent_albums",
            "Recently Added Albums",
            MusicLibraryView::new(
                index,
                Mode::RecentlyAddedAlbums {
                    count: recently_added.albums_count,
                    max_age_days: recently_added.max_age_days,
                },
            ),
        ),
    }
}

fn prefixed(prefix: &str, id: &ObjectId) -> ObjectId {
    ObjectId::new(format!("{prefix}${id}"))
}

/// Rewrites a mount's own IDs into composite-space IDs, by adding the
/// mount's prefix to `id` and to `parent_id`. A mount's own root has no
/// parent (`parent_id: None`); in composite space its parent is the
/// overall root, `"0"`.
fn rewrap(prefix: &str, entry: Entry) -> Entry {
    match entry {
        Entry::Container(mut container) => {
            container.id = prefixed(prefix, &container.id);
            container.parent_id = Some(match container.parent_id {
                Some(parent) => prefixed(prefix, &parent),
                None => ObjectId::root(),
            });
            Entry::Container(container)
        }
        Entry::Item(mut item) => {
            item.id = prefixed(prefix, &item.id);
            item.parent_id = prefixed(prefix, &item.parent_id);
            Entry::Item(item)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::IndexBuilder;

    fn library_with(views: Vec<View>) -> LibraryConfig {
        LibraryConfig {
            views,
            recently_added: RecentlyAddedConfig::default(),
        }
    }

    fn small_index() -> SharedIndex {
        let mut builder = IndexBuilder::new();
        let artist = builder.add_container(&ObjectId::root(), "Artist".to_string());
        let album = builder.add_container(&artist, "Album".to_string());
        builder.add_item(
            &album,
            "01 - Track.mp3".to_string(),
            std::path::PathBuf::from("/m/Artist/Album/01 - Track.mp3"),
            1,
            std::time::SystemTime::now(),
        );
        SharedIndex::new(builder.build())
    }

    #[test]
    fn root_lists_exactly_the_configured_views_in_order() {
        let library = library_with(vec![View::Folders, View::Albums, View::Artists]);
        let composite = CompositeContentSource::from_config(&library, small_index());

        let root = composite.children(&ObjectId::root()).unwrap();
        let titles: Vec<&str> = root
            .iter()
            .map(|entry| match entry {
                Entry::Container(c) => c.title.as_str(),
                Entry::Item(_) => panic!("root should hold only containers"),
            })
            .collect();
        assert_eq!(titles, vec!["Folders", "Albums", "Artists"]);
    }

    #[test]
    fn browsing_into_a_mount_reaches_its_real_content() {
        let library = library_with(vec![View::Folders]);
        let composite = CompositeContentSource::from_config(&library, small_index());

        let root = composite.children(&ObjectId::root()).unwrap();
        let folders_id = root[0].id().clone();
        assert_eq!(folders_id.as_str(), "folders$0");

        let artists = composite.children(&folders_id).unwrap();
        assert_eq!(artists.len(), 1);
        let artist_id = artists[0].id().clone();
        assert_eq!(artist_id.as_str(), "folders$1");

        let albums = composite.children(&artist_id).unwrap();
        let album_id = albums[0].id().clone();
        let tracks = composite.children(&album_id).unwrap();
        assert_eq!(tracks.len(), 1);
    }

    #[test]
    fn an_id_with_no_matching_mount_prefix_is_not_found() {
        let library = library_with(vec![View::Folders]);
        let composite = CompositeContentSource::from_config(&library, small_index());

        assert_eq!(composite.children(&ObjectId::new("bogus$0")), None);
        assert_eq!(composite.entry(&ObjectId::new("bogus$0")), None);
    }

    #[test]
    fn an_id_with_no_dollar_separator_is_not_found() {
        let library = library_with(vec![View::Folders]);
        let composite = CompositeContentSource::from_config(&library, small_index());

        assert_eq!(composite.children(&ObjectId::new("42")), None);
    }

    #[test]
    fn a_valid_prefix_with_an_id_the_mount_does_not_recognize_is_not_found() {
        let library = library_with(vec![View::Folders]);
        let composite = CompositeContentSource::from_config(&library, small_index());

        assert_eq!(composite.children(&ObjectId::new("folders$9999")), None);
    }

    #[test]
    fn every_view_kind_mounts_without_panicking() {
        let library = library_with(vec![
            View::Folders,
            View::AllSongs,
            View::Albums,
            View::Artists,
            View::RecentlyAddedSongs,
            View::RecentlyAddedAlbums,
        ]);
        let composite = CompositeContentSource::from_config(&library, small_index());
        assert_eq!(composite.children(&ObjectId::root()).unwrap().len(), 6);
    }
}
