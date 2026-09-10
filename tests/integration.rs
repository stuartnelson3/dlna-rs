//! Spins up the real HTTP server on an ephemeral loopback port and drives
//! it with a plain HTTP client, per the testing strategy in docs/PLAN.md:
//! this is the automated half of Phase 3's exit criterion (the manual half
//! is `curl` against a real running instance on the LAN).

use std::net::Ipv4Addr;

use dlna_rs::core::http::HttpServer;
use quick_xml::Reader;
use quick_xml::events::Event;
use uuid::Uuid;

async fn start_server() -> (String, tokio::task::JoinHandle<()>) {
    let server = HttpServer::bind(
        Ipv4Addr::LOCALHOST,
        0,
        "integration-test".to_string(),
        Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap(),
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
