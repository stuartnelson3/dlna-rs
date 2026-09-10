//! Manual acceptance check for Phase 8's music library views. Hand-rolled
//! HTTP/1.1 over `TcpStream`, same as `examples/verify_item_playback.rs`
//! and for the same reason: no dependency on the host having `curl`
//! installed, no new crate dependency.
//!
//! Doesn't assume a specific `library.views` list - it walks whatever the
//! running server actually returns, so it works against any config. Two
//! checks it makes at every level of that walk are exactly the two real
//! bugs Phase 8's property test found and this project fixed by hand
//! (see docs/PLAN.md's Phase 8 section):
//!
//! - a container's `childCount` attribute must match the number of
//!   entries you actually get back when you browse into it;
//! - every entry's `parentID` must match the container you just browsed.
//!
//! It also checks the fail-closed contract (an unknown `ObjectID` faults
//! with UPnP error 701, not a crash or a wrong answer) and that a real
//! item found during the walk plays back over `/item/{id}`.
//!
//! Usage: `cargo run --example verify_music_library -- [host] [port]`
//! (defaults: 127.0.0.1 8200) while `dlna-rs` is running with at least one
//! configured view and one real audio file under it.

use std::collections::{HashMap, HashSet};
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

/// A Browse response, unescaped once so the tag-scraping helpers below can
/// just look for plain `<container ...>`/`<item ...>` text.
struct BrowseResult {
    status: u16,
    body: String,
}

fn browse(addr: &str, object_id: &str) -> BrowseResult {
    let response = request(
        addr,
        "POST",
        "/ContentDirectory/control",
        &[("Content-Type", "text/xml")],
        Some(&browse_body(object_id, "BrowseDirectChildren")),
    );
    let body = String::from_utf8_lossy(&response.body)
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&");
    BrowseResult {
        status: response.status,
        body,
    }
}

#[derive(Debug, Clone)]
struct Entry {
    is_container: bool,
    id: String,
    parent_id: String,
    child_count: Option<u64>,
}

/// Pulls every `<container ...>`/`<item ...>` tag's `id`, `parentID`, and
/// (for containers) `childCount` attributes out of a DIDL-Lite fragment.
/// Good enough for scraping our own server's output; not a real parser.
fn parse_entries(didl: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut pos = 0;
    loop {
        let container_pos = didl[pos..].find("<container ");
        let item_pos = didl[pos..].find("<item ");
        let rel = match (container_pos, item_pos) {
            (Some(c), Some(i)) => c.min(i),
            (Some(c), None) => c,
            (None, Some(i)) => i,
            (None, None) => break,
        };
        let start = pos + rel;
        let is_container = didl[start..].starts_with("<container ");
        let tag_end = didl[start..]
            .find('>')
            .map(|i| start + i)
            .unwrap_or(didl.len());
        let tag = &didl[start..tag_end];
        entries.push(Entry {
            is_container,
            id: attr(tag, "id").unwrap_or_default(),
            parent_id: attr(tag, "parentID").unwrap_or_default(),
            child_count: attr(tag, "childCount").and_then(|s| s.parse().ok()),
        });
        pos = tag_end;
    }
    entries
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = tag[start..].find('"')?;
    Some(tag[start..start + end].to_string())
}

fn number_returned(soap_body: &str) -> Option<u64> {
    let start = soap_body.find("<NumberReturned>")? + "<NumberReturned>".len();
    let end = soap_body[start..].find("</NumberReturned>")?;
    soap_body[start..start + end].parse().ok()
}

fn check(label: &str, condition: bool) -> bool {
    println!("[{}] {label}", if condition { "PASS" } else { "FAIL" });
    condition
}

/// Walks the whole tree from root, breadth first, checking two things at
/// every container: the number of entries actually returned matches
/// `NumberReturned`, and each entry's `parentID` names the container we
/// just browsed. A container's `childCount`, recorded when it was first
/// seen as an entry, is checked against its real child count once we
/// browse into it. Capped so a real, large library doesn't take forever.
fn walk_and_check(addr: &str) -> (bool, Option<String>) {
    let mut all_passed = true;
    let mut first_item_id = None;
    let mut expected_child_counts: HashMap<String, u64> = HashMap::new();
    let mut visited = HashSet::new();
    let mut queue = vec!["0".to_string()];
    let mut containers_checked = 0;

    while let Some(id) = queue.pop() {
        if !visited.insert(id.clone()) || containers_checked >= 200 {
            continue;
        }
        containers_checked += 1;

        let result = browse(addr, &id);
        if !check(&format!("Browse({id}) returns 200"), result.status == 200) {
            all_passed = false;
            continue;
        }

        let entries = parse_entries(&result.body);
        let returned = number_returned(&result.body);
        all_passed &= check(
            &format!("Browse({id}): NumberReturned matches entries actually in the response"),
            returned == Some(entries.len() as u64),
        );

        if let Some(&expected) = expected_child_counts.get(&id) {
            all_passed &= check(
                &format!(
                    "Browse({id}): childCount seen at the parent ({expected}) matches what browsing in returns ({})",
                    entries.len()
                ),
                expected == entries.len() as u64,
            );
        }

        for entry in &entries {
            all_passed &= check(
                &format!(
                    "{} {}: parentID is {id}",
                    if entry.is_container {
                        "container"
                    } else {
                        "item"
                    },
                    entry.id
                ),
                entry.parent_id == id,
            );
            if entry.is_container {
                if let Some(count) = entry.child_count {
                    expected_child_counts.insert(entry.id.clone(), count);
                }
                queue.push(entry.id.clone());
            } else if first_item_id.is_none() {
                first_item_id = Some(entry.id.clone());
            }
        }
    }

    (all_passed, first_item_id)
}

fn check_fault_701(addr: &str) -> bool {
    let result = browse(addr, "this-object-id-should-not-exist-anywhere");
    check(
        "Browse with an unknown ObjectID returns UPnP fault 701 (No Such Object)",
        result.body.contains("<errorCode>701</errorCode>"),
    )
}

fn check_playback(addr: &str, item_id: &str) -> bool {
    let path = format!("/item/{item_id}");
    let response = request(addr, "GET", &path, &[], None);
    let mut passed = check(&format!("GET {path} returns 200"), response.status == 200);
    passed &= check("response body is non-empty", !response.body.is_empty());
    let content_length: usize = response
        .headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    passed &= check(
        "Content-Length matches the actual body length",
        content_length == response.body.len(),
    );
    passed
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let host = args.get(1).map_or("127.0.0.1", String::as_str);
    let port = args.get(2).map_or("8200", String::as_str);
    let addr = format!("{host}:{port}");

    println!("Walking the whole tree from root at {addr}...\n");
    let (walk_passed, first_item_id) = walk_and_check(&addr);

    println!();
    let fault_passed = check_fault_701(&addr);

    println!();
    let playback_passed = match &first_item_id {
        Some(item_id) => {
            println!("Found item {item_id}; checking playback...");
            check_playback(&addr, item_id)
        }
        None => check("found at least one real item somewhere in the tree", false),
    };

    println!();
    if walk_passed && fault_passed && playback_passed {
        println!("All checks passed.");
    } else {
        println!("Some checks FAILED - see above.");
        std::process::exit(1);
    }
}
