//! Protocol mechanics: the parts of a DLNA/UPnP-AV server that are the
//! same no matter how the media library is organized or whether bytes get
//! transformed before serving. See docs/DESIGN.md for the full boundary.

// Only http/net/ssdp are used from outside `core` (main.rs, tests/,
// fuzz/); everything else here is consumed internally within `core`
// itself and stays `pub(crate)` — see docs/DESIGN.md's encapsulation
// guidance. `fuzz_support` in lib.rs is the deliberate, narrow exception
// for the fuzz targets that need a `pub(crate)` module's function anyway.
pub(crate) mod art_source;
pub(crate) mod byte_source;
pub(crate) mod content_source;
pub(crate) mod device;
pub(crate) mod didl;
pub(crate) mod dispatch;
pub mod http;
pub(crate) mod metadata_provider;
pub mod net;
pub(crate) mod soap;
pub mod ssdp;
