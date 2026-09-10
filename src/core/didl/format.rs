//! The one canonical "what audio format is this" table. `scanner` uses
//! [`is_audio_extension`] to decide what belongs in the index at all;
//! [`protocol_info`] uses the same table to describe it correctly in
//! DIDL-Lite. One table, not two independently-maintained lists that
//! could drift — an extension `scanner` recognizes as audio but this
//! module doesn't know about would silently fall back to a wrong,
//! generic protocolInfo.
//!
//! `DLNA.ORG_PN` (the DLNA media profile) is only ever claimed for MP3.
//! Profiles like AAC or LPCM specify constraints — bitrate ceilings,
//! sample rate/channel count — that can't be verified without parsing the
//! file's actual header, which is exactly the binary-content-parsing this
//! project's threat model deliberately avoids (see docs/THREAT_MODEL.md
//! and docs/PLAN.md Phase 4). Claiming a profile we haven't verified is
//! worse than claiming none: it's exactly the "plays on client A, not on
//! client B" bug class the spec calls out. MP3's profile has no such
//! constraints, so it's safe to claim from the extension alone — matching
//! real-world minimal server practice (MiniDLNA does the same).

use std::path::Path;

/// A conservative, near-universal baseline: DLNA v1.5, streaming transfer
/// mode, background transfer mode, connection stalling allowed. This
/// exact value is what MiniDLNA and most minimal DLNA servers send for
/// plain HTTP GET/Range-served content — reused verbatim rather than
/// invented, since getting this wrong is a well-documented source of
/// client-specific playback bugs.
const DLNA_FLAGS: &str = "01700000000000000000000000000000";

pub struct AudioFormat {
    pub mime: &'static str,
    pub dlna_profile: Option<&'static str>,
}

fn for_extension(ext: &str) -> Option<AudioFormat> {
    let (mime, dlna_profile) = match ext.to_ascii_lowercase().as_str() {
        "mp3" => ("audio/mpeg", Some("MP3")),
        "flac" => ("audio/flac", None),
        "m4a" | "mp4" => ("audio/mp4", None),
        "aac" => ("audio/aac", None),
        "ogg" | "oga" => ("audio/ogg", None),
        "opus" => ("audio/opus", None),
        "wav" => ("audio/wav", None),
        "wma" => ("audio/x-ms-wma", None),
        "ape" => ("audio/x-ape", None),
        "wv" => ("audio/x-wavpack", None),
        _ => return None,
    };
    Some(AudioFormat { mime, dlna_profile })
}

pub fn is_audio_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| for_extension(ext).is_some())
}

/// The MIME type for the `Content-Type` header (Phase 6) as well as the
/// `protocolInfo`/`contentFeatures.dlna.org` values below. Falls back to a
/// generic value for an unrecognized extension rather than panicking —
/// `scanner` should never hand this an extension outside the table in
/// practice (it filters on the same table via `is_audio_extension`), but
/// none of these functions assume that of their caller.
pub fn mime_for(path: &Path) -> &'static str {
    path.extension()
        .and_then(|ext| ext.to_str())
        .and_then(for_extension)
        .map_or("application/octet-stream", |f| f.mime)
}

/// The DLNA-specific portion of `protocolInfo` — also used verbatim as
/// the `contentFeatures.dlna.org` response header (Phase 6), which is
/// this same value without the `http-get:*:<mime>:` prefix `protocolInfo`
/// itself needs.
pub fn dlna_content_features(path: &Path) -> String {
    let profile = path
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(for_extension)
        .and_then(|f| f.dlna_profile);
    match profile {
        Some(pn) => format!("DLNA.ORG_PN={pn};DLNA.ORG_OP=01;DLNA.ORG_FLAGS={DLNA_FLAGS}"),
        None => format!("DLNA.ORG_OP=01;DLNA.ORG_FLAGS={DLNA_FLAGS}"),
    }
}

/// The `protocolInfo` value for a `<res>` element.
pub fn protocol_info(path: &Path) -> String {
    format!(
        "http-get:*:{}:{}",
        mime_for(path),
        dlna_content_features(path)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn protocol_info_for(name: &str) -> String {
        protocol_info(&PathBuf::from(name))
    }

    #[test]
    fn mp3_claims_the_mp3_dlna_profile() {
        assert_eq!(
            protocol_info_for("track.mp3"),
            "http-get:*:audio/mpeg:DLNA.ORG_PN=MP3;DLNA.ORG_OP=01;DLNA.ORG_FLAGS=01700000000000000000000000000000"
        );
    }

    #[test]
    fn flac_has_no_dlna_profile() {
        assert_eq!(
            protocol_info_for("track.flac"),
            "http-get:*:audio/flac:DLNA.ORG_OP=01;DLNA.ORG_FLAGS=01700000000000000000000000000000"
        );
    }

    #[test]
    fn m4a_has_no_dlna_profile() {
        assert_eq!(
            protocol_info_for("track.m4a"),
            "http-get:*:audio/mp4:DLNA.ORG_OP=01;DLNA.ORG_FLAGS=01700000000000000000000000000000"
        );
    }

    #[test]
    fn ogg_has_no_dlna_profile() {
        assert_eq!(
            protocol_info_for("track.ogg"),
            "http-get:*:audio/ogg:DLNA.ORG_OP=01;DLNA.ORG_FLAGS=01700000000000000000000000000000"
        );
    }

    #[test]
    fn wav_has_no_dlna_profile() {
        assert_eq!(
            protocol_info_for("track.wav"),
            "http-get:*:audio/wav:DLNA.ORG_OP=01;DLNA.ORG_FLAGS=01700000000000000000000000000000"
        );
    }

    #[test]
    fn extension_matching_is_case_insensitive() {
        assert_eq!(
            protocol_info_for("track.MP3"),
            protocol_info_for("track.mp3")
        );
    }

    #[test]
    fn unrecognized_extension_falls_back_without_panicking() {
        assert_eq!(
            protocol_info_for("track.xyz"),
            "http-get:*:application/octet-stream:DLNA.ORG_OP=01;DLNA.ORG_FLAGS=01700000000000000000000000000000"
        );
    }

    #[test]
    fn mime_for_matches_the_protocol_info_mime() {
        assert_eq!(mime_for(&PathBuf::from("track.mp3")), "audio/mpeg");
        assert_eq!(mime_for(&PathBuf::from("track.flac")), "audio/flac");
    }

    #[test]
    fn dlna_content_features_is_protocol_info_without_the_prefix() {
        let path = PathBuf::from("track.mp3");
        let expected_suffix = dlna_content_features(&path);
        assert!(protocol_info(&path).ends_with(&expected_suffix));
        assert_eq!(
            expected_suffix,
            "DLNA.ORG_PN=MP3;DLNA.ORG_OP=01;DLNA.ORG_FLAGS=01700000000000000000000000000000"
        );
    }

    #[test]
    fn recognizes_every_extension_scanner_relies_on() {
        for ext in [
            "mp3", "flac", "m4a", "mp4", "aac", "ogg", "oga", "opus", "wav", "wma", "ape", "wv",
        ] {
            assert!(
                is_audio_extension(&PathBuf::from(format!("x.{ext}"))),
                "{ext} should be recognized as audio"
            );
        }
    }
}
