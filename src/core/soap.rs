//! SOAP envelope parsing and building. Parsing is attacker-reachable from
//! any device on the LAN (see docs/THREAT_MODEL.md), so it's bounded and
//! fail-closed like the SSDP parser: a `Result`-returning pure function,
//! no I/O, cheap to fuzz in isolation.
//!
//! Deliberately syntactic rather than fully namespace-aware: `local_name`
//! strips whatever prefix a client's SOAP toolkit chose (`s:`,
//! `SOAP-ENV:`, `soap:` all resolve to the same local name), which is all
//! real UPnP control points need. *Which* service is being invoked comes
//! from the HTTP route (`/{service}/control`, `core::device`), not from
//! re-deriving it out of the body's declared namespace.

use std::collections::HashMap;

use quick_xml::escape::escape;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

/// Real ContentDirectory/ConnectionManager requests are a handful of short
/// elements. Bound the input up front, same reasoning as SSDP's 2KB cap
/// (`core::ssdp::message`) - checked against `Content-Length` before the
/// body is even read (`core::http`), and enforced again here regardless.
pub const MAX_BODY_LEN: usize = 8192;

#[derive(Debug, PartialEq)]
pub struct SoapAction {
    pub name: String,
    pub arguments: HashMap<String, String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    TooLarge,
    NotUtf8,
    Malformed,
    NoAction,
}

/// Parses a SOAP request body down to the action name and its arguments.
/// Never panics on malformed input - every XML operation here is
/// `Result`/`Option`-based, so the worst a hostile body can do is produce
/// `Err` or (for weirdly-nested-but-well-formed XML) a `SoapAction` with
/// unexpected contents, never a crash.
pub fn parse_action(body: &[u8]) -> Result<SoapAction, ParseError> {
    if body.len() > MAX_BODY_LEN {
        return Err(ParseError::TooLarge);
    }
    let text = std::str::from_utf8(body).map_err(|_| ParseError::NotUtf8)?;

    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);

    find_body(&mut reader)?;
    let (name, has_content) = read_action_open(&mut reader)?;
    let arguments = if has_content {
        read_arguments(&mut reader)?
    } else {
        HashMap::new()
    };

    Ok(SoapAction { name, arguments })
}

fn find_body(reader: &mut Reader<&[u8]>) -> Result<(), ParseError> {
    loop {
        match reader.read_event().map_err(|_| ParseError::Malformed)? {
            Event::Eof => return Err(ParseError::NoAction),
            Event::Start(e) if e.local_name().as_ref() == b"Body" => return Ok(()),
            _ => {}
        }
    }
}

/// Reads the single element inside `<Body>` — the action call itself.
/// Returns its local name and whether it has any content worth reading
/// (`false` for a self-closing `<Action/>` with zero arguments).
fn read_action_open(reader: &mut Reader<&[u8]>) -> Result<(String, bool), ParseError> {
    loop {
        match reader.read_event().map_err(|_| ParseError::Malformed)? {
            Event::Eof => return Err(ParseError::NoAction),
            Event::Start(e) => return Ok((local_name_string(&e), true)),
            Event::Empty(e) => return Ok((local_name_string(&e), false)),
            _ => {}
        }
    }
}

fn read_arguments(reader: &mut Reader<&[u8]>) -> Result<HashMap<String, String>, ParseError> {
    let mut arguments = HashMap::new();
    loop {
        match reader.read_event().map_err(|_| ParseError::Malformed)? {
            Event::Eof => return Err(ParseError::Malformed),
            Event::End(_) => return Ok(arguments), // the action element's own close
            Event::Empty(e) => {
                arguments.insert(local_name_string(&e), String::new());
            }
            Event::Start(e) => {
                let name = local_name_string(&e);
                let value = read_text_until_end(reader)?;
                arguments.insert(name, value);
            }
            _ => {}
        }
    }
}

fn read_text_until_end(reader: &mut Reader<&[u8]>) -> Result<String, ParseError> {
    let mut value = String::new();
    loop {
        match reader.read_event().map_err(|_| ParseError::Malformed)? {
            Event::Eof => return Err(ParseError::Malformed),
            Event::End(_) => return Ok(value),
            Event::Text(t) => {
                let decoded = t.decode().map_err(|_| ParseError::Malformed)?;
                value.push_str(&decoded);
            }
            _ => {}
        }
    }
}

fn local_name_string(e: &BytesStart<'_>) -> String {
    String::from_utf8_lossy(e.local_name().as_ref()).into_owned()
}

/// Builds a successful action response envelope.
pub fn build_response(
    service_urn: &str,
    action_name: &str,
    arguments: &[(&str, String)],
) -> String {
    let args_xml: String = arguments
        .iter()
        .map(|(name, value)| format!("<{name}>{}</{name}>", escape(value.as_str())))
        .collect();
    format!(
        r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:{action_name}Response xmlns:u="{service_urn}">{args_xml}</u:{action_name}Response></s:Body></s:Envelope>"#
    )
}

/// Builds a UPnP SOAP fault envelope. `code`/`description` are a standard
/// UPnP error code (401 Invalid Action, 402 Invalid Args, 701 No Such
/// Object, ...) and its description, per `core::dispatch::DispatchError`.
pub fn build_fault(code: u32, description: &str) -> String {
    format!(
        r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail><UPnPError xmlns="urn:schemas-upnp-org:control-1-0"><errorCode>{code}</errorCode><errorDescription>{}</errorDescription></UPnPError></detail></s:Fault></s:Body></s:Envelope>"#,
        escape(description)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_browse_request() {
        let body = br#"<?xml version="1.0"?>
            <s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
              <s:Body>
                <u:Browse xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1">
                  <ObjectID>0</ObjectID>
                  <BrowseFlag>BrowseDirectChildren</BrowseFlag>
                  <Filter>*</Filter>
                  <StartingIndex>0</StartingIndex>
                  <RequestedCount>0</RequestedCount>
                  <SortCriteria></SortCriteria>
                </u:Browse>
              </s:Body>
            </s:Envelope>"#;
        let action = parse_action(body).unwrap();
        assert_eq!(action.name, "Browse");
        assert_eq!(action.arguments.get("ObjectID").unwrap(), "0");
        assert_eq!(
            action.arguments.get("BrowseFlag").unwrap(),
            "BrowseDirectChildren"
        );
        assert_eq!(action.arguments.get("SortCriteria").unwrap(), "");
    }

    #[test]
    fn tolerates_different_envelope_prefixes() {
        let body =
            br#"<SOAP-ENV:Envelope xmlns:SOAP-ENV="http://schemas.xmlsoap.org/soap/envelope/">
              <SOAP-ENV:Body>
                <m:GetSystemUpdateID xmlns:m="urn:schemas-upnp-org:service:ContentDirectory:1"/>
              </SOAP-ENV:Body>
            </SOAP-ENV:Envelope>"#;
        let action = parse_action(body).unwrap();
        assert_eq!(action.name, "GetSystemUpdateID");
        assert!(action.arguments.is_empty());
    }

    #[test]
    fn no_prefix_at_all_still_parses() {
        let body = br#"<Envelope xmlns="http://schemas.xmlsoap.org/soap/envelope/">
              <Body>
                <GetSearchCapabilities xmlns="urn:schemas-upnp-org:service:ContentDirectory:1"/>
              </Body>
            </Envelope>"#;
        let action = parse_action(body).unwrap();
        assert_eq!(action.name, "GetSearchCapabilities");
    }

    #[test]
    fn rejects_oversized_bodies() {
        let body = vec![b'a'; MAX_BODY_LEN + 1];
        assert_eq!(parse_action(&body), Err(ParseError::TooLarge));
    }

    #[test]
    fn rejects_invalid_utf8() {
        assert_eq!(parse_action(&[0xff, 0xfe]), Err(ParseError::NotUtf8));
    }

    #[test]
    fn rejects_a_body_with_no_body_element() {
        assert_eq!(
            parse_action(b"<Envelope></Envelope>"),
            Err(ParseError::NoAction)
        );
    }

    #[test]
    fn rejects_an_empty_body_element() {
        assert_eq!(
            parse_action(b"<Envelope><Body></Body></Envelope>"),
            Err(ParseError::NoAction)
        );
    }

    #[test]
    fn never_panics_on_arbitrary_short_inputs() {
        let candidates: &[&[u8]] = &[
            b"",
            b"<",
            b"<Body",
            b"<Body></Body>",
            b"<Body><Action></Wrong></Body>",
            b"<Body><Action><Arg></Action></Body>",
            &[0u8; 1],
            b"<<<<<<<<<<<<<<<<<<",
        ];
        for candidate in candidates {
            let _ = parse_action(candidate);
        }
    }

    #[test]
    fn build_response_escapes_argument_values() {
        let xml = build_response(
            "urn:schemas-upnp-org:service:ContentDirectory:1",
            "Browse",
            &[("Result", "<Rock & Roll>".to_string())],
        );
        assert!(xml.contains("&lt;Rock &amp; Roll&gt;"));
        assert!(!xml.contains("<Rock & Roll>"));
    }

    #[test]
    fn build_fault_has_the_expected_error_code() {
        let xml = build_fault(401, "Invalid Action");
        assert!(xml.contains("<errorCode>401</errorCode>"));
        assert!(xml.contains("<errorDescription>Invalid Action</errorDescription>"));
        assert!(xml.contains("<s:Fault>"));
    }
}
