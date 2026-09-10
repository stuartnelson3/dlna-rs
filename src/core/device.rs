//! The device model SSDP and HTTP both need to agree on: what services
//! this device implements, and the well-known URL each one is served
//! under. Kept as one shared definition so SSDP's advertised service list
//! (`core::ssdp::targets`) and HTTP's SCPD/control routing (`core::http`)
//! can't drift apart by each keeping their own copy of "what services
//! exist."

pub const DEVICE_TYPE: &str = "urn:schemas-upnp-org:device:MediaServer:1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceType {
    ContentDirectory,
    ConnectionManager,
}

impl ServiceType {
    pub const ALL: [ServiceType; 2] = [
        ServiceType::ContentDirectory,
        ServiceType::ConnectionManager,
    ];

    pub fn urn(self) -> &'static str {
        match self {
            ServiceType::ContentDirectory => "urn:schemas-upnp-org:service:ContentDirectory:1",
            ServiceType::ConnectionManager => "urn:schemas-upnp-org:service:ConnectionManager:1",
        }
    }

    /// Per UPnP convention: `urn:upnp-org:serviceId:<name>`.
    pub fn service_id(self) -> String {
        format!("urn:upnp-org:serviceId:{}", self.path_segment())
    }

    /// Where this service's SCPD document is served.
    pub fn scpd_path(self) -> String {
        format!("/{}/scpd.xml", self.path_segment())
    }

    /// Where this service's SOAP control endpoint is served (Phase 5).
    pub fn control_path(self) -> String {
        format!("/{}/control", self.path_segment())
    }

    /// Where this service's GENA event subscription endpoint would be.
    /// Not implemented for MVP — no eventing support (see
    /// docs/THREAT_MODEL.md and docs/PLAN.md Phase 3) — a `SUBSCRIBE`
    /// here just falls through the router to a 404, which UPnP permits
    /// for a service that doesn't support eventing.
    pub fn event_path(self) -> String {
        format!("/{}/event", self.path_segment())
    }

    fn path_segment(self) -> &'static str {
        match self {
            ServiceType::ContentDirectory => "ContentDirectory",
            ServiceType::ConnectionManager => "ConnectionManager",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_distinct_per_service() {
        for service in ServiceType::ALL {
            assert_ne!(service.scpd_path(), service.control_path());
            assert_ne!(service.scpd_path(), service.event_path());
        }
        assert_ne!(
            ServiceType::ContentDirectory.scpd_path(),
            ServiceType::ConnectionManager.scpd_path()
        );
    }
}
