//! Fuzzes SSDP M-SEARCH parsing — attacker-reachable from any device on
//! the LAN (see docs/THREAT_MODEL.md). The only property under test is
//! that it never panics; parsing is a pure function with no I/O, so this
//! is cheap to run exhaustively.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = dlna_rs::fuzz_support::parse_search_request(data);
});
