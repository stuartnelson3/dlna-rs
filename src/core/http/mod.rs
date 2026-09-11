//! The HTTP server: accept-loop plumbing around [`router`], [`description`],
//! [`scpd`], SOAP dispatch (`core::dispatch`), and item byte-serving
//! (`range`, `core::byte_source`).

// Not `pub`: these are implementation details of `HttpServer` below, which
// is the only thing outside this module that should depend on anything
// here. `router` in particular takes `hyper::Method` in its signature —
// keeping it crate-internal means a future hyper swap can't leak past
// `HttpServer`'s own (hyper-free) API.
pub(crate) mod description;
pub(crate) mod range;
pub(crate) mod router;
pub(crate) mod scpd;

use std::convert::Infallible;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_core::Stream;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::header::{CONTENT_LENGTH, RANGE};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{HeaderMap, Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use uuid::Uuid;

use self::description::DeviceInfo;
use self::router::Route;
use crate::core::art_source::ArtSource;
use crate::core::byte_source::{ByteSource, ByteStream};
use crate::core::content_source::ContentSource;
use crate::core::device::ServiceType;
use crate::core::didl::format;
use crate::core::{dispatch, soap};
use crate::index::{Entry, ObjectId};

/// Every response this server sends, whether a small XML/SOAP body
/// built in memory or a media file streamed lazily off disk (see
/// `item_response`/`streamed_body`) - one concrete type so `handle`'s
/// match arms and every helper below can share a single return type.
type RespBody = BoxBody<Bytes, io::Error>;

/// Wraps an already-in-memory body - every response except the item
/// byte-stream itself. `Full`'s error type is the uninhabited
/// `Infallible`, so there's really no error to convert, just a type
/// to satisfy `RespBody`'s shared `io::Error`.
fn full_body(bytes: Bytes) -> RespBody {
    Full::new(bytes).map_err(|never| match never {}).boxed()
}

/// Wraps a `ByteSource::read` stream as a response body - chunks reach
/// the client as they're read off disk, instead of waiting for the
/// whole file/range to sit in memory first.
fn streamed_body(stream: ByteStream) -> RespBody {
    StreamBody::new(FrameStream(stream)).boxed()
}

/// Adapts a `ByteSource`'s plain `Bytes` chunks to the `Frame`s a
/// hyper body must yield - the only place this file's streaming needs
/// to know about hyper's body-framing at all.
struct FrameStream(ByteStream);

impl Stream for FrameStream {
    type Item = io::Result<Frame<Bytes>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut()
            .0
            .as_mut()
            .poll_next(cx)
            .map(|next| next.map(|chunk| chunk.map(Frame::data)))
    }
}

pub struct HttpServer {
    listener: TcpListener,
    friendly_name: String,
    uuid: Uuid,
    interface_addr: Ipv4Addr,
    port: u16,
    content_source: Arc<dyn ContentSource>,
    byte_source: Arc<dyn ByteSource>,
    art_source: Arc<dyn ArtSource>,
    /// Canonicalized once at startup. Every item GET/HEAD re-canonicalizes
    /// the item's own path and checks it against these — defense in depth
    /// against `follow_symlinks` escaping the configured library (see
    /// docs/THREAT_MODEL.md). Checked at serve time, not just scan time,
    /// since a symlink's target can change between scans.
    media_roots: Vec<PathBuf>,
}

impl HttpServer {
    /// `content_source`/`byte_source` are generic rather than `Arc<dyn
    /// ...>` so callers outside this crate (`main.rs`, integration tests —
    /// each its own crate, since a package with both a lib and a bin
    /// target compiles them separately) never need to name or import
    /// either trait at all; they just pass a `FolderMirror` and a
    /// `PassthroughSource`, and this function does the type-erasure
    /// internally. That's what keeps `core::content_source`/
    /// `core::byte_source` `pub(crate)` instead of fully `pub`.
    ///
    /// Fails if any `media_root` doesn't exist — a media directory that's
    /// missing at startup is a configuration problem worth failing loudly
    /// on, not silently serving an empty library for.
    ///
    /// One more plain argument than clippy's default cap, for the eighth
    /// extension point (`art_source`) — a wrapper struct here would only
    /// exist to satisfy the lint, not to make any of the four call sites
    /// clearer, so this is a deliberate, named exception, not a lapse.
    #[allow(clippy::too_many_arguments)]
    pub async fn bind(
        interface_addr: Ipv4Addr,
        port: u16,
        friendly_name: String,
        uuid: Uuid,
        content_source: impl ContentSource + 'static,
        byte_source: impl ByteSource + 'static,
        art_source: impl ArtSource + 'static,
        media_roots: Vec<PathBuf>,
    ) -> std::io::Result<Arc<HttpServer>> {
        let listener = TcpListener::bind((interface_addr, port)).await?;
        let media_roots = media_roots
            .into_iter()
            .map(|root| {
                std::fs::canonicalize(&root).map_err(|err| {
                    std::io::Error::other(format!("media directory {}: {err}", root.display()))
                })
            })
            .collect::<std::io::Result<Vec<_>>>()?;
        Ok(Arc::new(HttpServer {
            listener,
            friendly_name,
            uuid,
            interface_addr,
            port,
            content_source: Arc::new(content_source),
            byte_source: Arc::new(byte_source),
            art_source: Arc::new(art_source),
            media_roots,
        }))
    }

    /// The address actually bound — useful when binding port `0` for a
    /// test and needing to know which ephemeral port the OS picked.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Accepts connections and serves them forever. Each connection gets
    /// its own task, so one connection erroring or panicking can't take
    /// down the others or the accept loop (see docs/THREAT_MODEL.md on
    /// panic policy).
    pub async fn serve(self: Arc<Self>) {
        loop {
            let (stream, _peer) = match self.listener.accept().await {
                Ok(accepted) => accepted,
                Err(err) => {
                    log::warn!("HTTP accept error: {err}");
                    continue;
                }
            };
            let server = self.clone();
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let service = service_fn(move |req| {
                    let server = server.clone();
                    async move { server.handle(req).await }
                });
                if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                    log::debug!("HTTP connection error: {err}");
                }
            });
        }
    }

    async fn handle(&self, req: Request<Incoming>) -> Result<Response<RespBody>, Infallible> {
        let route = router::route(req.method(), req.uri().path());
        Ok(match route {
            Route::DeviceDescription => xml_response(description::build(&DeviceInfo {
                friendly_name: &self.friendly_name,
                uuid: self.uuid,
                interface_addr: self.interface_addr,
                port: self.port,
            })),
            Route::Scpd(service) => xml_response(scpd::document(service).to_string()),
            Route::Control(service) => self.handle_control(service, req).await,
            Route::Item(id) => self.handle_item(&id, req.method(), req.headers()).await,
            Route::Art(id) => self.handle_art(&id).await,
            Route::NotFound => not_found(),
        })
    }

    async fn handle_control(
        &self,
        service: ServiceType,
        req: Request<Incoming>,
    ) -> Response<RespBody> {
        let Some(len) = content_length(&req) else {
            return bad_request();
        };
        if len > soap::MAX_BODY_LEN {
            return payload_too_large();
        }

        let body = match req.into_body().collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(_) => return bad_request(),
        };

        let action = match soap::parse_action(&body) {
            Ok(action) => action,
            Err(_) => return soap_fault_response(soap::build_fault(401, "Invalid Action")),
        };

        let base_url = format!("http://{}:{}", self.interface_addr, self.port);
        match dispatch::handle(service, &action, self.content_source.as_ref(), &base_url) {
            Ok(body) => soap_ok_response(body),
            Err(err) => soap_fault_response(err.into_fault()),
        }
    }

    async fn handle_item(
        &self,
        id: &ObjectId,
        method: &Method,
        headers: &HeaderMap,
    ) -> Response<RespBody> {
        let Some(resolved) = self.resolve_item_path(id, "serve").await else {
            return not_found();
        };

        let file_size = match tokio::fs::metadata(&resolved).await {
            Ok(metadata) => metadata.len(),
            Err(err) => {
                log::warn!("couldn't stat {}: {err}", resolved.display());
                return not_found();
            }
        };

        let range_header = headers.get(RANGE).and_then(|v| v.to_str().ok());
        let outcome = match (range_header, self.byte_source.supports_range()) {
            (Some(raw), true) => match range::parse(raw, file_size) {
                Ok(range) => Outcome::Partial(range),
                Err(_) => return range_not_satisfiable(file_size),
            },
            // No Range header, or the active ByteSource can't seek: serve
            // the whole thing with 200 rather than lying about Range
            // support (see core::byte_source's doc comment).
            _ => Outcome::Full,
        };

        let stream = match self.byte_source.read(&resolved, outcome.range()).await {
            Ok(stream) => stream,
            Err(err) => {
                log::warn!("failed to read {}: {err}", resolved.display());
                return not_found();
            }
        };

        let response = item_response(&outcome, &resolved, file_size, stream);
        if *method == Method::HEAD {
            without_body(response)
        } else {
            response
        }
    }

    /// No Range support here, unlike `handle_item` — real embedded cover
    /// art is small, a single whole-body response is enough, and the
    /// simpler handler is the deliberate trade for that (see
    /// `core::art_source`'s doc comment).
    async fn handle_art(&self, id: &ObjectId) -> Response<RespBody> {
        let Some(resolved) = self.resolve_item_path(id, "serve its art").await else {
            return not_found();
        };

        let Some(art) = self.art_source.art(&resolved).await else {
            return not_found();
        };

        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", art.mime)
            .header("Content-Length", art.bytes.len().to_string())
            .body(full_body(art.bytes))
            .expect("a real MIME string and byte length are always valid header values")
    }

    /// Resolves `id` to a real, verified-within-roots file path, or
    /// `None` (already logged) if it isn't a real item or its path
    /// resolves outside the configured media roots - the shared
    /// preamble `handle_item`/`handle_art` both need before doing
    /// anything specific to serving the file or its art. `purpose`
    /// names what the caller is about to do, for the warning log line.
    async fn resolve_item_path(&self, id: &ObjectId, purpose: &str) -> Option<PathBuf> {
        let Some(Entry::Item(item)) = self.content_source.entry(id) else {
            return None;
        };
        match self.verify_within_roots(&item.path).await {
            Ok(resolved) => Some(resolved),
            Err(_) => {
                log::warn!(
                    "item {id} path {} resolved outside the configured media roots; refusing to {purpose}",
                    item.path.display()
                );
                None
            }
        }
    }

    async fn verify_within_roots(&self, path: &Path) -> std::io::Result<PathBuf> {
        resolve_within_roots(path, &self.media_roots).await
    }
}

/// Canonicalizes `path` and verifies the result is inside one of `roots`
/// (also assumed already-canonical). A standalone function, not a method,
/// specifically so it's testable against real symlinks without needing a
/// running `HttpServer` — see the property test below.
async fn resolve_within_roots(path: &Path, roots: &[PathBuf]) -> std::io::Result<PathBuf> {
    let resolved = tokio::fs::canonicalize(path).await?;
    if roots.iter().any(|root| resolved.starts_with(root)) {
        Ok(resolved)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "outside configured media roots",
        ))
    }
}

enum Outcome {
    Full,
    Partial(crate::core::byte_source::ByteRange),
}

impl Outcome {
    fn range(&self) -> Option<crate::core::byte_source::ByteRange> {
        match self {
            Outcome::Full => None,
            Outcome::Partial(range) => Some(*range),
        }
    }
}

fn item_response(
    outcome: &Outcome,
    path: &Path,
    file_size: u64,
    stream: ByteStream,
) -> Response<RespBody> {
    let mut builder = Response::builder()
        .header("Content-Type", format::mime_for(path))
        .header("Accept-Ranges", "bytes")
        .header(
            "contentFeatures.dlna.org",
            format::dlna_content_features(path),
        )
        .header("transferMode.dlna.org", "Streaming");

    builder = match outcome {
        Outcome::Full => builder
            .status(StatusCode::OK)
            .header("Content-Length", file_size.to_string()),
        Outcome::Partial(range) => builder
            .status(StatusCode::PARTIAL_CONTENT)
            .header("Content-Length", range.len().to_string())
            .header(
                "Content-Range",
                format!("bytes {}-{}/{file_size}", range.start, range.end),
            ),
    };

    builder
        .body(streamed_body(stream))
        .expect("header values built from our own format/range types are always valid")
}

/// Drops the body without ever polling it - for a HEAD request, `stream`
/// is dropped unread here, so a HEAD never actually reads the file off
/// disk just to discard the bytes.
fn without_body(response: Response<RespBody>) -> Response<RespBody> {
    let (parts, _) = response.into_parts();
    Response::from_parts(parts, full_body(Bytes::new()))
}

fn range_not_satisfiable(file_size: u64) -> Response<RespBody> {
    Response::builder()
        .status(StatusCode::RANGE_NOT_SATISFIABLE)
        .header("Content-Range", format!("bytes */{file_size}"))
        .body(full_body(Bytes::new()))
        .expect("static header name/value are always valid")
}

fn content_length(req: &Request<Incoming>) -> Option<usize> {
    req.headers()
        .get(CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

fn xml_response(body: String) -> Response<RespBody> {
    response(StatusCode::OK, body)
}

fn soap_ok_response(body: String) -> Response<RespBody> {
    response(StatusCode::OK, body)
}

// UPnP convention: a SOAP fault is carried in the body of an HTTP 500
// response, not a 200 - the body's <s:Fault> element is what actually
// describes the error.
fn soap_fault_response(body: String) -> Response<RespBody> {
    response(StatusCode::INTERNAL_SERVER_ERROR, body)
}

fn response(status: StatusCode, body: String) -> Response<RespBody> {
    Response::builder()
        .status(status)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .body(full_body(Bytes::from(body)))
        .expect("static header name/value are always valid")
}

fn not_found() -> Response<RespBody> {
    empty_response(StatusCode::NOT_FOUND)
}

fn bad_request() -> Response<RespBody> {
    empty_response(StatusCode::BAD_REQUEST)
}

fn payload_too_large() -> Response<RespBody> {
    empty_response(StatusCode::PAYLOAD_TOO_LARGE)
}

fn empty_response(status: StatusCode) -> Response<RespBody> {
    Response::builder()
        .status(status)
        .body(full_body(Bytes::new()))
        .expect("static header name/value are always valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Runtime::new().unwrap().block_on(fut)
    }

    #[test]
    fn file_inside_root_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("track.mp3");
        std::fs::write(&file, b"x").unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        assert!(block_on(resolve_within_roots(&file, &[root])).is_ok());
    }

    #[test]
    fn file_outside_every_root_is_rejected() {
        let root_dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let file = elsewhere.path().join("secret.txt");
        std::fs::write(&file, b"shh").unwrap();
        let root = std::fs::canonicalize(root_dir.path()).unwrap();

        assert!(block_on(resolve_within_roots(&file, &[root])).is_err());
    }

    #[test]
    fn symlink_escaping_the_root_is_rejected() {
        let root_dir = tempfile::tempdir().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();
        let secret = outside_dir.path().join("secret.txt");
        std::fs::write(&secret, b"shh").unwrap();
        let link = root_dir.path().join("link.mp3");
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        let root = std::fs::canonicalize(root_dir.path()).unwrap();

        // The classic follow_symlinks escape (see docs/THREAT_MODEL.md):
        // link.mp3 lives inside the configured root, but resolves outside
        // it, and must be rejected regardless.
        assert!(block_on(resolve_within_roots(&link, &[root])).is_err());
    }

    #[test]
    fn symlink_staying_inside_the_root_is_accepted() {
        let root_dir = tempfile::tempdir().unwrap();
        let real = root_dir.path().join("real.mp3");
        std::fs::write(&real, b"x").unwrap();
        let link = root_dir.path().join("link.mp3");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let root = std::fs::canonicalize(root_dir.path()).unwrap();

        assert!(block_on(resolve_within_roots(&link, &[root])).is_ok());
    }

    #[test]
    fn sibling_directory_with_a_prefix_matching_name_is_not_wrongly_accepted() {
        // The classic naive-string-prefix bug: "/media/music-private" must
        // not be treated as inside "/media/music". Path::starts_with
        // already compares components, not raw strings, so this is really
        // a regression guard on that assumption holding.
        let parent = tempfile::tempdir().unwrap();
        let root_dir = parent.path().join("music");
        let sibling_dir = parent.path().join("music-private");
        std::fs::create_dir(&root_dir).unwrap();
        std::fs::create_dir(&sibling_dir).unwrap();
        let file = sibling_dir.join("secret.mp3");
        std::fs::write(&file, b"shh").unwrap();
        let root = std::fs::canonicalize(&root_dir).unwrap();

        assert!(block_on(resolve_within_roots(&file, &[root])).is_err());
    }

    proptest::proptest! {
        /// Phase 6's property-test task: path resolution never escapes
        /// the media root. Arbitrary nested *real* files created inside a
        /// tempdir root must always be accepted — a regression here would
        /// mean legitimate library files start silently 404ing.
        #[test]
        fn arbitrary_nested_files_inside_root_are_always_accepted(
            segments in proptest::collection::vec("[a-zA-Z0-9_]{1,8}", 1..4)
        ) {
            let dir = tempfile::tempdir().unwrap();
            let mut path = dir.path().to_path_buf();
            for segment in &segments[..segments.len() - 1] {
                path.push(segment);
            }
            std::fs::create_dir_all(&path).unwrap();
            path.push(format!("{}.mp3", segments.last().unwrap()));
            std::fs::write(&path, b"x").unwrap();
            let root = std::fs::canonicalize(dir.path()).unwrap();

            let result = block_on(resolve_within_roots(&path, &[root]));
            proptest::prop_assert!(result.is_ok());
        }
    }

    /// A `ContentSource` that panics on one specific ID and behaves
    /// normally otherwise - built to prove, against a real running
    /// server, that the per-connection task in `serve()` really does
    /// isolate a panic (see docs/THREAT_MODEL.md's panic policy and
    /// `Cargo.toml`'s `panic = "unwind"` comment). Only usable from this
    /// test module - it names `ContentSource` directly, which nothing
    /// outside `core` may do (see docs/DESIGN.md's encapsulation rule).
    struct PanicsOnBoom;

    impl ContentSource for PanicsOnBoom {
        fn children(&self, _id: &ObjectId) -> Option<Vec<Entry>> {
            Some(Vec::new())
        }

        fn entry(&self, id: &ObjectId) -> Option<Entry> {
            if id.as_str() == "boom" {
                panic!("deliberate panic for the panic-isolation test");
            }
            Some(Entry::Container(crate::index::Container {
                id: ObjectId::root(),
                parent_id: None,
                title: String::new(),
                child_count: 0,
            }))
        }
    }

    fn browse_metadata_body(object_id: &str) -> String {
        format!(
            r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:Browse xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1"><ObjectID>{object_id}</ObjectID><BrowseFlag>BrowseMetadata</BrowseFlag><Filter>*</Filter><StartingIndex>0</StartingIndex><RequestedCount>0</RequestedCount><SortCriteria></SortCriteria></u:Browse></s:Body></s:Envelope>"#
        )
    }

    #[tokio::test]
    async fn a_panic_in_one_connection_task_does_not_take_down_the_server() {
        let server = HttpServer::bind(
            Ipv4Addr::LOCALHOST,
            0,
            "panic-test".to_string(),
            Uuid::new_v4(),
            PanicsOnBoom,
            crate::transform::passthrough::PassthroughSource,
            crate::metadata::tags::TagMetadata::new(Vec::new()),
            Vec::new(),
        )
        .await
        .expect("failed to bind test HTTP server");
        let addr = server.local_addr().unwrap();
        tokio::spawn(server.serve());

        let client = reqwest::Client::new();

        // BrowseMetadata on "boom" reaches PanicsOnBoom::entry, which
        // panics. That connection's own task dies mid-response, so the
        // client sees a network error, not an HTTP response of any
        // status - a panicking handler must never look like success.
        let panicking = client
            .post(format!("http://{addr}/ContentDirectory/control"))
            .body(browse_metadata_body("boom"))
            .send()
            .await;
        assert!(
            panicking.is_err(),
            "a panicking handler should drop the connection, not answer normally"
        );

        // A fresh request, a fresh connection: it must still succeed.
        // The accept loop and every other task are untouched by the
        // panic above - that's the whole point of one task per
        // connection.
        let recovered = client
            .post(format!("http://{addr}/ContentDirectory/control"))
            .body(browse_metadata_body("anything-else"))
            .send()
            .await
            .expect("the server must still be serving other connections");
        assert_eq!(recovered.status(), 200);
    }
}
