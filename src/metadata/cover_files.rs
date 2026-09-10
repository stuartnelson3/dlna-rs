//! Finds a cover-art file next to a track, using the filesystem
//! conventions real music collections actually use: a loose `cover.jpg`
//! or `folder.png` beside the tracks, a whole image file with some other
//! name, or an `Artwork`/`Scans`-style subfolder holding one. Checked
//! only when no embedded picture exists — see `metadata::tags`.
//!
//! Pure filesystem lookup, no format parsing: this module knows nothing
//! about audio tags and doesn't depend on `lofty`.
//!
//! The lookup order below isn't a guess. A survey script run against a
//! real ~980-album library found more matches this way than through
//! embedded pictures alone: named files and art-hinted subfolders
//! covered roughly half the library on their own, with a further chunk
//! explained by multi-disc albums (`Album/CD1/track.flac`) that keep
//! their art one level up, beside the disc folders rather than inside
//! them. Both directory levels get the same three checks, in the same
//! order, for that reason.

use std::fs::{self, DirEntry};
use std::path::{Path, PathBuf};

const IMAGE_EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "png"];
const NAMED_BASES: [&str; 7] = [
    "cover", "folder", "front", "albumart", "album", "art", "thumb",
];
const ART_SUBDIR_HINTS: [&str; 3] = ["art", "cover", "scan"];

/// The cover image for the track at `track_path`, or `None` if nothing
/// in its directory or the directory above it matches. Fails closed: a
/// directory this process can't read is treated the same as an empty
/// one, never an error.
///
/// `track_path`'s own directory is always inside `roots` already - it
/// came from a file the scanner found by walking a configured directory,
/// or an item path the HTTP layer already checked with the equivalent
/// `core::http::resolve_within_roots`. Climbing one level up (for a
/// multi-disc album whose art sits beside its disc subfolders, not
/// inside them) is the one step that could step outside a configured
/// root - when the track's own directory already *is* one - so that
/// step alone is checked against `roots` before it runs.
pub(crate) fn find_cover_file(track_path: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
    let dir = track_path.parent()?;
    find_in_dir(dir).or_else(|| {
        let parent = dir.parent()?;
        is_within_roots(parent, roots).then(|| find_in_dir(parent))?
    })
}

/// Mirrors `core::http::resolve_within_roots`'s check (canonicalize,
/// then confirm the result starts with one of `roots`) but stays local
/// to this module: that function is private to `core::http` and async,
/// while this lookup runs synchronously from both scan time and request
/// time. `roots` themselves are canonicalized once, in `TagMetadata::new`.
fn is_within_roots(path: &Path, roots: &[PathBuf]) -> bool {
    std::fs::canonicalize(path)
        .map(|resolved| roots.iter().any(|root| resolved.starts_with(root)))
        .unwrap_or(false)
}

/// The MIME type for a cover file's own extension. Separate from
/// `core::didl::format::mime_for`, which maps *audio* extensions - an
/// image file has no place in that table.
pub(crate) fn mime_for_cover_file(path: &Path) -> &'static str {
    match extension_lowercase(path).as_deref() {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

fn find_in_dir(dir: &Path) -> Option<PathBuf> {
    let entries = list_dir(dir);
    named_file(&entries)
        .or_else(|| loose_image(&entries))
        .or_else(|| art_subdir_image(&entries))
}

struct ListedEntry {
    path: PathBuf,
    lower_name: String,
    lower_ext: Option<String>,
    is_dir: bool,
}

fn list_dir(dir: &Path) -> Vec<ListedEntry> {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<ListedEntry> = read_dir.filter_map(Result::ok).map(describe).collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries
}

fn describe(entry: DirEntry) -> ListedEntry {
    let path = entry.path();
    let lower_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_lowercase();
    let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
    ListedEntry {
        lower_ext: extension_lowercase(&path),
        lower_name,
        is_dir,
        path,
    }
}

fn extension_lowercase(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
}

fn is_image(entry: &ListedEntry) -> bool {
    !entry.is_dir
        && entry
            .lower_ext
            .as_deref()
            .is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext))
}

/// `cover.jpg`, `Folder.PNG`, and so on - an image whose name (ignoring
/// extension and case) is exactly one of the recognized conventions.
fn named_file(entries: &[ListedEntry]) -> Option<PathBuf> {
    NAMED_BASES.iter().find_map(|base| {
        entries
            .iter()
            .find(|e| is_image(e) && e.lower_name.starts_with(&format!("{base}.")))
            .map(|e| e.path.clone())
    })
}

/// Any image file at all, taken in sorted order for a deterministic
/// pick - covers arbitrarily-named single scans (`AlbumArt_Large.jpg`
/// from Windows Media Player, a bare `folder1.jpg`, and the like).
fn loose_image(entries: &[ListedEntry]) -> Option<PathBuf> {
    entries.iter().find(|e| is_image(e)).map(|e| e.path.clone())
}

/// An `Artwork`/`Scans`/`Covers`-ish subfolder, checked for a loose
/// image the same way as the parent. Real folder names vary too much to
/// enumerate ("Artwork", "Scans covers", "-scans"), so this matches by
/// substring rather than a fixed list.
fn art_subdir_image(entries: &[ListedEntry]) -> Option<PathBuf> {
    let subdir = entries.iter().find(|e| {
        e.is_dir
            && ART_SUBDIR_HINTS
                .iter()
                .any(|hint| e.lower_name.contains(hint))
    })?;
    loose_image(&list_dir(&subdir.path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{create_dir_all, write};
    use tempfile::TempDir;

    fn fixture(build: impl FnOnce(&Path)) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        build(dir.path());
        dir
    }

    /// `dir` itself, canonicalized, as the sole configured root - matches
    /// what `TagMetadata::new` does for real roots, so a test's directory
    /// boundary behaves exactly like production's.
    fn roots_for(dir: &TempDir) -> Vec<PathBuf> {
        vec![std::fs::canonicalize(dir.path()).unwrap()]
    }

    #[test]
    fn finds_a_named_cover_file_beside_the_track() {
        let dir = fixture(|root| {
            write(root.join("01 - Track.mp3"), b"").unwrap();
            write(root.join("cover.jpg"), b"").unwrap();
        });
        let found = find_cover_file(&dir.path().join("01 - Track.mp3"), &roots_for(&dir)).unwrap();
        assert_eq!(found, dir.path().join("cover.jpg"));
    }

    #[test]
    fn matches_named_files_case_insensitively() {
        let dir = fixture(|root| {
            write(root.join("track.mp3"), b"").unwrap();
            write(root.join("Folder.JPG"), b"").unwrap();
        });
        let found = find_cover_file(&dir.path().join("track.mp3"), &roots_for(&dir)).unwrap();
        assert_eq!(found, dir.path().join("Folder.JPG"));
    }

    #[test]
    fn falls_back_to_an_arbitrarily_named_image() {
        let dir = fixture(|root| {
            write(root.join("track.mp3"), b"").unwrap();
            write(root.join("AlbumArt_Large.jpg"), b"").unwrap();
        });
        let found = find_cover_file(&dir.path().join("track.mp3"), &roots_for(&dir)).unwrap();
        assert_eq!(found, dir.path().join("AlbumArt_Large.jpg"));
    }

    #[test]
    fn finds_an_image_inside_an_art_hinted_subdirectory() {
        let dir = fixture(|root| {
            write(root.join("track.mp3"), b"").unwrap();
            create_dir_all(root.join("Scans covers")).unwrap();
            write(root.join("Scans covers/front.jpg"), b"").unwrap();
        });
        let found = find_cover_file(&dir.path().join("track.mp3"), &roots_for(&dir)).unwrap();
        assert_eq!(found, dir.path().join("Scans covers/front.jpg"));
    }

    #[test]
    fn checks_one_directory_up_for_a_multi_disc_layout() {
        let dir = fixture(|root| {
            create_dir_all(root.join("CD1")).unwrap();
            write(root.join("CD1/01 - Track.flac"), b"").unwrap();
            write(root.join("cover.jpg"), b"").unwrap();
        });
        let found =
            find_cover_file(&dir.path().join("CD1/01 - Track.flac"), &roots_for(&dir)).unwrap();
        assert_eq!(found, dir.path().join("cover.jpg"));
    }

    #[test]
    fn checks_an_art_subdirectory_one_directory_up() {
        let dir = fixture(|root| {
            create_dir_all(root.join("CD1")).unwrap();
            write(root.join("CD1/01 - Track.flac"), b"").unwrap();
            create_dir_all(root.join("Artwork")).unwrap();
            write(root.join("Artwork/front.png"), b"").unwrap();
        });
        let found =
            find_cover_file(&dir.path().join("CD1/01 - Track.flac"), &roots_for(&dir)).unwrap();
        assert_eq!(found, dir.path().join("Artwork/front.png"));
    }

    #[test]
    fn prefers_the_track_s_own_directory_over_the_one_above_it() {
        let dir = fixture(|root| {
            create_dir_all(root.join("CD1")).unwrap();
            write(root.join("CD1/01 - Track.flac"), b"").unwrap();
            write(root.join("CD1/cover.jpg"), b"").unwrap();
            write(root.join("folder.jpg"), b"").unwrap();
        });
        let found =
            find_cover_file(&dir.path().join("CD1/01 - Track.flac"), &roots_for(&dir)).unwrap();
        assert_eq!(found, dir.path().join("CD1/cover.jpg"));
    }

    #[test]
    fn a_folder_whose_own_name_contains_an_art_hint_does_not_match_itself() {
        // The album folder is named "Bartok" (contains "art"). Nothing
        // should treat the album folder as if it were an art subfolder
        // of itself.
        let dir = fixture(|root| {
            create_dir_all(root.join("Bartok")).unwrap();
            write(root.join("Bartok/track.mp3"), b"").unwrap();
        });
        assert!(find_cover_file(&dir.path().join("Bartok/track.mp3"), &roots_for(&dir)).is_none());
    }

    #[test]
    fn no_image_anywhere_yields_none() {
        let dir = fixture(|root| {
            write(root.join("track.mp3"), b"").unwrap();
        });
        assert!(find_cover_file(&dir.path().join("track.mp3"), &roots_for(&dir)).is_none());
    }

    #[test]
    fn a_track_with_no_parent_directory_does_not_panic() {
        assert!(find_cover_file(Path::new("track.mp3"), &[]).is_none());
    }

    #[test]
    fn does_not_climb_above_the_configured_root() {
        // A track sitting directly in the configured root, with no album
        // folder wrapping it - the one layout where "check the directory
        // above" would otherwise step outside the root entirely. A cover
        // file placed just outside the root must never be picked up, even
        // though a plain "check the parent directory" search would find
        // it - the same containment property core::http's
        // resolve_within_roots enforces for every file this server serves.
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("Music");
        create_dir_all(&root).unwrap();
        write(root.join("track.mp3"), b"").unwrap();
        write(outer.path().join("cover.jpg"), b"").unwrap();

        let roots = vec![std::fs::canonicalize(&root).unwrap()];
        assert!(find_cover_file(&root.join("track.mp3"), &roots).is_none());
    }

    #[test]
    fn mime_types_for_recognized_extensions() {
        assert_eq!(mime_for_cover_file(Path::new("cover.jpg")), "image/jpeg");
        assert_eq!(mime_for_cover_file(Path::new("cover.JPEG")), "image/jpeg");
        assert_eq!(mime_for_cover_file(Path::new("cover.png")), "image/png");
        assert_eq!(
            mime_for_cover_file(Path::new("cover.bmp")),
            "application/octet-stream"
        );
    }
}
