//! Passively listens to SSDP multicast traffic on the LAN — NOTIFY
//! ssdp:alive/ssdp:byebye, M-SEARCH requests, anything sent to
//! 239.255.255.250:1900. Useful for watching dlna-rs's own announce/byebye
//! cycle (start it, run this, then send dlna-rs SIGTERM and watch for
//! ssdp:byebye) or just seeing what else is announcing itself.
//! `SO_REUSEADDR`/`SO_REUSEPORT` so it can run alongside dlna-rs's own
//! socket on the same host.
//!
//! Usage: `cargo run --example ssdp_monitor`, then Ctrl-C to stop.

use std::net::{Ipv4Addr, SocketAddr};

use socket2::{Domain, Protocol, Socket, Type};

const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const PORT: u16 = 1900;

fn main() -> std::io::Result<()> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT)).into())?;
    socket.join_multicast_v4(&MULTICAST_ADDR, &Ipv4Addr::UNSPECIFIED)?;
    let socket: std::net::UdpSocket = socket.into();

    println!("Listening on {MULTICAST_ADDR}:{PORT} (Ctrl-C to stop)...\n");

    let mut buf = [0u8; 4096];
    loop {
        let (len, src) = socket.recv_from(&mut buf)?;
        println!("--- from {src} ---");
        println!("{}", String::from_utf8_lossy(&buf[..len]));
    }
}
