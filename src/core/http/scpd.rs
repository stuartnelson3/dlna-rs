//! SCPD (service description) documents. Per the spec, these don't vary
//! at runtime — they describe what actions a service has, not any live
//! state — so they're static XML embedded at compile time, not generated.
//!
//! Declares exactly the actions `dispatch` (Phase 5) implements for each
//! service, not the full optional action set UPnP allows. That matches
//! real minimal DLNA servers (MiniDLNA does the same): SCPD should be an
//! honest description of what this device can do, not a checklist of
//! everything the spec permits.

use crate::core::device::ServiceType;

const CONTENT_DIRECTORY: &str = include_str!("scpd/content_directory.xml");
const CONNECTION_MANAGER: &str = include_str!("scpd/connection_manager.xml");

pub fn document(service: ServiceType) -> &'static str {
    match service {
        ServiceType::ContentDirectory => CONTENT_DIRECTORY,
        ServiceType::ConnectionManager => CONNECTION_MANAGER,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::assert_well_formed;

    #[test]
    fn every_service_scpd_is_well_formed() {
        for service in ServiceType::ALL {
            assert_well_formed(document(service));
        }
    }

    #[test]
    fn content_directory_declares_implemented_actions() {
        let xml = document(ServiceType::ContentDirectory);
        for action in [
            "Browse",
            "GetSearchCapabilities",
            "GetSortCapabilities",
            "GetSystemUpdateID",
        ] {
            assert!(
                xml.contains(&format!("<name>{action}</name>")),
                "missing action {action}"
            );
        }
    }

    #[test]
    fn connection_manager_declares_implemented_actions() {
        let xml = document(ServiceType::ConnectionManager);
        for action in [
            "GetProtocolInfo",
            "GetCurrentConnectionIDs",
            "GetCurrentConnectionInfo",
        ] {
            assert!(
                xml.contains(&format!("<name>{action}</name>")),
                "missing action {action}"
            );
        }
    }
}
