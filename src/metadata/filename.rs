//! `FilenameMetadata`: pulls a track number and a clean title out of a
//! filename, nothing more. Pure string parsing, no file content read —
//! matches the threat model's narrow, network-input-only parsing surface
//! (see docs/THREAT_MODEL.md).

use std::path::Path;

use crate::core::metadata_provider::{Metadata, MetadataProvider};

pub struct FilenameMetadata;

impl MetadataProvider for FilenameMetadata {
    fn metadata(&self, path: &Path) -> Metadata {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let (track_number, title) = parse_track_number(stem);
        Metadata {
            title,
            track_number,
            artist: None,
            album: None,
            genre: None,
            has_art: false,
        }
    }
}

/// Splits a leading track number off a filename stem: `"01 - Track"`
/// becomes `(Some(1), "Track")`. A run of 1 to 3 leading digits counts as
/// a track number; a longer run (a 4-digit year, say) does not, since a
/// real track number rarely runs past 999.
///
/// A disc-track convention (`"1-01 Track"`, disc 1 track 01) strips
/// more than one such prefix: whatever's left after stripping one is
/// tried again, and the innermost successful strip wins - so
/// "1-01 Track" yields track number 1 (the real per-disc track
/// number, not the disc number) and title "Track", while a single
/// prefix ("01 - Track") is unaffected, since there's no second prefix
/// left to find. Real regression: a real "1-01 Just Friends"-style
/// file was showing up with "01 Just Friends" left in the title,
/// confirmed against a real running instance.
fn parse_track_number(stem: &str) -> (Option<u32>, String) {
    let digit_count = stem.chars().take_while(char::is_ascii_digit).count();
    if digit_count == 0 || digit_count > 3 {
        return (None, stem.to_string());
    }

    let Ok(number) = stem[..digit_count].parse::<u32>() else {
        return (None, stem.to_string());
    };

    let rest = stem[digit_count..]
        .trim_start_matches(['-', '.', '_', ' '])
        .trim();
    if rest.is_empty() {
        return (None, stem.to_string());
    }

    match parse_track_number(rest) {
        (Some(inner_number), inner_title) => (Some(inner_number), inner_title),
        (None, _) => (Some(number), rest.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn metadata_for(name: &str) -> Metadata {
        FilenameMetadata.metadata(&PathBuf::from(name))
    }

    #[test]
    fn dash_separated_track_number() {
        let m = metadata_for("01 - Track.mp3");
        assert_eq!(m.track_number, Some(1));
        assert_eq!(m.title, "Track");
    }

    #[test]
    fn dot_separated_track_number() {
        let m = metadata_for("02. Another Song.flac");
        assert_eq!(m.track_number, Some(2));
        assert_eq!(m.title, "Another Song");
    }

    #[test]
    fn underscore_separated_track_number() {
        let m = metadata_for("3_Third.mp3");
        assert_eq!(m.track_number, Some(3));
        assert_eq!(m.title, "Third");
    }

    #[test]
    fn leading_zero_track_number() {
        let m = metadata_for("007 Bond Theme.mp3");
        assert_eq!(m.track_number, Some(7));
        assert_eq!(m.title, "Bond Theme");
    }

    #[test]
    fn no_track_number_keeps_the_title_as_is() {
        let m = metadata_for("Track With No Number.mp3");
        assert_eq!(m.track_number, None);
        assert_eq!(m.title, "Track With No Number");
    }

    #[test]
    fn a_four_digit_prefix_is_not_a_track_number() {
        let m = metadata_for("1999 New Year Mix.mp3");
        assert_eq!(
            m.track_number, None,
            "a year should not parse as a track number"
        );
        assert_eq!(m.title, "1999 New Year Mix");
    }

    #[test]
    fn digits_with_nothing_after_are_not_a_track_number() {
        let m = metadata_for("42.mp3");
        assert_eq!(m.track_number, None);
        assert_eq!(m.title, "42");
    }

    #[test]
    fn a_disc_track_prefix_uses_the_real_track_number_and_strips_both() {
        let m = metadata_for("1-01 Just Friends.mp3");
        assert_eq!(m.track_number, Some(1));
        assert_eq!(m.title, "Just Friends");
    }

    #[test]
    fn a_disc_track_prefix_with_a_double_digit_track_number() {
        let m = metadata_for("2-14 I'll Remember April.mp3");
        assert_eq!(m.track_number, Some(14));
        assert_eq!(m.title, "I'll Remember April");
    }

    #[test]
    fn a_leading_number_that_is_really_the_title_is_not_double_stripped() {
        // "2112" is the song/album title, not a second track number -
        // the four-digit guard on the recursive call must still apply.
        let m = metadata_for("1-2112.mp3");
        assert_eq!(m.track_number, Some(1));
        assert_eq!(m.title, "2112");
    }

    #[test]
    fn never_panics_on_arbitrary_short_inputs() {
        for name in ["", ".", "..mp3", "   .mp3", "999999999999999999.mp3"] {
            let _ = metadata_for(name);
        }
    }
}
