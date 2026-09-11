//! `/description.xml`: the root UPnP device description. Unlike SCPD,
//! this varies with config (friendly name, device UUID) and with where
//! the server is actually reachable (interface IP, port), so it's a pure
//! builder function rather than a static asset — the same pattern as
//! `core::ssdp::message`'s builders.

use std::net::Ipv4Addr;

use quick_xml::escape::escape;
use uuid::Uuid;

use crate::core::device::{DEVICE_TYPE, ServiceType};

const MANUFACTURER: &str = "dlna-rs";
const MANUFACTURER_URL: &str = "https://github.com/stuartnelson3/dlna-rs";
const MODEL_NAME: &str = "dlna-rs";
const MODEL_DESCRIPTION: &str = "dlna-rs media server";

pub struct DeviceInfo<'a> {
    pub friendly_name: &'a str,
    pub uuid: Uuid,
    pub interface_addr: Ipv4Addr,
    pub port: u16,
}

pub fn build(info: &DeviceInfo) -> String {
    let base = format!("http://{}:{}", info.interface_addr, info.port);
    let services: String = ServiceType::ALL.into_iter().map(service_xml).collect();

    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <specVersion>
    <major>1</major>
    <minor>0</minor>
  </specVersion>
  <URLBase>{base}</URLBase>
  <device>
    <deviceType>{DEVICE_TYPE}</deviceType>
    <friendlyName>{friendly_name}</friendlyName>
    <manufacturer>{MANUFACTURER}</manufacturer>
    <manufacturerURL>{MANUFACTURER_URL}</manufacturerURL>
    <modelDescription>{MODEL_DESCRIPTION}</modelDescription>
    <modelName>{MODEL_NAME}</modelName>
    <modelNumber>{model_number}</modelNumber>
    <UDN>uuid:{uuid}</UDN>
    <serviceList>
{services}    </serviceList>
  </device>
</root>
"#,
        friendly_name = escape(info.friendly_name),
        model_number = env!("CARGO_PKG_VERSION"),
        uuid = info.uuid,
    )
}

fn service_xml(service: ServiceType) -> String {
    format!(
        r#"      <service>
        <serviceType>{urn}</serviceType>
        <serviceId>{id}</serviceId>
        <SCPDURL>{scpd}</SCPDURL>
        <controlURL>{control}</controlURL>
        <eventSubURL>{event}</eventSubURL>
      </service>
"#,
        urn = service.urn(),
        id = service.service_id(),
        scpd = service.scpd_path(),
        control = service.control_path(),
        event = service.event_path(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::assert_well_formed;

    fn sample_info(friendly_name: &str) -> DeviceInfo<'_> {
        DeviceInfo {
            friendly_name,
            uuid: Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap(),
            interface_addr: Ipv4Addr::new(192, 168, 1, 5),
            port: 8200,
        }
    }

    #[test]
    fn produces_well_formed_xml() {
        assert_well_formed(&build(&sample_info("my-server")));
    }

    #[test]
    fn includes_configured_identity_and_base_url() {
        let xml = build(&sample_info("my-server"));
        assert!(xml.contains("<friendlyName>my-server</friendlyName>"));
        assert!(xml.contains("<UDN>uuid:11111111-2222-3333-4444-555555555555</UDN>"));
        assert!(xml.contains("<URLBase>http://192.168.1.5:8200</URLBase>"));
        assert!(xml.contains(&format!("<deviceType>{DEVICE_TYPE}</deviceType>")));
    }

    #[test]
    fn escapes_special_characters_in_friendly_name() {
        let xml = build(&sample_info("Bob & Alice's <Music>"));
        assert_well_formed(&xml);
        assert!(!xml.contains("<Music>"), "raw '<' should have been escaped");
    }

    #[test]
    fn lists_both_services_with_distinct_urls() {
        let xml = build(&sample_info("my-server"));
        for service in ServiceType::ALL {
            assert!(xml.contains(service.urn()));
            assert!(xml.contains(&service.scpd_path()));
            assert!(xml.contains(&service.control_path()));
            assert!(xml.contains(&service.event_path()));
        }
    }
}
