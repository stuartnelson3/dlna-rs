//! The fixed set of UPnP identities this server advertises.
//!
//! Per the UPnP Device Architecture, a root device with embedded services
//! (and no embedded devices, which is all we have) advertises itself under
//! exactly these identities: the root device itself, its UUID, its device
//! type, and each service type it implements. `ssdp:all` means "all of
//! them at once" — that's the only thing special-cased here, everything
//! else is an exact (case-insensitive) match against one target's type
//! string.

use uuid::Uuid;

use crate::core::device::{DEVICE_TYPE, ServiceType};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    RootDevice,
    Uuid,
    DeviceType,
    Service(ServiceType),
}

impl Target {
    pub const ALL: [Target; 5] = [
        Target::RootDevice,
        Target::Uuid,
        Target::DeviceType,
        Target::Service(ServiceType::ContentDirectory),
        Target::Service(ServiceType::ConnectionManager),
    ];

    /// The `NT` (NOTIFY) / `ST` (search response) header value identifying
    /// this target.
    pub fn type_string(self, uuid: &Uuid) -> String {
        match self {
            Target::RootDevice => "upnp:rootdevice".to_string(),
            Target::Uuid => format!("uuid:{uuid}"),
            Target::DeviceType => DEVICE_TYPE.to_string(),
            Target::Service(service) => service.urn().to_string(),
        }
    }

    /// The `USN` header value: the type string, prefixed with the device's
    /// UUID — except for the UUID target itself, which *is* just the UUID.
    pub fn usn(self, uuid: &Uuid) -> String {
        match self {
            Target::Uuid => format!("uuid:{uuid}"),
            _ => format!("uuid:{uuid}::{}", self.type_string(uuid)),
        }
    }
}

/// Which of our advertised targets a search string (the `ST` header on an
/// incoming M-SEARCH) matches.
pub fn matching(search_target: &str, uuid: &Uuid) -> Vec<Target> {
    if search_target.eq_ignore_ascii_case("ssdp:all") {
        return Target::ALL.to_vec();
    }
    Target::ALL
        .into_iter()
        .filter(|target| target.type_string(uuid).eq_ignore_ascii_case(search_target))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid() -> Uuid {
        Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap()
    }

    #[test]
    fn ssdp_all_matches_every_target() {
        assert_eq!(matching("ssdp:all", &uuid()).len(), Target::ALL.len());
        assert_eq!(matching("SSDP:ALL", &uuid()).len(), Target::ALL.len());
    }

    #[test]
    fn matches_root_device_case_insensitively() {
        assert_eq!(
            matching("UPNP:ROOTDEVICE", &uuid()),
            vec![Target::RootDevice]
        );
    }

    #[test]
    fn matches_device_type() {
        assert_eq!(
            matching("urn:schemas-upnp-org:device:MediaServer:1", &uuid()),
            vec![Target::DeviceType]
        );
    }

    #[test]
    fn matches_content_directory_service() {
        assert_eq!(
            matching("urn:schemas-upnp-org:service:ContentDirectory:1", &uuid()),
            vec![Target::Service(ServiceType::ContentDirectory)]
        );
    }

    #[test]
    fn matches_own_uuid() {
        let id = uuid();
        assert_eq!(matching(&format!("uuid:{id}"), &id), vec![Target::Uuid]);
    }

    #[test]
    fn unrelated_search_target_matches_nothing() {
        assert!(matching("urn:schemas-upnp-org:device:Printer:1", &uuid()).is_empty());
    }

    #[test]
    fn usn_for_root_device_includes_uuid() {
        let id = uuid();
        assert_eq!(
            Target::RootDevice.usn(&id),
            format!("uuid:{id}::upnp:rootdevice")
        );
    }

    #[test]
    fn usn_for_uuid_target_is_not_doubled() {
        let id = uuid();
        assert_eq!(Target::Uuid.usn(&id), format!("uuid:{id}"));
    }
}
