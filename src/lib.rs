#![forbid(unsafe_code)]

pub mod config;
pub mod core;

/// The seam `fuzz/` (a separate crate — see `fuzz/Cargo.toml`) reaches
/// through to fuzz pure parsing functions. Deliberately narrow: re-export
/// exactly the functions each fuzz target needs, nothing else. A fuzz
/// target needing a new function is not a reason to make its containing
/// module `pub` — add one line here instead. See docs/DESIGN.md's
/// encapsulation guidance for why this exists.
#[doc(hidden)]
pub mod fuzz_support {
    pub use crate::core::ssdp::message::parse_search_request;
}
