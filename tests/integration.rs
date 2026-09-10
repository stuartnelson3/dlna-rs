//! Spins up the real HTTP server on an ephemeral loopback port and drives
//! it with a plain HTTP client, per the testing strategy in docs/PLAN.md:
//! this is the automated half of Phases 3 and 5's exit criteria (the
//! manual half is `curl`/a real client against a running instance on the
//! LAN).

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::time::SystemTime;

use dlna_rs::content::folder::FolderMirror;
use dlna_rs::core::http::HttpServer;
use dlna_rs::index::{IndexBuilder, ObjectId};
use quick_xml::Reader;
use quick_xml::events::Event;
use uuid::Uuid;

fn fixture_content_source() -> FolderMirror {
    let mut builder = IndexBuilder::new();
    let album = builder.add_container(&ObjectId::root(), "Album".to_string());
    builder.add_item(
        &album,
        "01 - Track.mp3".to_string(),
        PathBuf::from("/music/Album/01 - Track.mp3"),
        12345,
        SystemTime::UNIX_EPOCH,
    );
    FolderMirror::new(builder.build())
}

async fn start_server() -> (String, tokio::task::JoinHandle<()>) {
    let server = HttpServer::bind(
        Ipv4Addr::LOCALHOST,
        0,
        "integration-test".to_string(),
        Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap(),
        fixture_content_source(),
    )
    .await
    .expect("failed to bind test HTTP server");
    let addr = server.local_addr().expect("bound server has a local addr");
    let handle = tokio::spawn(server.serve());
    (format!("http://{addr}"), handle)
}

fn assert_well_formed_xml(body: &str) {
    let mut reader = Reader::from_str(body);
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

fn browse_request_body(object_id: &str, browse_flag: &str) -> String {
    format!(
        r#"<?xml version="1.0"?>
        <s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
          <s:Body>
            <u:Browse xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1">
              <ObjectID>{object_id}</ObjectID>
              <BrowseFlag>{browse_flag}</BrowseFlag>
              <Filter>*</Filter>
              <StartingIndex>0</StartingIndex>
              <RequestedCount>0</RequestedCount>
              <SortCriteria></SortCriteria>
            </u:Browse>
          </s:Body>
        </s:Envelope>"#
    )
}

#[tokio::test]
async fn serves_well_formed_description_xml() {
    let (base, _handle) = start_server().await;
    let response = reqwest::get(format!("{base}/description.xml"))
        .await
        .expect("request failed");

    assert_eq!(response.status(), 200);
    let content_type = response
        .headers()
        .get("content-type")
        .expect("missing content-type")
        .to_str()
        .unwrap()
        .to_string();
    assert!(content_type.contains("text/xml"));

    let body = response.text().await.expect("failed to read body");
    assert_well_formed_xml(&body);
    assert!(body.contains("<friendlyName>integration-test</friendlyName>"));
}

#[tokio::test]
async fn serves_well_formed_scpd_for_both_services() {
    let (base, _handle) = start_server().await;

    for path in ["/ContentDirectory/scpd.xml", "/ConnectionManager/scpd.xml"] {
        let response = reqwest::get(format!("{base}{path}"))
            .await
            .unwrap_or_else(|err| panic!("request to {path} failed: {err}"));
        assert_eq!(response.status(), 200, "unexpected status for {path}");
        let body = response.text().await.expect("failed to read body");
        assert_well_formed_xml(&body);
    }
}

#[tokio::test]
async fn unknown_path_is_404() {
    let (base, _handle) = start_server().await;
    let response = reqwest::get(format!("{base}/nope")).await.unwrap();
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn browse_direct_children_of_root_over_real_http() {
    let (base, _handle) = start_server().await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{base}/ContentDirectory/control"))
        .body(browse_request_body("0", "BrowseDirectChildren"))
        .send()
        .await
        .expect("request failed");

    assert_eq!(response.status(), 200);
    let body = response.text().await.unwrap();
    assert_well_formed_xml(&body);
    assert!(body.contains("<NumberReturned>1</NumberReturned>"));
    assert!(body.contains("Album"));
}

#[tokio::test]
async fn browse_metadata_of_an_item_over_real_http() {
    let (base, _handle) = start_server().await;
    let client = reqwest::Client::new();

    // Find the album's id first via BrowseDirectChildren on root, then the
    // item's id via BrowseDirectChildren on the album - exercising the same
    // two-hop navigation a real client does.
    let album_response = client
        .post(format!("{base}/ContentDirectory/control"))
        .body(browse_request_body("0", "BrowseDirectChildren"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let album_id = extract_attr(&album_response, "container", "id");

    let items_response = client
        .post(format!("{base}/ContentDirectory/control"))
        .body(browse_request_body(&album_id, "BrowseDirectChildren"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let item_id = extract_attr(&items_response, "item", "id");

    let response = client
        .post(format!("{base}/ContentDirectory/control"))
        .body(browse_request_body(&item_id, "BrowseMetadata"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = response.text().await.unwrap();
    assert_well_formed_xml(&body);
    assert!(body.contains("01 - Track.mp3"));
    assert!(body.contains("DLNA.ORG_PN=MP3"));
}

#[tokio::test]
async fn unimplemented_action_returns_a_soap_fault_with_500() {
    let (base, _handle) = start_server().await;
    let client = reqwest::Client::new();
    let body = r#"<?xml version="1.0"?>
        <s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
          <s:Body><u:Search xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1"/></s:Body>
        </s:Envelope>"#;
    let response = client
        .post(format!("{base}/ContentDirectory/control"))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 500);
    let text = response.text().await.unwrap();
    assert!(text.contains("<errorCode>401</errorCode>"));
}

/// Pulls `attribute="value"` out of the first `<tag ...>` in some
/// DIDL-Lite-flavored XML embedded (double-escaped) inside a SOAP
/// response. Good enough for a test helper driving a two-hop Browse;
/// not a real XML parser.
fn extract_attr(soap_body: &str, tag: &str, attribute: &str) -> String {
    let unescaped = soap_body
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&");
    let needle = format!("<{tag} {attribute}=\"");
    let start = unescaped
        .find(&needle)
        .unwrap_or_else(|| panic!("no <{tag}> with {attribute} found"))
        + needle.len();
    let end = unescaped[start..].find('"').unwrap();
    unescaped[start..start + end].to_string()
}
