//! Small host-networking lookups that protocol code needs but that aren't
//! themselves part of any protocol: right now, just "what's our IPv4
//! address on this interface." SSDP needs it to join the right multicast
//! interface; both SSDP and the device description (Phase 3) need it to
//! build a LOCATION URL clients can actually reach.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr};

#[derive(Debug)]
pub struct InterfaceNotFound {
    pub name: String,
}

impl fmt::Display for InterfaceNotFound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "no IPv4 address found for interface {:?} (check server.interface in your config)",
            self.name
        )
    }
}

impl std::error::Error for InterfaceNotFound {}

/// The IPv4 address assigned to the named network interface (e.g. `"eth0"`).
pub fn interface_ipv4(name: &str) -> Result<Ipv4Addr, InterfaceNotFound> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .find(|iface| iface.name == name)
        .and_then(|iface| match iface.ip() {
            IpAddr::V4(ip) => Some(ip),
            IpAddr::V6(_) => None,
        })
        .ok_or_else(|| InterfaceNotFound {
            name: name.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_has_an_ipv4_address() {
        assert_eq!(interface_ipv4("lo").unwrap(), Ipv4Addr::LOCALHOST);
    }

    #[test]
    fn unknown_interface_errors() {
        assert!(interface_ipv4("definitely-not-a-real-interface-xyz").is_err());
    }
}
