//! Routes a parsed SOAP action against a `ContentSource`, turning the
//! result into a complete SOAP response (or fault) envelope. This is the
//! "ContentDirectory action mechanics" layer: it decides *what* goes in
//! the response (calling into `ContentSource`, building DIDL-Lite); how
//! that gets wrapped as SOAP XML is `core::soap`'s job, kept separate so
//! this module reads as business logic, not string templating.

use crate::core::content_source::ContentSource;
use crate::core::device::ServiceType;
use crate::core::{didl, soap};
use crate::index::{Entry, ObjectId};

const CONTENT_DIRECTORY_URN: &str = "urn:schemas-upnp-org:service:ContentDirectory:1";
const CONNECTION_MANAGER_URN: &str = "urn:schemas-upnp-org:service:ConnectionManager:1";

/// A UPnP standard error code and description. See the UPnP Device
/// Architecture / ContentDirectory:1 spec for the numeric codes; these
/// are the ones this dispatcher can actually produce.
#[derive(Debug)]
pub enum DispatchError {
    InvalidAction,
    InvalidArgs,
    NoSuchObject,
}

impl DispatchError {
    fn code(&self) -> u32 {
        match self {
            DispatchError::InvalidAction => 401,
            DispatchError::InvalidArgs => 402,
            DispatchError::NoSuchObject => 701,
        }
    }

    fn description(&self) -> &'static str {
        match self {
            DispatchError::InvalidAction => "Invalid Action",
            DispatchError::InvalidArgs => "Invalid Args",
            DispatchError::NoSuchObject => "No Such Object",
        }
    }

    pub fn into_fault(self) -> String {
        soap::build_fault(self.code(), self.description())
    }
}

/// Handles one parsed action for `service`. `Ok` is a complete, ready-to-
/// send SOAP response envelope; `Err` describes what fault to send
/// instead (the HTTP layer decides the status code — see `core::http`).
pub fn handle(
    service: ServiceType,
    action: &soap::SoapAction,
    source: &dyn ContentSource,
    base_url: &str,
) -> Result<String, DispatchError> {
    match service {
        ServiceType::ContentDirectory => content_directory_action(action, source, base_url),
        ServiceType::ConnectionManager => connection_manager_action(action),
    }
}

fn content_directory_action(
    action: &soap::SoapAction,
    source: &dyn ContentSource,
    base_url: &str,
) -> Result<String, DispatchError> {
    match action.name.as_str() {
        "Browse" => browse(action, source, base_url),
        "GetSearchCapabilities" => Ok(soap::build_response(
            CONTENT_DIRECTORY_URN,
            "GetSearchCapabilities",
            &[("SearchCaps", String::new())],
        )),
        "GetSortCapabilities" => Ok(soap::build_response(
            CONTENT_DIRECTORY_URN,
            "GetSortCapabilities",
            &[("SortCaps", String::new())],
        )),
        "GetSystemUpdateID" => Ok(soap::build_response(
            CONTENT_DIRECTORY_URN,
            "GetSystemUpdateID",
            &[("Id", "1".to_string())],
        )),
        // Search, CreateObject, etc.: real, spec-known actions this
        // server doesn't implement. A proper fault, not silence or a
        // 404 - see docs/PLAN.md Phase 5 / the spec's non-goals.
        _ => Err(DispatchError::InvalidAction),
    }
}

fn browse(
    action: &soap::SoapAction,
    source: &dyn ContentSource,
    base_url: &str,
) -> Result<String, DispatchError> {
    let object_id = ObjectId::new(arg(action, "ObjectID")?);
    let browse_flag = arg(action, "BrowseFlag")?;

    let (entries, total_matches) = match browse_flag {
        "BrowseDirectChildren" => {
            let children = source
                .children(&object_id)
                .ok_or(DispatchError::NoSuchObject)?;
            let total = children.len();
            (paginate(children, action), total)
        }
        "BrowseMetadata" => {
            let entry = source
                .entry(&object_id)
                .ok_or(DispatchError::NoSuchObject)?;
            (vec![entry], 1)
        }
        _ => return Err(DispatchError::InvalidArgs),
    };

    let number_returned = entries.len();
    let result = didl::render(&entries, base_url);

    Ok(soap::build_response(
        CONTENT_DIRECTORY_URN,
        "Browse",
        &[
            ("Result", result),
            ("NumberReturned", number_returned.to_string()),
            ("TotalMatches", total_matches.to_string()),
            ("UpdateID", "1".to_string()),
        ],
    ))
}

/// `StartingIndex`/`RequestedCount` slicing for `BrowseDirectChildren`.
/// `RequestedCount = 0` means "no limit," per the UPnP spec. Sorting
/// (`SortCriteria`) isn't implemented — `GetSortCapabilities` correctly
/// advertises no capabilities, so a compliant client won't rely on it.
fn paginate(mut children: Vec<Entry>, action: &soap::SoapAction) -> Vec<Entry> {
    let starting_index: usize = action
        .arguments
        .get("StartingIndex")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let requested_count: usize = action
        .arguments
        .get("RequestedCount")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    if starting_index >= children.len() {
        return Vec::new();
    }
    children = children.split_off(starting_index);
    if requested_count > 0 && requested_count < children.len() {
        children.truncate(requested_count);
    }
    children
}

fn connection_manager_action(action: &soap::SoapAction) -> Result<String, DispatchError> {
    match action.name.as_str() {
        "GetProtocolInfo" => Ok(soap::build_response(
            CONNECTION_MANAGER_URN,
            "GetProtocolInfo",
            // A blanket wildcard rather than enumerating every specific
            // protocolInfo combination: real DLNA/UPnP clients use this
            // mainly as a coarse compatibility probe, not a strict
            // allowlist - each <res> in a Browse response already
            // carries its own exact protocolInfo (core::didl), which is
            // what actually matters for playback. MiniDLNA does the
            // same. Sink is empty: this is a MediaServer, not a
            // MediaRenderer (explicit non-goal).
            &[
                ("Source", "http-get:*:*:*".to_string()),
                ("Sink", String::new()),
            ],
        )),
        "GetCurrentConnectionIDs" => Ok(soap::build_response(
            CONNECTION_MANAGER_URN,
            "GetCurrentConnectionIDs",
            // HTTP GET/Range serving is stateless - there's no real
            // connection to enumerate. "0" is the conventional
            // always-present placeholder ID minimal servers use here.
            &[("ConnectionIDs", "0".to_string())],
        )),
        "GetCurrentConnectionInfo" => {
            let connection_id = arg(action, "ConnectionID")?;
            if connection_id != "0" {
                return Err(DispatchError::InvalidArgs);
            }
            Ok(soap::build_response(
                CONNECTION_MANAGER_URN,
                "GetCurrentConnectionInfo",
                &[
                    ("RcsID", "-1".to_string()),
                    ("AVTransportID", "-1".to_string()),
                    ("ProtocolInfo", String::new()),
                    ("PeerConnectionManager", String::new()),
                    ("PeerConnectionID", "-1".to_string()),
                    ("Direction", "Output".to_string()),
                    ("Status", "OK".to_string()),
                ],
            ))
        }
        _ => Err(DispatchError::InvalidAction),
    }
}

fn arg<'a>(action: &'a soap::SoapAction, name: &str) -> Result<&'a str, DispatchError> {
    action
        .arguments
        .get(name)
        .map(String::as_str)
        .ok_or(DispatchError::InvalidArgs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::folder::FolderMirror;
    use crate::index::IndexBuilder;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn action(name: &str, args: &[(&str, &str)]) -> soap::SoapAction {
        soap::SoapAction {
            name: name.to_string(),
            arguments: args
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn fixture_source() -> FolderMirror {
        let mut builder = IndexBuilder::new();
        let album = builder.add_container(&ObjectId::root(), "Album".to_string());
        builder.add_item(
            &album,
            "01.mp3".to_string(),
            PathBuf::from("/a/01.mp3"),
            100,
            SystemTime::UNIX_EPOCH,
        );
        builder.add_item(
            &album,
            "02.mp3".to_string(),
            PathBuf::from("/a/02.mp3"),
            200,
            SystemTime::UNIX_EPOCH,
        );
        FolderMirror::new(builder.build())
    }

    #[test]
    fn browse_direct_children_of_root() {
        let source = fixture_source();
        let action = action(
            "Browse",
            &[("ObjectID", "0"), ("BrowseFlag", "BrowseDirectChildren")],
        );
        let response = handle(
            ServiceType::ContentDirectory,
            &action,
            &source,
            "http://h:1",
        )
        .unwrap();
        // The DIDL-Lite <Result> is XML-escaped one level inside the SOAP
        // response (per UPnP convention - it's a string-typed argument
        // whose value happens to be XML), so the raw tag won't appear.
        assert!(response.contains("Album"));
        assert!(response.contains("&lt;dc:title&gt;"));
        assert!(response.contains("<NumberReturned>1</NumberReturned>"));
        assert!(response.contains("<TotalMatches>1</TotalMatches>"));
    }

    #[test]
    fn browse_metadata_of_an_item() {
        let source = fixture_source();
        let root_children = source.children(&ObjectId::root()).unwrap();
        let album_id = root_children[0].id().clone();
        let children = source.children(&album_id).unwrap();
        let item_id = children[0].id().clone();

        let action = action(
            "Browse",
            &[
                (("ObjectID"), item_id.as_str()),
                ("BrowseFlag", "BrowseMetadata"),
            ],
        );
        let response = handle(
            ServiceType::ContentDirectory,
            &action,
            &source,
            "http://h:1",
        )
        .unwrap();
        assert!(response.contains("<NumberReturned>1</NumberReturned>"));
        assert!(response.contains("audioItem.musicTrack"));
    }

    #[test]
    fn browse_unknown_object_id_is_no_such_object() {
        let source = fixture_source();
        let action = action(
            "Browse",
            &[("ObjectID", "9999"), ("BrowseFlag", "BrowseDirectChildren")],
        );
        let err = handle(
            ServiceType::ContentDirectory,
            &action,
            &source,
            "http://h:1",
        )
        .unwrap_err();
        assert_eq!(err.code(), 701);
    }

    #[test]
    fn browse_missing_required_argument_is_invalid_args() {
        let source = fixture_source();
        let action = action("Browse", &[("BrowseFlag", "BrowseDirectChildren")]);
        let err = handle(
            ServiceType::ContentDirectory,
            &action,
            &source,
            "http://h:1",
        )
        .unwrap_err();
        assert_eq!(err.code(), 402);
    }

    #[test]
    fn browse_pagination_honors_starting_index_and_requested_count() {
        let source = fixture_source();
        let root_children = source.children(&ObjectId::root()).unwrap();
        let album_id = root_children[0].id().clone();

        let action = action(
            "Browse",
            &[
                ("ObjectID", album_id.as_str()),
                ("BrowseFlag", "BrowseDirectChildren"),
                ("StartingIndex", "1"),
                ("RequestedCount", "1"),
            ],
        );
        let response = handle(
            ServiceType::ContentDirectory,
            &action,
            &source,
            "http://h:1",
        )
        .unwrap();
        assert!(response.contains("<NumberReturned>1</NumberReturned>"));
        assert!(response.contains("<TotalMatches>2</TotalMatches>"));
        assert!(response.contains("02.mp3"));
        assert!(!response.contains("01.mp3"));
    }

    #[test]
    fn unimplemented_action_is_invalid_action_fault() {
        let source = fixture_source();
        let action = action("Search", &[]);
        let err = handle(
            ServiceType::ContentDirectory,
            &action,
            &source,
            "http://h:1",
        )
        .unwrap_err();
        assert_eq!(err.code(), 401);
    }

    #[test]
    fn get_search_capabilities_advertises_none() {
        let source = fixture_source();
        let action = action("GetSearchCapabilities", &[]);
        let response = handle(
            ServiceType::ContentDirectory,
            &action,
            &source,
            "http://h:1",
        )
        .unwrap();
        assert!(response.contains("<SearchCaps></SearchCaps>"));
    }

    #[test]
    fn connection_manager_get_protocol_info() {
        let source = fixture_source();
        let action = action("GetProtocolInfo", &[]);
        let response = handle(
            ServiceType::ConnectionManager,
            &action,
            &source,
            "http://h:1",
        )
        .unwrap();
        assert!(response.contains("<Source>http-get:*:*:*</Source>"));
        assert!(response.contains("<Sink></Sink>"));
    }

    #[test]
    fn connection_manager_current_connection_info_for_unknown_id_faults() {
        let source = fixture_source();
        let action = action("GetCurrentConnectionInfo", &[("ConnectionID", "42")]);
        let err = handle(
            ServiceType::ConnectionManager,
            &action,
            &source,
            "http://h:1",
        )
        .unwrap_err();
        assert_eq!(err.code(), 402);
    }
}
