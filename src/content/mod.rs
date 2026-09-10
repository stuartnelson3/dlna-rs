//! `ContentSource` implementations. The trait itself is defined in
//! `core::content_source` (not here — see that module's doc comment for
//! why); this module holds the concrete implementations that depend on
//! it, per the spec's stated dependency direction.

pub mod folder;
