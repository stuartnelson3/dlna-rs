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
        }
    }
}

/// Splits a leading track number off a filename stem: `"01 - Track"`
/// becomes `(Some(1), "Track")`. A run of 1 to 3 leading digits counts as
/// a track number; a longer run (a 4-digit year, say) does not, since a
/// real track number rarely runs past 999.
fn parse_track_number(stem: &str) -> (Option<u32>, String) {
    let digit_count = stem.chars().take_while(char::is_ascii_digit).count();
    if digit_count == 0 || digit_count > 3 {
        return (None, stem.to_string());
    }

    let Ok(number) = stem[..digit_count].parse::<u32>() else {
        return (None, stem.to_string());
    };

    let title = stem[digit_count..]
        .trim_start_matches(['-', '.', '_', ' '])
        .trim();
    if title.is_empty() {
        return (None, stem.to_string());
    }

    (Some(number), title.to_string())
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
    fn never_panics_on_arbitrary_short_inputs() {
        for name in ["", ".", "..mp3", "   .mp3", "999999999999999999.mp3"] {
            let _ = metadata_for(name);
        }
    }
}
