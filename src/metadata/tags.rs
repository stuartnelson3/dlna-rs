//! `TagMetadata`: reads real tags and embedded cover art from an audio
//! file, using `lofty`. Unlike `FilenameMetadata`, this does real file
//! I/O and binary parsing — expensive enough that it must run only once
//! per file, at scan time (see `core::metadata_provider`'s doc comment
//! on why). `lofty` is named only in this file: nothing outside it
//! names a `lofty` type, so a future swap to a different tag library
//! touches only this file — see docs/DESIGN.md's encapsulation rule.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use bytes::Bytes;
use lofty::file::TaggedFileExt;
use lofty::picture::{MimeType, PictureType};
use lofty::tag::{Accessor, Tag};

use crate::core::art_source::{Art, ArtSource};
use crate::core::metadata_provider::{Metadata, MetadataProvider};
use crate::metadata::cover_files::{find_cover_file, mime_for_cover_file};
use crate::metadata::filename::FilenameMetadata;

pub struct TagMetadata {
    media_roots: Vec<PathBuf>,
}

impl TagMetadata {
    /// `media_roots` bounds the external-cover-file search: `find_cover_file`'s
    /// "check the directory above, too" step (for a multi-disc album whose
    /// art sits beside its disc subfolders) never returns a path outside
    /// these directories - the same containment property `core::http`'s
    /// `verify_within_roots` enforces for every file this server serves.
    /// A root that can't be canonicalized (already invalid, or gone) is
    /// dropped rather than failing construction - `HttpServer::bind`
    /// independently canonicalizes the same list and fails startup loudly
    /// if a configured directory is genuinely bad; this is only a
    /// best-effort boundary for the lookup above, not that check.
    pub fn new(media_roots: Vec<PathBuf>) -> Self {
        let media_roots = media_roots
            .into_iter()
            .filter_map(|root| std::fs::canonicalize(&root).ok())
            .collect();
        TagMetadata { media_roots }
    }
}

impl MetadataProvider for TagMetadata {
    fn metadata(&self, path: &Path) -> Metadata {
        // title/track_number come from FilenameMetadata, not the tag -
        // §5.1's folder-structure grouping is what actually uses these,
        // and that logic is already tested; this method only adds to
        // it, never replaces it.
        let filename = FilenameMetadata.metadata(path);
        let tag = read_tag(path);
        let has_embedded_art = tag.as_ref().is_some_and(|t| !t.pictures().is_empty());
        let has_art = has_embedded_art || find_cover_file(path, &self.media_roots).is_some();
        let Some(tag) = tag else {
            return Metadata {
                artist: None,
                album: None,
                genre: None,
                has_art,
                ..filename
            };
        };
        Metadata {
            artist: tag.artist().map(|s| s.into_owned()),
            album: tag.album().map(|s| s.into_owned()),
            genre: tag.genre().map(|s| s.into_owned()),
            has_art,
            ..filename
        }
    }
}

impl ArtSource for TagMetadata {
    fn art<'a>(&'a self, path: &'a Path) -> Pin<Box<dyn Future<Output = Option<Art>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(art) = embedded_picture(path) {
                return Some(art);
            }
            let cover_path = find_cover_file(path, &self.media_roots)?;
            let bytes = std::fs::read(&cover_path).ok()?;
            Some(Art {
                bytes: Bytes::from(bytes),
                mime: mime_for_cover_file(&cover_path),
            })
        })
    }
}

/// The embedded picture from `path`'s own tag, preferred over an
/// external cover file when both exist - an embedded picture was
/// deliberately attached to this exact track, so it wins.
fn embedded_picture(path: &Path) -> Option<Art> {
    let tag = read_tag(path)?;
    let picture = tag
        .get_picture_type(PictureType::CoverFront)
        .or_else(|| tag.pictures().first())?
        .clone();
    Some(Art {
        mime: mime_str(picture.mime_type()),
        bytes: Bytes::from(picture.into_data()),
    })
}

/// Reads `path`'s tag, or `None` if the file has no tag, isn't a format
/// `lofty` understands (it has no WMA support, for one — see
/// docs/PLAN.md), or is malformed. Never panics: every `lofty` call in
/// this chain returns a `Result`, and every error is logged and
/// swallowed here, not propagated — one bad file's tags must never
/// abort a whole scan.
fn read_tag(path: &Path) -> Option<Tag> {
    let tagged_file = match lofty::read_from_path(path) {
        Ok(file) => file,
        Err(err) => {
            log::warn!("couldn't read tags from {}: {err}", path.display());
            return None;
        }
    };
    tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag())
        .cloned()
}

/// `lofty`'s own `MimeType` never leaves this function — the same
/// fallback convention `core::didl::format::mime_for` already uses for
/// an unrecognized audio extension.
fn mime_str(mime: Option<&MimeType>) -> &'static str {
    match mime {
        Some(MimeType::Png) => "image/png",
        Some(MimeType::Jpeg) => "image/jpeg",
        Some(MimeType::Tiff) => "image/tiff",
        Some(MimeType::Bmp) => "image/bmp",
        Some(MimeType::Gif) => "image/gif",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lofty::file::AudioFile;

    /// Writes a minimal real MP3, applies `edit` to its tag, and hands
    /// back the path — every test gets a fresh, disposable file, and
    /// lofty's own write API is what stamps in known tag/picture
    /// values, not hand-built binary tags.
    fn fixture_with(edit: impl FnOnce(&mut Tag)) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("track.mp3");
        std::fs::write(&path, minimal_mp3()).unwrap();

        let mut tagged_file = lofty::read_from_path(&path).unwrap();
        if tagged_file.primary_tag().is_none() {
            tagged_file.insert_tag(Tag::new(tagged_file.primary_tag_type()));
        }
        let tag = tagged_file.primary_tag_mut().unwrap();
        edit(tag);
        tagged_file
            .save_to_path(&path, lofty::config::WriteOptions::default())
            .unwrap();

        (dir, path)
    }

    /// A minimal, structurally valid MPEG-1 Layer III frame: enough for
    /// `lofty` to recognize the file as MP3 and attach a tag to it. Real
    /// audio content doesn't matter here - only real audio *framing*
    /// does, which is what makes lofty accept the file at all.
    fn minimal_mp3() -> Vec<u8> {
        // 30 repeats of one silent 128kbps 44.1kHz stereo MPEG-1 Layer
        // III frame. A single frame isn't enough for lofty to confirm
        // it's really looking at an MP3 stream (verified directly: one
        // frame fails to parse, thirty parses cleanly) - it wants
        // several consistent, consecutive frame headers before it
        // trusts the file, not just one.
        let mut frame = vec![0xFFu8, 0xFB, 0x90, 0x00];
        frame.resize(417, 0);
        frame.repeat(30)
    }

    /// A `TagMetadata` whose one configured root is `dir` - every test
    /// below keeps its fixture file directly in `dir`, so this is enough
    /// for the external-cover-file lookup to behave exactly as it would
    /// against a real configured media directory.
    fn tags_for(dir: &tempfile::TempDir) -> TagMetadata {
        TagMetadata::new(vec![dir.path().to_path_buf()])
    }

    #[test]
    fn reads_artist_album_genre_from_a_real_tag() {
        let (dir, path) = fixture_with(|tag| {
            tag.set_artist("Test Artist".to_string());
            tag.set_album("Test Album".to_string());
            tag.set_genre("Test Genre".to_string());
        });

        let metadata = tags_for(&dir).metadata(&path);
        assert_eq!(metadata.artist.as_deref(), Some("Test Artist"));
        assert_eq!(metadata.album.as_deref(), Some("Test Album"));
        assert_eq!(metadata.genre.as_deref(), Some("Test Genre"));
        assert!(!metadata.has_art);
    }

    #[test]
    fn a_file_with_no_tag_yields_no_metadata_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("untagged.mp3");
        std::fs::write(&path, minimal_mp3()).unwrap();

        let metadata = tags_for(&dir).metadata(&path);
        assert_eq!(metadata.artist, None);
        assert_eq!(metadata.album, None);
        assert_eq!(metadata.genre, None);
        assert!(!metadata.has_art);
    }

    #[test]
    fn an_extension_lofty_does_not_support_yields_no_metadata_without_panicking() {
        // lofty has no WMA support, but core::didl::format still lists
        // .wma as a valid audio extension for byte-serving - a file
        // that reaches this provider with that extension must not
        // crash the scan.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("track.wma");
        std::fs::write(&path, b"not a real wma file").unwrap();

        let metadata = tags_for(&dir).metadata(&path);
        assert_eq!(metadata.artist, None);
        assert!(!metadata.has_art);

        assert!(tokio_test_block_on(tags_for(&dir).art(&path)).is_none());
    }

    #[test]
    fn reads_back_an_embedded_picture() {
        let (dir, path) = fixture_with(|tag| {
            tag.push_picture(
                lofty::picture::Picture::unchecked(tiny_png())
                    .pic_type(PictureType::CoverFront)
                    .mime_type(MimeType::Png)
                    .build(),
            );
        });

        let metadata = tags_for(&dir).metadata(&path);
        assert!(metadata.has_art);

        let art = tokio_test_block_on(tags_for(&dir).art(&path)).unwrap();
        assert_eq!(art.mime, "image/png");
        assert_eq!(&art.bytes[..], &tiny_png()[..]);
    }

    #[test]
    fn a_tag_with_no_picture_has_no_art() {
        let (dir, path) = fixture_with(|tag| {
            tag.set_artist("Artist Only".to_string());
        });

        assert!(tokio_test_block_on(tags_for(&dir).art(&path)).is_none());
    }

    #[test]
    fn falls_back_to_an_external_cover_file_when_the_tag_has_no_picture() {
        let (dir, path) = fixture_with(|tag| {
            tag.set_artist("Artist Only".to_string());
        });
        std::fs::write(dir.path().join("cover.jpg"), tiny_png()).unwrap();

        let metadata = tags_for(&dir).metadata(&path);
        assert!(metadata.has_art);

        let art = tokio_test_block_on(tags_for(&dir).art(&path)).unwrap();
        assert_eq!(art.mime, "image/jpeg");
        assert_eq!(&art.bytes[..], &tiny_png()[..]);
    }

    #[test]
    fn an_embedded_picture_wins_over_an_external_cover_file() {
        let (dir, path) = fixture_with(|tag| {
            tag.push_picture(
                lofty::picture::Picture::unchecked(tiny_png())
                    .pic_type(PictureType::CoverFront)
                    .mime_type(MimeType::Png)
                    .build(),
            );
        });
        std::fs::write(dir.path().join("cover.jpg"), b"not the real cover").unwrap();

        let art = tokio_test_block_on(tags_for(&dir).art(&path)).unwrap();
        assert_eq!(art.mime, "image/png");
        assert_eq!(&art.bytes[..], &tiny_png()[..]);
    }

    /// A 1x1 transparent PNG - the smallest real, valid PNG there is.
    fn tiny_png() -> Vec<u8> {
        vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ]
    }

    fn tokio_test_block_on<F: Future>(fut: F) -> F::Output {
        tokio::runtime::Runtime::new().unwrap().block_on(fut)
    }
}
