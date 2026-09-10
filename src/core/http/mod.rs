//! The HTTP server: accept-loop plumbing around [`router`], [`description`],
//! [`scpd`], and SOAP dispatch (`core::dispatch`). Range parsing/serving
//! (Phase 6) extends this, not replaces it.

// Not `pub`: these are implementation details of `HttpServer` below, which
// is the only thing outside this module that should depend on anything
// here. `router` in particular takes `hyper::Method` in its signature —
// keeping it crate-internal means a future hyper swap can't leak past
// `HttpServer`'s own (hyper-free) API.
pub(crate) mod description;
pub(crate) mod router;
pub(crate) mod scpd;

use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::CONTENT_LENGTH;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use uuid::Uuid;

use self::description::DeviceInfo;
use self::router::Route;
use crate::core::content_source::ContentSource;
use crate::core::{dispatch, soap};

pub struct HttpServer {
    listener: TcpListener,
    friendly_name: String,
    uuid: Uuid,
    interface_addr: Ipv4Addr,
    port: u16,
    content_source: Arc<dyn ContentSource>,
}

impl HttpServer {
    /// `content_source` is generic rather than `Arc<dyn ContentSource>` so
    /// callers outside this crate (`main.rs`, integration tests — each
    /// its own crate, since a package with both a lib and a bin target
    /// compiles them separately) never need to name or import
    /// `ContentSource` at all; they just pass a `FolderMirror` and this
    /// function does the type-erasure internally. That's what keeps
    /// `core::content_source` `pub(crate)` instead of fully `pub`.
    pub async fn bind(
        interface_addr: Ipv4Addr,
        port: u16,
        friendly_name: String,
        uuid: Uuid,
        content_source: impl ContentSource + 'static,
    ) -> std::io::Result<Arc<HttpServer>> {
        let listener = TcpListener::bind((interface_addr, port)).await?;
        Ok(Arc::new(HttpServer {
            listener,
            friendly_name,
            uuid,
            interface_addr,
            port,
            content_source: Arc::new(content_source),
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

    async fn handle(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
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
            Route::NotFound => not_found(),
        })
    }

    async fn handle_control(
        &self,
        service: crate::core::device::ServiceType,
        req: Request<Incoming>,
    ) -> Response<Full<Bytes>> {
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
}

fn content_length(req: &Request<Incoming>) -> Option<usize> {
    req.headers()
        .get(CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

fn xml_response(body: String) -> Response<Full<Bytes>> {
    response(StatusCode::OK, body)
}

fn soap_ok_response(body: String) -> Response<Full<Bytes>> {
    response(StatusCode::OK, body)
}

// UPnP convention: a SOAP fault is carried in the body of an HTTP 500
// response, not a 200 - the body's <s:Fault> element is what actually
// describes the error.
fn soap_fault_response(body: String) -> Response<Full<Bytes>> {
    response(StatusCode::INTERNAL_SERVER_ERROR, body)
}

fn response(status: StatusCode, body: String) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .body(Full::new(Bytes::from(body)))
        .expect("static header name/value are always valid")
}

fn not_found() -> Response<Full<Bytes>> {
    empty_response(StatusCode::NOT_FOUND)
}

fn bad_request() -> Response<Full<Bytes>> {
    empty_response(StatusCode::BAD_REQUEST)
}

fn payload_too_large() -> Response<Full<Bytes>> {
    empty_response(StatusCode::PAYLOAD_TOO_LARGE)
}

fn empty_response(status: StatusCode) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::new()))
        .expect("static header name/value are always valid")
}
