//! SSDP discovery: the UDP multicast responder and announcer. Parsing and
//! message building are pure functions in [`message`]; this module is just
//! the socket plumbing around them.

pub mod message;
pub mod targets;

pub use targets::Target;

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use uuid::Uuid;

const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const PORT: u16 = 1900;

pub struct Ssdp {
    socket: UdpSocket,
    uuid: Uuid,
    location: String,
    max_age: u32,
}

impl Ssdp {
    /// Binds the SSDP socket and joins the multicast group on the given
    /// interface. `location` is the full URL clients should fetch the
    /// device description from; `max_age` is the `CACHE-CONTROL` lifetime
    /// (seconds) attached to every advertisement.
    ///
    /// Binds with `SO_REUSEADDR`/`SO_REUSEPORT`: port 1900 is a shared
    /// well-known multicast port, and other UPnP software (a router's IGD
    /// responder, a monitoring tool, another instance during a restart)
    /// reasonably expects to bind it too.
    pub async fn bind(
        interface_addr: Ipv4Addr,
        uuid: Uuid,
        location: String,
        max_age: u32,
    ) -> std::io::Result<Arc<Ssdp>> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        #[cfg(unix)]
        socket.set_reuse_port(true)?;
        socket.set_nonblocking(true)?;
        socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT)).into())?;
        socket.set_multicast_if_v4(&interface_addr)?;
        socket.join_multicast_v4(&MULTICAST_ADDR, &interface_addr)?;

        let socket = UdpSocket::from_std(socket.into())?;
        Ok(Arc::new(Ssdp {
            socket,
            uuid,
            location,
            max_age,
        }))
    }

    /// Answers incoming M-SEARCH requests. Runs until the socket errors.
    pub async fn serve_search_requests(&self) {
        let mut buf = [0u8; 2048];
        loop {
            let (len, src) = match self.socket.recv_from(&mut buf).await {
                Ok(received) => received,
                Err(err) => {
                    log::warn!("SSDP socket recv error: {err}");
                    continue;
                }
            };
            let Ok(request) = message::parse_search_request(&buf[..len]) else {
                continue; // malformed or irrelevant datagram - ignore, don't log-spam
            };
            for target in targets::matching(&request.search_target, &self.uuid) {
                let ad = message::Advertisement {
                    target,
                    location: self.location.clone(),
                    max_age: self.max_age,
                };
                let response = message::build_search_response(&ad, &self.uuid);
                if let Err(err) = self.socket.send_to(response.as_bytes(), src).await {
                    log::warn!("failed to send SSDP search response to {src}: {err}");
                }
            }
        }
    }

    /// Re-sends NOTIFY ssdp:alive every `interval`, forever. The caller is
    /// expected to have already sent one round via [`Ssdp::announce_alive`]
    /// at startup — this only handles the *repeat*, so the two don't race
    /// to send the first announcement twice.
    pub async fn announce_alive_periodically(&self, interval: Duration) {
        loop {
            tokio::time::sleep(interval).await;
            self.announce_alive().await;
        }
    }

    pub async fn announce_alive(&self) {
        for target in Target::ALL {
            let ad = message::Advertisement {
                target,
                location: self.location.clone(),
                max_age: self.max_age,
            };
            let message = message::build_notify_alive(&ad, &self.uuid);
            self.send_multicast(&message).await;
        }
    }

    /// Sends NOTIFY ssdp:byebye for every advertised target. Call once on
    /// shutdown.
    pub async fn announce_byebye(&self) {
        for target in Target::ALL {
            let message = message::build_notify_byebye(target, &self.uuid);
            self.send_multicast(&message).await;
        }
    }

    async fn send_multicast(&self, message: &str) {
        let addr = SocketAddr::V4(SocketAddrV4::new(MULTICAST_ADDR, PORT));
        if let Err(err) = self.socket.send_to(message.as_bytes(), addr).await {
            log::warn!("failed to send SSDP multicast message: {err}");
        }
    }
}
