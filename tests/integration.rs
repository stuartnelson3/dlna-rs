//! Spins up the real HTTP server on an ephemeral loopback port and drives
//! it with a plain HTTP client, per the testing strategy in docs/PLAN.md:
//! this is the automated half of Phases 3, 5, and 6's exit criteria (the
//! manual half is `examples/verify_item_playback.rs`/a real client against
//! a running instance on the LAN).

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::time::SystemTime;

use dlna_rs::config::{
    LibraryConfig, MediaConfig, MediaDirectory, MediaKind, RecentlyAddedConfig, View,
};
use dlna_rs::content::composite::CompositeContentSource;
use dlna_rs::content::folder::FolderMirror;
use dlna_rs::core::http::HttpServer;
use dlna_rs::index::{IndexBuilder, ObjectId, SharedIndex};
use dlna_rs::transform::passthrough::PassthroughSource;
use quick_xml::Reader;
use quick_xml::events::Event;
use tempfile::TempDir;
use uuid::Uuid;

const TRACK_CONTENT: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// Builds a real fixture directory on disk (Phase 6's item-serving reads
/// actual file bytes, so a fake path in the index isn't enough anymore)
/// and an `Index` that mirrors it.
fn fixture_media_dir() -> (TempDir, FolderMirror, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let track_path = dir.path().join("01 - Track.mp3");
    std::fs::write(&track_path, TRACK_CONTENT).unwrap();

    let mut builder = IndexBuilder::new();
    let album = builder.add_container(&ObjectId::root(), "Album".to_string());
    builder.add_item(
        &album,
        "01 - Track.mp3".to_string(),
        track_path.clone(),
        TRACK_CONTENT.len() as u64,
        SystemTime::UNIX_EPOCH,
    );
    (
        dir,
        FolderMirror::new(SharedIndex::new(builder.build())),
        dir_path_of(&track_path),
    )
}

fn dir_path_of(path: &std::path::Path) -> PathBuf {
    path.parent().unwrap().to_path_buf()
}

async fn start_server() -> (String, TempDir, tokio::task::JoinHandle<()>) {
    let (dir, content_source, media_root) = fixture_media_dir();
    let server = HttpServer::bind(
        Ipv4Addr::LOCALHOST,
        0,
        "integration-test".to_string(),
        Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap(),
        content_source,
        PassthroughSource,
        dlna_rs::metadata::tags::TagMetadata::new(vec![media_root.clone()]),
        vec![media_root],
    )
    .await
    .expect("failed to bind test HTTP server");
    let addr = server.local_addr().expect("bound server has a local addr");
    let handle = tokio::spawn(server.serve());
    (format!("http://{addr}"), dir, handle)
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
    let (base, _dir, _handle) = start_server().await;
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
    let (base, _dir, _handle) = start_server().await;

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
    let (base, _dir, _handle) = start_server().await;
    let response = reqwest::get(format!("{base}/nope")).await.unwrap();
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn browse_direct_children_of_root_over_real_http() {
    let (base, _dir, _handle) = start_server().await;
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

async fn find_item_url(base: &str) -> String {
    let client = reqwest::Client::new();
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
    format!("{base}/item/{item_id}")
}

#[tokio::test]
async fn browse_metadata_of_an_item_over_real_http() {
    let (base, _dir, _handle) = start_server().await;
    let client = reqwest::Client::new();

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
    let (base, _dir, _handle) = start_server().await;
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

#[tokio::test]
async fn gets_the_whole_item_with_no_range_header() {
    let (base, _dir, _handle) = start_server().await;
    let item_url = find_item_url(&base).await;

    let response = reqwest::get(&item_url).await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "audio/mpeg"
    );
    assert_eq!(response.headers().get("accept-ranges").unwrap(), "bytes");
    assert_eq!(
        response.headers().get("content-length").unwrap(),
        &TRACK_CONTENT.len().to_string()
    );
    assert!(
        response
            .headers()
            .get("contentfeatures.dlna.org")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("DLNA.ORG_PN=MP3")
    );

    let body = response.bytes().await.unwrap();
    assert_eq!(&body[..], TRACK_CONTENT);
}

#[tokio::test]
async fn head_returns_the_same_headers_with_no_body() {
    let (base, _dir, _handle) = start_server().await;
    let item_url = find_item_url(&base).await;

    let client = reqwest::Client::new();
    let response = client.head(&item_url).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers().get("content-length").unwrap(),
        &TRACK_CONTENT.len().to_string()
    );
    let body = response.bytes().await.unwrap();
    assert!(body.is_empty());
}

#[tokio::test]
async fn mid_file_range_returns_exactly_the_requested_bytes() {
    let (base, _dir, _handle) = start_server().await;
    let item_url = find_item_url(&base).await;

    let client = reqwest::Client::new();
    let response = client
        .get(&item_url)
        .header("Range", "bytes=5-9")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 206);
    assert_eq!(
        response.headers().get("content-range").unwrap(),
        &format!("bytes 5-9/{}", TRACK_CONTENT.len())
    );
    assert_eq!(response.headers().get("content-length").unwrap(), "5");

    let body = response.bytes().await.unwrap();
    assert_eq!(&body[..], &TRACK_CONTENT[5..10]);
}

#[tokio::test]
async fn suffix_range_returns_the_last_n_bytes() {
    let (base, _dir, _handle) = start_server().await;
    let item_url = find_item_url(&base).await;

    let client = reqwest::Client::new();
    let response = client
        .get(&item_url)
        .header("Range", "bytes=-4")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 206);
    let body = response.bytes().await.unwrap();
    assert_eq!(&body[..], &TRACK_CONTENT[TRACK_CONTENT.len() - 4..]);
}

#[tokio::test]
async fn out_of_bounds_range_is_416_with_content_range_star() {
    let (base, _dir, _handle) = start_server().await;
    let item_url = find_item_url(&base).await;

    let client = reqwest::Client::new();
    let response = client
        .get(&item_url)
        .header(
            "Range",
            format!(
                "bytes={}-{}",
                TRACK_CONTENT.len() + 100,
                TRACK_CONTENT.len() + 200
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 416);
    assert_eq!(
        response.headers().get("content-range").unwrap(),
        &format!("bytes */{}", TRACK_CONTENT.len())
    );
}

#[tokio::test]
async fn multi_range_is_416() {
    let (base, _dir, _handle) = start_server().await;
    let item_url = find_item_url(&base).await;

    let client = reqwest::Client::new();
    let response = client
        .get(&item_url)
        .header("Range", "bytes=0-1,2-3")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 416);
}

#[tokio::test]
async fn unknown_item_id_is_404() {
    let (base, _dir, _handle) = start_server().await;
    let response = reqwest::get(format!("{base}/item/999999")).await.unwrap();
    assert_eq!(response.status(), 404);
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

/// A separate, self-contained fixture for the rescan test - uses the real
/// scanner (not a hand-built index like the other tests here) since this
/// test is specifically about the scan -> SharedIndex::replace pipeline,
/// not about Browse/DIDL correctness in isolation.
async fn start_server_with_rescan(
    interval: std::time::Duration,
) -> (
    String,
    TempDir,
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("01 - Track.mp3"), TRACK_CONTENT).unwrap();

    let media = MediaConfig {
        directories: vec![MediaDirectory {
            path: dir.path().to_path_buf(),
            kind: MediaKind::Audio,
        }],
        follow_symlinks: false,
        exclude_patterns: vec![],
    };
    let tags = dlna_rs::metadata::tags::TagMetadata::new(media.roots());
    let shared_index = SharedIndex::new(dlna_rs::scanner::scan(&media, &tags));
    let content_source = FolderMirror::new(shared_index.clone());

    let server = HttpServer::bind(
        Ipv4Addr::LOCALHOST,
        0,
        "rescan-test".to_string(),
        Uuid::parse_str("22222222-3333-4444-5555-666666666666").unwrap(),
        content_source,
        PassthroughSource,
        tags,
        vec![dir.path().to_path_buf()],
    )
    .await
    .expect("failed to bind test HTTP server");
    let addr = server.local_addr().unwrap();
    let http_handle = tokio::spawn(server.serve());
    // on_startup=false: the fixture's initial scan above already covers
    // "on_startup=true" (Phase 1's own config option); this test is
    // specifically about the periodic re-scan.
    let scan_tags =
        dlna_rs::rescan::build_tags(&dlna_rs::config::TagCacheConfig::default(), media.roots());
    let rescan_handle = tokio::spawn(dlna_rs::rescan::run(
        media,
        shared_index,
        interval,
        false,
        scan_tags,
    ));
    (format!("http://{addr}"), dir, http_handle, rescan_handle)
}

async fn browse_didl(base: &str, object_id: &str) -> String {
    let client = reqwest::Client::new();
    let text = client
        .post(format!("{base}/ContentDirectory/control"))
        .body(browse_request_body(object_id, "BrowseDirectChildren"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

/// Scans a real directory of tracks and serves it through
/// `CompositeContentSource`, built from `library` the same way `main.rs`
/// builds it. Used for the music-library-view tests below, which need
/// the real Albums/Artists grouping, not the hand-built single-album
/// fixture the earlier tests in this file use.
async fn start_server_with_library(
    dir: &TempDir,
    library: LibraryConfig,
) -> (String, tokio::task::JoinHandle<()>) {
    let media = MediaConfig {
        directories: vec![MediaDirectory {
            path: dir.path().to_path_buf(),
            kind: MediaKind::Audio,
        }],
        follow_symlinks: false,
        exclude_patterns: vec![],
    };
    let tags = dlna_rs::metadata::tags::TagMetadata::new(media.roots());
    let shared_index = SharedIndex::new(dlna_rs::scanner::scan(&media, &tags));
    let content_source = CompositeContentSource::from_config(&library, shared_index);

    let server = HttpServer::bind(
        Ipv4Addr::LOCALHOST,
        0,
        "library-test".to_string(),
        Uuid::parse_str("33333333-4444-5555-6666-777777777777").unwrap(),
        content_source,
        PassthroughSource,
        tags,
        vec![dir.path().to_path_buf()],
    )
    .await
    .expect("failed to bind test HTTP server");
    let addr = server.local_addr().unwrap();
    let handle = tokio::spawn(server.serve());
    (format!("http://{addr}"), handle)
}

#[tokio::test]
async fn root_browse_shows_exactly_the_configured_views() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Track.mp3"), TRACK_CONTENT).unwrap();

    let library = LibraryConfig {
        views: vec![View::Folders, View::Albums],
        recently_added: RecentlyAddedConfig::default(),
    };
    let (base, _handle) = start_server_with_library(&dir, library).await;

    let root_didl = browse_didl(&base, "0").await;
    assert!(root_didl.contains(">Folders<"));
    assert!(root_didl.contains(">Albums<"));
    assert!(!root_didl.contains(">Artists<"));
}

#[tokio::test]
async fn recently_added_songs_over_http_is_capped_at_the_configured_count() {
    let dir = tempfile::tempdir().unwrap();
    // More tracks than the configured count below - this is the exit
    // criterion from docs/PLAN.md Phase 8: more than the configured
    // count must not break the view.
    for n in 0..5 {
        std::fs::write(dir.path().join(format!("Track {n}.mp3")), TRACK_CONTENT).unwrap();
    }

    let library = LibraryConfig {
        views: vec![View::RecentlyAddedSongs],
        recently_added: RecentlyAddedConfig {
            songs_count: 3,
            albums_count: 20,
            max_age_days: 0,
        },
    };
    let (base, _handle) = start_server_with_library(&dir, library).await;

    let root_didl = browse_didl(&base, "0").await;
    let recent_id = extract_attr(&root_didl, "container", "id");

    let songs_didl = browse_didl(&base, &recent_id).await;
    assert_eq!(songs_didl.matches("<item ").count(), 3);
}

#[tokio::test]
async fn new_file_appears_after_one_rescan_interval_with_no_restart() {
    let interval = std::time::Duration::from_millis(50);
    let (base, dir, _http, _rescan) = start_server_with_rescan(interval).await;

    // The scanner mounts the one configured directory as its own
    // top-level container (named after its basename), so the files
    // themselves are one level below root, not direct children of "0".
    let root_didl = browse_didl(&base, "0").await;
    let media_dir_id = extract_attr(&root_didl, "container", "id");

    // Titles are parsed (track number and extension stripped), not the
    // raw filename - "01 - Track.mp3" scans to "Track".
    let before = browse_didl(&base, &media_dir_id).await;
    assert!(before.contains(">Track<"));
    assert!(!before.contains(">New Track<"));

    // Add a file after the server (and rescan timer) are already running -
    // this is the "no restart needed" part of the exit criterion.
    std::fs::write(dir.path().join("02 - New Track.mp3"), b"more content").unwrap();

    tokio::time::sleep(interval * 4).await;

    let after = browse_didl(&base, &media_dir_id).await;
    assert!(after.contains(">Track<"));
    assert!(after.contains(">New Track<"));
}

/// A 1x1 transparent PNG - the smallest real, valid PNG there is.
fn tiny_png() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ]
}

/// Writes a real, taggable MP3 (30 repeats of one silent MPEG-1 Layer
/// III frame - lofty needs several consistent frames to trust the file,
/// verified directly in `metadata::tags`'s own tests) with a real
/// artist/album/genre tag and a real embedded cover art picture,
/// written with lofty's own API - the same fixture-construction
/// approach used throughout this project, real files over hand-built
/// binary blobs.
fn write_tagged_mp3_with_art(path: &std::path::Path) {
    use lofty::config::WriteOptions;
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::picture::{MimeType, Picture, PictureType};
    use lofty::tag::{Accessor, Tag};

    let mut frame = vec![0xFFu8, 0xFB, 0x90, 0x00];
    frame.resize(417, 0);
    std::fs::write(path, frame.repeat(30)).unwrap();

    let mut tagged_file = lofty::read_from_path(path).unwrap();
    if tagged_file.primary_tag().is_none() {
        tagged_file.insert_tag(Tag::new(tagged_file.primary_tag_type()));
    }
    let tag = tagged_file.primary_tag_mut().unwrap();
    tag.set_artist("Integration Test Artist".to_string());
    tag.set_album("Integration Test Album".to_string());
    tag.set_genre("Integration Test Genre".to_string());
    tag.push_picture(
        Picture::unchecked(tiny_png())
            .pic_type(PictureType::CoverFront)
            .mime_type(MimeType::Png)
            .build(),
    );
    tagged_file
        .save_to_path(path, WriteOptions::default())
        .unwrap();
}

#[tokio::test]
async fn real_tags_and_cover_art_are_served_over_real_http() {
    let dir = tempfile::tempdir().unwrap();
    write_tagged_mp3_with_art(&dir.path().join("track.mp3"));

    let library = LibraryConfig {
        views: vec![View::Folders],
        recently_added: RecentlyAddedConfig::default(),
    };
    let (base, _handle) = start_server_with_library(&dir, library).await;

    // Two hops: root -> the "Folders" mount -> the real mounted
    // directory (FolderMirror names it after the tempdir's basename) ->
    // only then the track itself.
    let root_didl = browse_didl(&base, "0").await;
    let folders_mount_id = extract_attr(&root_didl, "container", "id");
    let mount_didl = browse_didl(&base, &folders_mount_id).await;
    let folder_id = extract_attr(&mount_didl, "container", "id");
    let track_didl = browse_didl(&base, &folder_id).await;

    assert!(track_didl.contains("<dc:creator>Integration Test Artist</dc:creator>"));
    assert!(track_didl.contains("<upnp:artist>Integration Test Artist</upnp:artist>"));
    assert!(track_didl.contains("<upnp:album>Integration Test Album</upnp:album>"));
    assert!(track_didl.contains("<upnp:genre>Integration Test Genre</upnp:genre>"));
    assert!(track_didl.contains("<upnp:albumArtURI>"));

    let item_id = extract_attr(&track_didl, "item", "id");
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{base}/art/{item_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers().get("content-type").unwrap(), "image/png");
    let body = response.bytes().await.unwrap();
    assert_eq!(&body[..], &tiny_png()[..]);
}

#[tokio::test]
async fn an_external_cover_file_is_served_over_real_http_with_no_embedded_picture() {
    let dir = tempfile::tempdir().unwrap();
    let mut frame = vec![0xFFu8, 0xFB, 0x90, 0x00];
    frame.resize(417, 0);
    std::fs::write(dir.path().join("track.mp3"), frame.repeat(30)).unwrap();
    std::fs::write(dir.path().join("cover.jpg"), tiny_png()).unwrap();

    let library = LibraryConfig {
        views: vec![View::Folders],
        recently_added: RecentlyAddedConfig::default(),
    };
    let (base, _handle) = start_server_with_library(&dir, library).await;

    let root_didl = browse_didl(&base, "0").await;
    let folders_mount_id = extract_attr(&root_didl, "container", "id");
    let mount_didl = browse_didl(&base, &folders_mount_id).await;
    let folder_id = extract_attr(&mount_didl, "container", "id");
    let track_didl = browse_didl(&base, &folder_id).await;
    assert!(track_didl.contains("<upnp:albumArtURI>"));

    let item_id = extract_attr(&track_didl, "item", "id");
    let response = reqwest::get(format!("{base}/art/{item_id}")).await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "image/jpeg"
    );
    let body = response.bytes().await.unwrap();
    assert_eq!(&body[..], &tiny_png()[..]);
}

#[tokio::test]
async fn art_for_an_item_with_no_embedded_picture_is_404() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("plain.mp3"), TRACK_CONTENT).unwrap();

    let library = LibraryConfig {
        views: vec![View::Folders],
        recently_added: RecentlyAddedConfig::default(),
    };
    let (base, _handle) = start_server_with_library(&dir, library).await;

    let root_didl = browse_didl(&base, "0").await;
    let folders_mount_id = extract_attr(&root_didl, "container", "id");
    let mount_didl = browse_didl(&base, &folders_mount_id).await;
    let folder_id = extract_attr(&mount_didl, "container", "id");
    let track_didl = browse_didl(&base, &folder_id).await;
    assert!(!track_didl.contains("<upnp:albumArtURI>"));

    let item_id = extract_attr(&track_didl, "item", "id");
    let response = reqwest::get(format!("{base}/art/{item_id}")).await.unwrap();
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn art_for_an_unknown_id_is_404() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("plain.mp3"), TRACK_CONTENT).unwrap();

    let library = LibraryConfig {
        views: vec![View::Folders],
        recently_added: RecentlyAddedConfig::default(),
    };
    let (base, _handle) = start_server_with_library(&dir, library).await;

    let response = reqwest::get(format!("{base}/art/does-not-exist"))
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn browsing_into_a_tag_artist_whose_name_contains_an_ampersand_works_over_real_http() {
    // Real bug report: clicking the artist "Danger Mouse & Black
    // Thought" in a real DLNA app reset the connection. Reproduces the
    // full real wire round-trip (SOAP request -> response -> a second
    // real SOAP request using the id straight from that response) that
    // an in-process unit test skips entirely.
    let dir = tempfile::tempdir().unwrap();
    write_tagged_mp3_with_art(&dir.path().join("track.mp3"));
    {
        use lofty::file::{AudioFile, TaggedFileExt};
        use lofty::tag::Accessor;
        let path = dir.path().join("track.mp3");
        let mut tagged_file = lofty::read_from_path(&path).unwrap();
        let tag = tagged_file.primary_tag_mut().unwrap();
        tag.set_artist("Danger Mouse & Black Thought".to_string());
        tag.set_album("Cheat Codes".to_string());
        tagged_file
            .save_to_path(&path, lofty::config::WriteOptions::default())
            .unwrap();
    }

    let library = LibraryConfig {
        views: vec![View::Artists],
        recently_added: RecentlyAddedConfig::default(),
    };
    let (base, _handle) = start_server_with_library(&dir, library).await;

    let root_didl = browse_didl(&base, "0").await;
    let artists_mount_id = extract_attr(&root_didl, "container", "id");
    let artists_didl = browse_didl(&base, &artists_mount_id).await;
    // `browse_didl` only undoes the SOAP envelope's own escaping - the
    // DIDL-Lite body it returns is itself still real XML, so a literal
    // "&" in a real value is still spelled "&amp;" here, same as a real
    // client would see before parsing this string as its own document.
    assert!(
        artists_didl.contains("Danger Mouse &amp; Black Thought"),
        "expected the artist in the listing: {artists_didl}"
    );
    let artist_id = extract_attr(&artists_didl, "container", "id");
    assert_eq!(artist_id, "artists$tag-artist:danger mouse & black thought");

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{base}/ContentDirectory/control"))
        .body(browse_request_body(
            &xml_escape(&artist_id),
            "BrowseDirectChildren",
        ))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(
        status, 200,
        "browsing into the artist by its own listed id must not fault: {body}"
    );
}

/// A minimal, correct XML text escaper for building a well-formed
/// second request from a value pulled out of a first response - the
/// same escaping any real UPnP control point must apply, and the thing
/// missing if it naively re-interpolates unescaped text instead.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
