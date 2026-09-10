//! The `ArtSource` extension point: given a resolved item path, produce
//! its embedded cover art, if it has any. Kept separate from
//! `ByteSource` on purpose — that trait serves a whole audio file
//! byte-for-byte, and forcing `transform::passthrough::PassthroughSource`
//! to also know how to pull a picture out of a tag block would conflate
//! two different jobs into one trait for no reason.
//!
//! Reads fresh from disk on every call, the same as `ByteSource` — no
//! picture bytes are cached in the `Index`. A library can hold thousands
//! of embedded covers; keeping the index to a `has_art: bool` and
//! re-reading the actual bytes only when a client asks for them is what
//! keeps memory use flat regardless of library size.
//!
//! Hand-rolled `Pin<Box<dyn Future>>`, not the `async-trait` crate —
//! same reasoning as `ByteSource`.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use bytes::Bytes;

/// An embedded picture's bytes and its MIME type.
pub struct Art {
    pub bytes: Bytes,
    pub mime: &'static str,
}

pub trait ArtSource: Send + Sync {
    /// The embedded picture, or `None` if the file has no picture or
    /// reading it failed. Fails closed, like every other "unknown
    /// thing" lookup in this project — the HTTP layer maps `None` to
    /// 404, never a panic or a guess.
    fn art<'a>(&'a self, path: &'a Path) -> Pin<Box<dyn Future<Output = Option<Art>> + Send + 'a>>;
}
