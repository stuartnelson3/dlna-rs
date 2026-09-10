//! `TagMetadata`: reads real tags and embedded cover art from an audio
//! file, using `lofty`. Unlike `FilenameMetadata`, this does real file
//! I/O and binary parsing — expensive enough that it must run only once
//! per file, at scan time (see `core::metadata_provider`'s doc comment
//! on why). `lofty` is named only in this file: nothing outside it
//! names a `lofty` type, so a future swap to a different tag library
//! touches only this file — see docs/DESIGN.md's encapsulation rule.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use bytes::Bytes;
use lofty::file::TaggedFileExt;
use lofty::picture::{MimeType, PictureType};
use lofty::tag::{Accessor, Tag};

use crate::core::art_source::{Art, ArtSource};
use crate::core::metadata_provider::{Metadata, MetadataProvider};
use crate::metadata::filename::FilenameMetadata;

pub struct TagMetadata;

impl MetadataProvider for TagMetadata {
    fn metadata(&self, path: &Path) -> Metadata {
        // title/track_number come from FilenameMetadata, not the tag -
        // §5.1's folder-structure grouping is what actually uses these,
        // and that logic is already tested; this method only adds to
        // it, never replaces it.
        let filename = FilenameMetadata.metadata(path);
        let Some(tag) = read_tag(path) else {
            return Metadata {
                artist: None,
                album: None,
                genre: None,
                has_art: false,
                ..filename
            };
        };
        Metadata {
            artist: tag.artist().map(|s| s.into_owned()),
            album: tag.album().map(|s| s.into_owned()),
            genre: tag.genre().map(|s| s.into_owned()),
            has_art: !tag.pictures().is_empty(),
            ..filename
        }
    }
}

impl ArtSource for TagMetadata {
    fn art<'a>(&'a self, path: &'a Path) -> Pin<Box<dyn Future<Output = Option<Art>> + Send + 'a>> {
        Box::pin(async move {
            let tag = read_tag(path)?;
            let picture = tag
                .get_picture_type(PictureType::CoverFront)
                .or_else(|| tag.pictures().first())?
                .clone();
            let mime = mime_str(picture.mime_type());
            Some(Art {
                bytes: Bytes::from(picture.into_data()),
                mime,
            })
        })
    }
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

    #[test]
    fn reads_artist_album_genre_from_a_real_tag() {
        let (_dir, path) = fixture_with(|tag| {
            tag.set_artist("Test Artist".to_string());
            tag.set_album("Test Album".to_string());
            tag.set_genre("Test Genre".to_string());
        });

        let metadata = TagMetadata.metadata(&path);
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

        let metadata = TagMetadata.metadata(&path);
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

        let metadata = TagMetadata.metadata(&path);
        assert_eq!(metadata.artist, None);
        assert!(!metadata.has_art);

        assert!(tokio_test_block_on(TagMetadata.art(&path)).is_none());
    }

    #[test]
    fn reads_back_an_embedded_picture() {
        let (_dir, path) = fixture_with(|tag| {
            tag.push_picture(
                lofty::picture::Picture::unchecked(tiny_png())
                    .pic_type(PictureType::CoverFront)
                    .mime_type(MimeType::Png)
                    .build(),
            );
        });

        let metadata = TagMetadata.metadata(&path);
        assert!(metadata.has_art);

        let art = tokio_test_block_on(TagMetadata.art(&path)).unwrap();
        assert_eq!(art.mime, "image/png");
        assert_eq!(&art.bytes[..], &tiny_png()[..]);
    }

    #[test]
    fn a_tag_with_no_picture_has_no_art() {
        let (_dir, path) = fixture_with(|tag| {
            tag.set_artist("Artist Only".to_string());
        });

        assert!(tokio_test_block_on(TagMetadata.art(&path)).is_none());
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
