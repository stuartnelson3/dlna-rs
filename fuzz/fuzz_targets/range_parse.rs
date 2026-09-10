//! Fuzzes `Range` header parsing — attacker-reachable on every GET to
//! `/item/{id}` (see docs/THREAT_MODEL.md), and the exact bug class
//! behind a real MiniDLNA CVE (a chunked-length parsing overflow). Fuzzes
//! both dimensions: the header string itself, and the file size it's
//! parsed against (the first 8 bytes of input, so weird combinations like
//! a huge range against a zero-length file get exercised too).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }
    let file_size = u64::from_le_bytes(data[0..8].try_into().unwrap());
    if let Ok(header) = std::str::from_utf8(&data[8..]) {
        let _ = dlna_rs::fuzz_support::parse_range(header, file_size);
    }
});
