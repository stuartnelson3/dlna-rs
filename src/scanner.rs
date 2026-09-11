//! Walks the configured media directories and builds an [`Index`] from
//! what's actually on disk. Uses `walkdir` rather than hand-rolled
//! recursion specifically for its symlink-loop handling (see the crate
//! table in the private planning notes) — a naive recursive walk with
//! `follow_symlinks` on could recurse forever on a cyclic symlink.

use std::path::Path;
use std::time::SystemTime;

use walkdir::WalkDir;

use crate::config::MediaConfig;
use crate::core::didl::format::is_audio_extension;
use crate::core::metadata_provider::MetadataProvider;
use crate::index::{Index, IndexBuilder, ObjectId, TrackTags};

/// `tags` reads each file's real metadata once, here, at scan time - see
/// `core::metadata_provider`'s doc comment for why that has to happen
/// here and not on every Browse call.
pub fn scan(media: &MediaConfig, tags: &dyn MetadataProvider) -> Index {
    let mut builder = IndexBuilder::new();
    for dir in &media.directories {
        scan_one_directory(
            &mut builder,
            &dir.path,
            media.follow_symlinks,
            &media.exclude_patterns,
            tags,
        );
    }
    builder.build()
}

/// Reconstructs the tree from `WalkDir`'s flat, depth-first-pre-order
/// iteration by remembering which container we most recently created at
/// each depth: an entry at depth *d* is always a child of whatever we
/// last saw at depth *d-1*, because depth-first order visits an entire
/// subtree before backtracking out of it.
fn scan_one_directory(
    builder: &mut IndexBuilder,
    path: &Path,
    follow_symlinks: bool,
    exclude_patterns: &[String],
    tags: &dyn MetadataProvider,
) {
    let mut container_at_depth: Vec<ObjectId> = Vec::new();

    // depth 0 is the configured directory itself - the user chose that
    // path deliberately, so exclude_patterns applies to its descendants
    // only. Otherwise a pattern like ".*" would silently exclude an
    // entire configured directory whose own name happens to start with a
    // dot, with no error to say why the library came up empty.
    let walker = WalkDir::new(path)
        .follow_links(follow_symlinks)
        .into_iter()
        .filter_entry(|entry| entry.depth() == 0 || !is_excluded(entry, exclude_patterns));

    for entry in walker.filter_map(Result::ok) {
        let depth = entry.depth();
        let parent = if depth == 0 {
            ObjectId::root()
        } else {
            container_at_depth[depth - 1].clone()
        };
        let title = entry
            .file_name()
            .to_str()
            .map(str::to_string)
            .unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned());

        if entry.file_type().is_dir() {
            let id = builder.add_container(&parent, title);
            container_at_depth.truncate(depth);
            container_at_depth.push(id);
        } else if entry.file_type().is_file()
            && is_audio_extension(entry.path())
            && let Ok(metadata) = entry.metadata()
        {
            // `read.title` is the parsed title (track number and
            // extension stripped, e.g. "01 - Track.mp3" -> "Track") -
            // not the raw filename computed above, which containers use
            // as-is. Track number itself isn't used here: it only
            // matters for in-album sort order, read fresh at Browse
            // time by `content::music_library`.
            let read = tags.metadata(entry.path());
            let track_tags = TrackTags {
                artist: read.artist,
                album: read.album,
                genre: read.genre,
                has_art: read.has_art,
                duration_millis: read.duration_millis,
                bitrate: read.bitrate,
                sample_rate: read.sample_rate,
                bits_per_sample: read.bits_per_sample,
                channels: read.channels,
            };
            builder.add_item_with_tags(
                &parent,
                read.title,
                entry.path().to_path_buf(),
                metadata.len(),
                metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                track_tags,
            );
        }
        // Anything else - a non-audio file, a symlink left unresolved
        // because follow_symlinks is off, a file we couldn't stat - is
        // silently skipped, not an error. A media directory is expected
        // to have junk in it (album art, .nfo files, whatever); the
        // scanner's job is to find the audio, not to complain about the
        // rest.
    }
}

fn is_excluded(entry: &walkdir::DirEntry, patterns: &[String]) -> bool {
    let name = entry.file_name().to_string_lossy();
    patterns.iter().any(|pattern| glob_match(pattern, &name))
}

/// A minimal `*`-only glob matcher for `exclude_patterns`. These come from
/// trusted local config, not the network, so this doesn't need to be on
/// the fuzzed/property-tested list from docs/THREAT_MODEL.md - it just
/// needs to be correct for the patterns config.rs's own docs promise
/// (`"*.tmp"`, `".*"`).
fn glob_match(pattern: &str, text: &str) -> bool {
    let segments: Vec<&str> = pattern.split('*').collect();
    if segments.len() == 1 {
        return text == pattern;
    }

    let last = segments.len() - 1;
    let mut rest = text;
    for (i, segment) in segments.iter().enumerate() {
        if segment.is_empty() {
            continue;
        }
        if i == 0 {
            if !rest.starts_with(segment) {
                return false;
            }
            rest = &rest[segment.len()..];
        } else if i == last {
            return rest.ends_with(segment);
        } else {
            match rest.find(segment) {
                Some(pos) => rest = &rest[pos + segment.len()..],
                None => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MediaDirectory, MediaKind};
    use crate::index::Entry;
    use crate::metadata::filename::FilenameMetadata;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn glob_match_examples() {
        assert!(glob_match("*.tmp", "foo.tmp"));
        assert!(!glob_match("*.tmp", "footmp"));
        assert!(glob_match(".*", ".DS_Store"));
        assert!(!glob_match(".*", "DS_Store"));
        assert!(glob_match("Thumbs.db", "Thumbs.db"));
        assert!(!glob_match("Thumbs.db", "thumbs.db"));
        assert!(glob_match("a*b*c", "aXbYc"));
        assert!(!glob_match("a*b*c", "aXbY"));
        assert!(glob_match("*", "anything"));
    }

    struct Fixture {
        _dir: TempDir,
        root: std::path::PathBuf,
    }

    fn fixture(build: impl FnOnce(&Path)) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        build(dir.path());
        Fixture {
            root: dir.path().to_path_buf(),
            _dir: dir,
        }
    }

    fn media_config(root: &Path, exclude_patterns: &[&str]) -> MediaConfig {
        MediaConfig {
            directories: vec![MediaDirectory {
                path: root.to_path_buf(),
                kind: MediaKind::Audio,
            }],
            follow_symlinks: false,
            exclude_patterns: exclude_patterns.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn container_named(
        index: &Index,
        parent: &ObjectId,
        name: &str,
    ) -> Option<crate::index::Container> {
        index
            .children(parent)?
            .into_iter()
            .find_map(|entry| match entry {
                Entry::Container(c) if c.title == name => Some(c),
                _ => None,
            })
    }

    #[test]
    fn exact_fixture_tree_matches_expected_shape() {
        let f = fixture(|root| {
            fs::create_dir_all(root.join("Artist/Album")).unwrap();
            fs::write(root.join("Artist/Album/01 - First.flac"), b"").unwrap();
            fs::write(root.join("Artist/Album/02 - Second.mp3"), b"").unwrap();
            fs::write(root.join("Artist/Album/cover.jpg"), b"").unwrap();
            fs::write(root.join("Artist/Album/.DS_Store"), b"").unwrap();
        });
        let config = media_config(&f.root, &["*.tmp", ".*"]);
        let index = scan(&config, &FilenameMetadata);

        let top = container_named(&index, &ObjectId::root(), &top_level_name(&f.root)).unwrap();
        assert_eq!(top.child_count, 1);

        let artist = container_named(&index, &top.id, "Artist").unwrap();
        assert_eq!(artist.child_count, 1);

        let album = container_named(&index, &artist.id, "Album").unwrap();
        // Two audio files only: cover.jpg isn't audio, .DS_Store is excluded.
        assert_eq!(album.child_count, 2);

        let children = index.children(&album.id).unwrap();
        let mut titles: Vec<&str> = children
            .iter()
            .map(|e| match e {
                Entry::Item(i) => i.title.as_str(),
                Entry::Container(c) => c.title.as_str(),
            })
            .collect();
        titles.sort_unstable();
        // The parsed title (track number and extension stripped), not
        // the raw filename - see `scan_one_directory`'s item branch.
        assert_eq!(titles, vec!["First", "Second"]);
    }

    fn top_level_name(path: &Path) -> String {
        path.file_name().unwrap().to_string_lossy().into_owned()
    }

    /// A real, tagged MP3: 30 repeats of one silent MPEG-1 Layer III
    /// frame (lofty needs several consistent frames to trust the file
    /// is really an MP3 - one alone isn't enough, verified directly),
    /// with a real artist/album/genre tag written via lofty's own API.
    fn write_tagged_mp3(path: &std::path::Path) {
        let mut frame = vec![0xFFu8, 0xFB, 0x90, 0x00];
        frame.resize(417, 0);
        fs::write(path, frame.repeat(30)).unwrap();

        use lofty::config::WriteOptions;
        use lofty::file::{AudioFile, TaggedFileExt};
        use lofty::tag::{Accessor, Tag};

        let mut tagged_file = lofty::read_from_path(path).unwrap();
        if tagged_file.primary_tag().is_none() {
            tagged_file.insert_tag(Tag::new(tagged_file.primary_tag_type()));
        }
        let tag = tagged_file.primary_tag_mut().unwrap();
        tag.set_artist("Scan Test Artist".to_string());
        tag.set_album("Scan Test Album".to_string());
        tagged_file
            .save_to_path(path, WriteOptions::default())
            .unwrap();
    }

    #[test]
    fn scanning_with_tag_metadata_populates_the_items_real_tags() {
        let f = fixture(|root| write_tagged_mp3(&root.join("track.mp3")));
        let config = media_config(&f.root, &[]);

        let tags = crate::metadata::tags::TagMetadata::new(vec![f.root.clone()]);
        let index = scan(&config, &tags);

        let top = container_named(&index, &ObjectId::root(), &top_level_name(&f.root)).unwrap();
        let children = index.children(&top.id).unwrap();
        let Entry::Item(item) = &children[0] else {
            panic!("expected an item");
        };
        assert_eq!(item.artist.as_deref(), Some("Scan Test Artist"));
        assert_eq!(item.album.as_deref(), Some("Scan Test Album"));
    }

    /// Not a tight benchmark - a tripwire, same philosophy as
    /// `music_library.rs`'s own `browsing_a_large_library_stays_fast`:
    /// a generous margin that stays green on a slower or loaded
    /// machine, but fails hard the moment the persistent tag cache
    /// stops actually skipping the `lofty` parse on an unchanged file.
    /// Exercises the real, reported complaint's exact code path -
    /// `scanner::scan` against a `CachedTagMetadata`, not an isolated
    /// cache-only micro-benchmark.
    #[test]
    fn a_second_scan_with_a_warm_cache_is_meaningfully_faster_than_the_first() {
        const FILE_COUNT: usize = 300;

        let f = fixture(|root| {
            for n in 0..FILE_COUNT {
                write_tagged_mp3(&root.join(format!("track-{n:04}.mp3")));
            }
        });
        let config = media_config(&f.root, &[]);

        let cache_dir = tempfile::tempdir().unwrap();
        let tags = crate::metadata::tag_cache::CachedTagMetadata::open(
            &cache_dir.path().join("cache.redb"),
            crate::metadata::tags::TagMetadata::new(vec![f.root.clone()]),
        )
        .unwrap();

        let cold_started = std::time::Instant::now();
        let cold_index = scan(&config, &tags);
        let cold_elapsed = cold_started.elapsed();

        let warm_started = std::time::Instant::now();
        let warm_index = scan(&config, &tags);
        let warm_elapsed = warm_started.elapsed();

        assert_eq!(cold_index.len(), warm_index.len());
        println!("cold scan ({FILE_COUNT} files): {cold_elapsed:?}, warm scan: {warm_elapsed:?}");
        assert!(
            warm_elapsed * 2 < cold_elapsed,
            "expected the warm (cached) scan to be at least 2x faster than the \
             cold one - cold={cold_elapsed:?} warm={warm_elapsed:?}"
        );
    }

    #[test]
    fn scanning_with_filename_metadata_leaves_tags_empty() {
        // The default used everywhere else in this file's tests - real
        // tag fields stay None, matching FilenameMetadata's contract.
        let f = fixture(|root| write_tagged_mp3(&root.join("track.mp3")));
        let config = media_config(&f.root, &[]);

        let index = scan(&config, &FilenameMetadata);

        let top = container_named(&index, &ObjectId::root(), &top_level_name(&f.root)).unwrap();
        let children = index.children(&top.id).unwrap();
        let Entry::Item(item) = &children[0] else {
            panic!("expected an item");
        };
        assert_eq!(item.artist, None);
    }

    #[test]
    fn excluded_directory_is_not_descended_into() {
        let f = fixture(|root| {
            fs::create_dir_all(root.join(".git")).unwrap();
            fs::write(root.join(".git/config"), b"").unwrap();
            fs::write(root.join("song.mp3"), b"").unwrap();
        });
        let config = media_config(&f.root, &[".*"]);
        let index = scan(&config, &FilenameMetadata);

        let top = container_named(&index, &ObjectId::root(), &top_level_name(&f.root)).unwrap();
        assert_eq!(top.child_count, 1, "only song.mp3 should remain");
    }

    #[test]
    fn multiple_configured_directories_each_get_a_top_level_container() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        fs::write(a.path().join("one.mp3"), b"").unwrap();
        fs::write(b.path().join("two.mp3"), b"").unwrap();

        let config = MediaConfig {
            directories: vec![
                MediaDirectory {
                    path: a.path().to_path_buf(),
                    kind: MediaKind::Audio,
                },
                MediaDirectory {
                    path: b.path().to_path_buf(),
                    kind: MediaKind::Audio,
                },
            ],
            follow_symlinks: false,
            exclude_patterns: vec![],
        };
        let index = scan(&config, &FilenameMetadata);

        let root_children = index.children(&ObjectId::root()).unwrap();
        assert_eq!(root_children.len(), 2);
    }

    #[test]
    fn empty_directory_scans_to_an_empty_root() {
        let f = fixture(|_root| {});
        let config = media_config(&f.root, &[]);
        let index = scan(&config, &FilenameMetadata);
        let top = container_named(&index, &ObjectId::root(), &top_level_name(&f.root)).unwrap();
        assert_eq!(top.child_count, 0);
    }

    /// Walking every container reachable from root and confirming each
    /// child both exists and points its `parent_id` straight back:
    /// exactly the "never a broken parent/child link" property from
    /// docs/PLAN.md Phase 4.
    fn assert_index_is_consistent(index: &Index) {
        let mut stack = vec![ObjectId::root()];
        let mut visited = std::collections::HashSet::new();

        while let Some(id) = stack.pop() {
            if !visited.insert(id.clone()) {
                continue;
            }
            let entry = index.entry(&id).unwrap_or_else(|| {
                panic!("dangling id {id}: listed as a child but missing from the index")
            });
            let Entry::Container(_) = entry else {
                continue;
            };
            for child in index.children(&id).unwrap() {
                match &child {
                    Entry::Container(c) => {
                        assert_eq!(
                            c.parent_id.as_ref(),
                            Some(&id),
                            "container {} has the wrong parent_id",
                            c.id
                        )
                    }
                    Entry::Item(i) => {
                        assert_eq!(i.parent_id, id, "item {} has the wrong parent_id", i.id)
                    }
                }
                stack.push(child.id().clone());
            }
        }
    }

    fn path_component() -> impl proptest::strategy::Strategy<Value = String> {
        "[a-zA-Z0-9_]{1,8}"
    }

    proptest::proptest! {
        /// An arbitrary set of nested paths (shared prefixes become shared
        /// directories, so this generates real varied tree shapes, not
        /// just flat file lists), some with audio extensions and some
        /// without. `scan` should never panic on any of it, and the
        /// result should always be internally consistent.
        #[test]
        fn scanning_an_arbitrary_tree_never_panics_and_has_no_dangling_links(
            entries in proptest::collection::vec(
                (proptest::collection::vec(path_component(), 1..4), proptest::bool::ANY),
                1..20,
            )
        ) {
            let dir = tempfile::tempdir().unwrap();
            for (segments, is_audio) in &entries {
                let mut full = dir.path().to_path_buf();
                for segment in &segments[..segments.len() - 1] {
                    full.push(segment);
                }
                fs::create_dir_all(&full).unwrap();
                let extension = if *is_audio { "mp3" } else { "txt" };
                full.push(format!("{}.{extension}", segments.last().unwrap()));
                fs::write(&full, b"").unwrap();
            }

            let config = media_config(dir.path(), &["*.tmp", ".*"]);
            let index = scan(&config, &FilenameMetadata);
            assert_index_is_consistent(&index);
        }
    }
}
