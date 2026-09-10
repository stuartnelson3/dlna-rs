//! Manual SSDP acceptance check: sends an M-SEARCH for `ssdp:all` and
//! prints whatever responds within a few seconds — dlna-rs itself and any
//! other UPnP devices on the LAN. This is the "hand-rolled SSDP client"
//! docs/PLAN.md's testing strategy calls for; real LAN multicast isn't
//! something CI can rely on, so it stays a manual tool rather than an
//! automated test (see docs/PLAN.md, Phases 2 and 11).
//!
//! Usage: `cargo run --example ssdp_discover` while dlna-rs is running on
//! the same network.

use std::io::ErrorKind;
use std::net::UdpSocket;
use std::time::Duration;

const REQUEST: &str = "M-SEARCH * HTTP/1.1\r\n\
                        HOST: 239.255.255.250:1900\r\n\
                        MAN: \"ssdp:discover\"\r\n\
                        MX: 2\r\n\
                        ST: ssdp:all\r\n\
                        \r\n";

fn main() -> std::io::Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_read_timeout(Some(Duration::from_secs(3)))?;
    socket.send_to(REQUEST.as_bytes(), "239.255.255.250:1900")?;
    println!("Sent M-SEARCH for ssdp:all, listening for 3s...\n");

    let mut buf = [0u8; 4096];
    let mut count = 0;
    loop {
        match socket.recv_from(&mut buf) {
            Ok((len, src)) => {
                count += 1;
                println!("--- response #{count} from {src} ---");
                println!("{}", String::from_utf8_lossy(&buf[..len]));
            }
            Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => break,
            Err(err) => return Err(err),
        }
    }

    println!("done: {count} response(s)");
    Ok(())
}
