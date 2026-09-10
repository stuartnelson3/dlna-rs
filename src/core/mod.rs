//! Protocol mechanics: the parts of a DLNA/UPnP-AV server that are the
//! same no matter how the media library is organized or whether bytes get
//! transformed before serving. See docs/DESIGN.md for the full boundary.

pub mod net;
pub mod ssdp;
