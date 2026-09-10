//! SSDP datagram parsing and message building — pure functions, no I/O.
//! Parsing in particular is attacker-reachable from any device on the LAN
//! (see docs/THREAT_MODEL.md), so it's kept as a self-contained function
//! that never panics and is cheap to fuzz in isolation
//! (`fuzz/fuzz_targets/ssdp_parse.rs`).

use uuid::Uuid;

use super::targets::Target;

/// Real M-SEARCH requests are a handful of short header lines. Bound the
/// input up front so a malicious or broken sender can't make us scan an
/// arbitrarily large buffer.
const MAX_DATAGRAM_LEN: usize = 2048;

#[derive(Debug, PartialEq, Eq)]
pub struct SearchRequest {
    pub search_target: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    TooLarge,
    NotUtf8,
    NotMSearch,
    MissingSearchTarget,
}

/// Parses an incoming datagram as an SSDP M-SEARCH request. Anything that
/// isn't a well-formed M-SEARCH with an `ST` header is rejected, not
/// guessed at — a malformed or irrelevant datagram (NOTIFY from some other
/// device, garbage, a port scanner) should fail closed.
pub fn parse_search_request(datagram: &[u8]) -> Result<SearchRequest, ParseError> {
    if datagram.len() > MAX_DATAGRAM_LEN {
        return Err(ParseError::TooLarge);
    }
    let text = std::str::from_utf8(datagram).map_err(|_| ParseError::NotUtf8)?;
    let mut lines = text.split('\n').map(|line| line.trim_end_matches('\r'));

    let request_line = lines.next().unwrap_or_default();
    if !request_line.eq_ignore_ascii_case("M-SEARCH * HTTP/1.1") {
        return Err(ParseError::NotMSearch);
    }

    let search_target = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("st"))
        .map(|(_, value)| value.trim().to_string())
        .ok_or(ParseError::MissingSearchTarget)?;

    Ok(SearchRequest { search_target })
}

/// The data needed to advertise one target, whether in a search response
/// or a NOTIFY.
pub struct Advertisement {
    pub target: Target,
    pub location: String,
    pub max_age: u32,
}

pub fn build_search_response(ad: &Advertisement, uuid: &Uuid) -> String {
    format!(
        "HTTP/1.1 200 OK\r\n\
         CACHE-CONTROL: max-age={max_age}\r\n\
         EXT:\r\n\
         LOCATION: {location}\r\n\
         SERVER: UPnP/1.0 dlna-rs/{version}\r\n\
         ST: {st}\r\n\
         USN: {usn}\r\n\
         \r\n",
        max_age = ad.max_age,
        location = ad.location,
        version = env!("CARGO_PKG_VERSION"),
        st = ad.target.type_string(uuid),
        usn = ad.target.usn(uuid),
    )
}

pub fn build_notify_alive(ad: &Advertisement, uuid: &Uuid) -> String {
    format!(
        "NOTIFY * HTTP/1.1\r\n\
         HOST: 239.255.255.250:1900\r\n\
         CACHE-CONTROL: max-age={max_age}\r\n\
         LOCATION: {location}\r\n\
         SERVER: UPnP/1.0 dlna-rs/{version}\r\n\
         NT: {nt}\r\n\
         NTS: ssdp:alive\r\n\
         USN: {usn}\r\n\
         \r\n",
        max_age = ad.max_age,
        location = ad.location,
        version = env!("CARGO_PKG_VERSION"),
        nt = ad.target.type_string(uuid),
        usn = ad.target.usn(uuid),
    )
}

pub fn build_notify_byebye(target: Target, uuid: &Uuid) -> String {
    format!(
        "NOTIFY * HTTP/1.1\r\n\
         HOST: 239.255.255.250:1900\r\n\
         NT: {nt}\r\n\
         NTS: ssdp:byebye\r\n\
         USN: {usn}\r\n\
         \r\n",
        nt = target.type_string(uuid),
        usn = target.usn(uuid),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::device::ServiceType;

    #[test]
    fn parses_a_real_msearch_request() {
        let datagram = b"M-SEARCH * HTTP/1.1\r\n\
                          HOST: 239.255.255.250:1900\r\n\
                          MAN: \"ssdp:discover\"\r\n\
                          MX: 2\r\n\
                          ST: ssdp:all\r\n\
                          \r\n";
        let request = parse_search_request(datagram).unwrap();
        assert_eq!(request.search_target, "ssdp:all");
    }

    #[test]
    fn header_names_are_case_insensitive() {
        let datagram = b"M-SEARCH * HTTP/1.1\r\nst: upnp:rootdevice\r\n\r\n";
        assert_eq!(
            parse_search_request(datagram).unwrap().search_target,
            "upnp:rootdevice"
        );
    }

    #[test]
    fn tolerates_bare_lf_line_endings() {
        let datagram = b"M-SEARCH * HTTP/1.1\nST: ssdp:all\n\n";
        assert_eq!(
            parse_search_request(datagram).unwrap().search_target,
            "ssdp:all"
        );
    }

    #[test]
    fn rejects_non_msearch_requests() {
        let datagram = b"NOTIFY * HTTP/1.1\r\nNTS: ssdp:alive\r\n\r\n";
        assert_eq!(parse_search_request(datagram), Err(ParseError::NotMSearch));
    }

    #[test]
    fn rejects_missing_search_target() {
        let datagram = b"M-SEARCH * HTTP/1.1\r\nMX: 2\r\n\r\n";
        assert_eq!(
            parse_search_request(datagram),
            Err(ParseError::MissingSearchTarget)
        );
    }

    #[test]
    fn rejects_oversized_datagrams() {
        let datagram = vec![b'a'; MAX_DATAGRAM_LEN + 1];
        assert_eq!(parse_search_request(&datagram), Err(ParseError::TooLarge));
    }

    #[test]
    fn rejects_invalid_utf8_without_panicking() {
        let datagram = [0xff, 0xfe, 0xfd];
        assert_eq!(parse_search_request(&datagram), Err(ParseError::NotUtf8));
    }

    #[test]
    fn never_panics_on_arbitrary_short_inputs() {
        // Not a fuzz run, just a cheap sanity sweep over structurally
        // varied garbage before this gets a real fuzz target.
        let candidates: &[&[u8]] = &[
            b"",
            b"\r\n",
            b"M-SEARCH",
            b"M-SEARCH * HTTP/1.1",
            b"M-SEARCH * HTTP/1.1\r\n:\r\n\r\n",
            b"M-SEARCH * HTTP/1.1\r\nST\r\n\r\n",
            &[0u8; 1],
        ];
        for candidate in candidates {
            let _ = parse_search_request(candidate);
        }
    }

    #[test]
    fn builds_a_search_response() {
        let uuid = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let ad = Advertisement {
            target: Target::Service(ServiceType::ContentDirectory),
            location: "http://192.168.1.5:8200/description.xml".to_string(),
            max_age: 1800,
        };
        let response = build_search_response(&ad, &uuid);
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("ST: urn:schemas-upnp-org:service:ContentDirectory:1\r\n"));
        assert!(response.contains(&format!(
            "USN: uuid:{uuid}::urn:schemas-upnp-org:service:ContentDirectory:1\r\n"
        )));
        assert!(response.contains("LOCATION: http://192.168.1.5:8200/description.xml\r\n"));
        assert!(response.ends_with("\r\n\r\n"));
    }

    #[test]
    fn builds_a_byebye_with_no_location_or_cache_control() {
        let uuid = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let message = build_notify_byebye(Target::RootDevice, &uuid);
        assert!(message.starts_with("NOTIFY * HTTP/1.1\r\n"));
        assert!(message.contains("NTS: ssdp:byebye\r\n"));
        assert!(!message.contains("LOCATION"));
        assert!(!message.contains("CACHE-CONTROL"));
    }
}
