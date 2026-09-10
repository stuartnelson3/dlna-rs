//! Manual acceptance check for Phase 6's exit criterion: GET with and
//! without `Range` streams a real item correctly, including a mid-file
//! seek. Hand-rolled HTTP/1.1 over `TcpStream` rather than `curl` or
//! `reqwest` — no dependency on the host having `curl` installed, and no
//! new crate dependency either (our own responses are always a plain
//! `Content-Length` body, never chunked, so a minimal client is enough).
//!
//! Fully self-contained: does its own Browse (root, then the first
//! container found, up to a few levels deep) to find a real item, then
//! exercises GET/HEAD/Range against it. Not part of the automated test
//! suite (`tests/integration.rs` already covers this against a synthetic
//! fixture) — this is for checking a real running instance, on the LAN
//! or on localhost, the same way `examples/ssdp_discover.rs` checks SSDP.
//!
//! Usage: `cargo run --example verify_item_playback -- [host] [port]`
//! (defaults: 127.0.0.1 8200) while `dlna-rs` is running with at least
//! one real audio file somewhere under its configured media directories.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

struct Response {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn request(
    addr: &str,
    method: &str,
    path: &str,
    extra_headers: &[(&str, &str)],
    body: Option<&str>,
) -> Response {
    let mut stream =
        TcpStream::connect(addr).unwrap_or_else(|err| panic!("couldn't connect to {addr}: {err}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let host = addr.split(':').next().unwrap_or(addr);
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    for (name, value) in extra_headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(body) = body {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    if let Some(body) = body {
        request.push_str(body);
    }
    stream
        .write_all(request.as_bytes())
        .expect("failed to send request");

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .expect("failed to read response");
    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> Response {
    let header_end = find(raw, b"\r\n\r\n").expect("response had no header/body separator");
    let header_text = std::str::from_utf8(&raw[..header_end]).expect("headers weren't valid UTF-8");
    let mut lines = header_text.split("\r\n");

    let status_line = lines.next().unwrap_or_default();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    let body = raw[header_end + 4..].to_vec();
    Response {
        status,
        headers,
        body,
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn browse_body(object_id: &str, browse_flag: &str) -> String {
    format!(
        r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:Browse xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1"><ObjectID>{object_id}</ObjectID><BrowseFlag>{browse_flag}</BrowseFlag><Filter>*</Filter><StartingIndex>0</StartingIndex><RequestedCount>0</RequestedCount><SortCriteria></SortCriteria></u:Browse></s:Body></s:Envelope>"#
    )
}

fn browse(addr: &str, object_id: &str) -> String {
    let response = request(
        addr,
        "POST",
        "/ContentDirectory/control",
        &[("Content-Type", "text/xml")],
        Some(&browse_body(object_id, "BrowseDirectChildren")),
    );
    assert_eq!(response.status, 200, "Browse({object_id}) failed");
    String::from_utf8_lossy(&response.body)
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

/// Finds the id of the first item reachable from root, descending into
/// containers up to `max_depth` levels.
fn find_first_item_id(addr: &str, max_depth: u32) -> String {
    let mut container_id = "0".to_string();
    for _ in 0..max_depth {
        let didl = browse(addr, &container_id);
        if let Some(id) = extract_attr(&didl, "item", "id") {
            return id;
        }
        match extract_attr(&didl, "container", "id") {
            Some(id) => container_id = id,
            None => break,
        }
    }
    panic!(
        "no item found within {max_depth} levels of root - point this at a server with at least one real audio file"
    );
}

fn extract_attr(xml: &str, tag: &str, attribute: &str) -> Option<String> {
    let needle = format!("<{tag} {attribute}=\"");
    let start = xml.find(&needle)? + needle.len();
    let end = xml[start..].find('"')?;
    Some(xml[start..start + end].to_string())
}

fn check(label: &str, condition: bool) -> bool {
    println!("[{}] {label}", if condition { "PASS" } else { "FAIL" });
    condition
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let host = args.get(1).map_or("127.0.0.1", String::as_str);
    let port = args.get(2).map_or("8200", String::as_str);
    let addr = format!("{host}:{port}");

    println!("Looking for a real item under {addr}'s root...");
    let item_id = find_first_item_id(&addr, 6);
    let item_path = format!("/item/{item_id}");
    println!("Using item {item_id} ({item_path})\n");

    let mut all_passed = true;

    // Full GET, no Range.
    let full = request(&addr, "GET", &item_path, &[], None);
    all_passed &= check("GET (no Range) returns 200", full.status == 200);
    let content_length: usize = full
        .headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    all_passed &= check(
        "Content-Length matches actual body length",
        content_length == full.body.len(),
    );
    all_passed &= check(
        "Accept-Ranges: bytes is present",
        full.headers.get("accept-ranges").map(String::as_str) == Some("bytes"),
    );
    all_passed &= check("body is non-empty", !full.body.is_empty());

    // HEAD: same headers, empty body.
    let head = request(&addr, "HEAD", &item_path, &[], None);
    all_passed &= check("HEAD returns 200", head.status == 200);
    all_passed &= check("HEAD body is empty", head.body.is_empty());
    all_passed &= check(
        "HEAD Content-Length matches GET's",
        head.headers.get("content-length") == full.headers.get("content-length"),
    );

    // Mid-file seek: bytes 10..=19 (10 bytes), if the file is even that
    // long - fall back to a smaller range for a tiny fixture file.
    let file_len = full.body.len() as u64;
    let (range_start, range_end) = if file_len > 20 {
        (10, 19)
    } else {
        (0, file_len.saturating_sub(1))
    };
    let range_header = format!("bytes={range_start}-{range_end}");
    let ranged = request(&addr, "GET", &item_path, &[("Range", &range_header)], None);
    all_passed &= check("Range GET returns 206", ranged.status == 206);
    let expected_range_len = (range_end - range_start + 1) as usize;
    all_passed &= check(
        "Range body length matches request",
        ranged.body.len() == expected_range_len,
    );
    let expected_content_range = format!("bytes {range_start}-{range_end}/{file_len}");
    all_passed &= check(
        "Content-Range header is correct",
        ranged.headers.get("content-range") == Some(&expected_content_range),
    );
    let expected_bytes = &full.body[range_start as usize..=range_end as usize];
    all_passed &= check(
        "Range body bytes match the same offset in the full body",
        ranged.body == expected_bytes,
    );

    // Out-of-bounds range -> 416 with Content-Range: bytes */{size}.
    let oob_header = format!("bytes={}-{}", file_len + 1000, file_len + 2000);
    let oob = request(&addr, "GET", &item_path, &[("Range", &oob_header)], None);
    all_passed &= check("out-of-bounds Range returns 416", oob.status == 416);
    all_passed &= check(
        "416 response has Content-Range: bytes */{size}",
        oob.headers.get("content-range") == Some(&format!("bytes */{file_len}")),
    );

    // Multi-range -> 416, not mis-implemented.
    let multi = request(
        &addr,
        "GET",
        &item_path,
        &[("Range", "bytes=0-1,2-3")],
        None,
    );
    all_passed &= check("multi-range returns 416", multi.status == 416);

    println!();
    if all_passed {
        println!("All checks passed.");
    } else {
        println!("Some checks FAILED - see above.");
        std::process::exit(1);
    }
}
