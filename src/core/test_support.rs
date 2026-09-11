//! Shared test-only helpers for `core`'s own XML-producing modules.
//! `#[cfg(test)]`-gated, so none of this ships in the real binary.

use quick_xml::Reader;
use quick_xml::events::Event;

/// Panics with the byte offset of the first parse error, if `xml` isn't
/// well-formed. Used by every test in `core` that renders XML
/// (`didl`, `http::description`, `http::scpd`) to check real
/// well-formedness, not just "the right substring is in there somewhere".
pub(crate) fn assert_well_formed(xml: &str) {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(err) => panic!("malformed XML at {}: {err}", reader.buffer_position()),
        }
        buf.clear();
    }
}
