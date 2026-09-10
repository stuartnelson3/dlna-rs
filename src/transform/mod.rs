//! `ByteSource` implementations. The trait itself is defined in
//! `core::byte_source` (not here — same reasoning as `content`'s doc
//! comment): implementations depend on `core`, never the reverse.

pub mod passthrough;
