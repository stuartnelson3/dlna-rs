//! The HTTP server: accept-loop plumbing around [`router`], [`description`],
//! and [`scpd`]. Range parsing/serving (Phase 6) and SOAP dispatch
//! (Phase 5) extend this, not replace it.

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
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use uuid::Uuid;

use self::description::DeviceInfo;
use self::router::Route;

pub struct HttpServer {
    listener: TcpListener,
    friendly_name: String,
    uuid: Uuid,
    interface_addr: Ipv4Addr,
    port: u16,
}

impl HttpServer {
    pub async fn bind(
        interface_addr: Ipv4Addr,
        port: u16,
        friendly_name: String,
        uuid: Uuid,
    ) -> std::io::Result<Arc<HttpServer>> {
        let listener = TcpListener::bind((interface_addr, port)).await?;
        Ok(Arc::new(HttpServer {
            listener,
            friendly_name,
            uuid,
            interface_addr,
            port,
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
            Route::NotFound => not_found(),
        })
    }
}

fn xml_response(body: String) -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .body(Full::new(Bytes::from(body)))
        .expect("static header name/value are always valid")
}

fn not_found() -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Full::new(Bytes::new()))
        .expect("static header name/value are always valid")
}
