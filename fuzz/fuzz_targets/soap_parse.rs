//! Fuzzes SOAP body parsing — attacker-reachable from any device on the
//! LAN via a POST to `/{service}/control` (see docs/THREAT_MODEL.md). As
//! with `ssdp_parse`, the only property under test is that it never
//! panics; parsing is a pure function with no I/O.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = dlna_rs::fuzz_support::parse_soap_action(data);
});
