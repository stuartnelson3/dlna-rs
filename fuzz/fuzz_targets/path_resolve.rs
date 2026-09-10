//! Fuzzes `/item/{id}` request-path parsing — the one place raw,
//! attacker-controlled request-path text gets turned into something used
//! for a lookup (see docs/THREAT_MODEL.md and
//! `core::http::router::parse_item_path`'s doc comment for why this is a
//! narrower scope than "path resolution" usually means: our opaque-
//! ObjectId URL scheme means the result is only ever an index lookup key,
//! never a filesystem path built from attacker text).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(path) = std::str::from_utf8(data) {
        let _ = dlna_rs::fuzz_support::parse_item_path(path);
    }
});
