//! The `ByteSource` extension point: given a resolved item path, produce
//! its bytes (or a byte range of them). Default: `transform::passthrough::PassthroughSource`
//! — opens the file, serves a range as a direct byte-offset seek, zero
//! transformation, matching the no-transcoding non-goal. This is where a
//! downstream fork would plug in transcoding, wiring in a different
//! `ByteSource` instead of `PassthroughSource` without touching `core`.
//!
//! Hand-rolled `Pin<Box<dyn Future>>` instead of the `async-trait` crate:
//! this is a two-method trait, boxing it by hand at the one call site
//! (`PassthroughSource::read`) is a few extra lines, not worth a
//! dependency whose entire job is that syntax.

use std::future::Future;
use std::io;
use std::path::Path;
use std::pin::Pin;

use bytes::Bytes;
use futures_core::Stream;

/// A lazily-produced sequence of chunks making up a `read()` call's
/// bytes - never the whole body materialized up front. A large file
/// served whole, or a large open-ended range, would otherwise sit
/// fully in memory before the first byte reaches the client.
pub type ByteStream = Pin<Box<dyn Stream<Item = io::Result<Bytes>> + Send + Sync>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    /// Inclusive.
    pub end: u64,
}

impl ByteRange {
    /// Never panics even if constructed with `end < start` directly
    /// (rather than through `core::http::range::parse`, which guarantees
    /// `end >= start`) — `saturating_sub` rather than assuming the
    /// invariant holds.
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start).saturating_add(1)
    }
}

pub trait ByteSource: Send + Sync {
    /// Whether this source can honor a byte-range request at all. If
    /// `false`, callers must ignore any `Range` header and serve the
    /// whole body with `200 OK` rather than lying about Range support —
    /// see docs/THREAT_MODEL.md and the spec's design note on this flag.
    fn supports_range(&self) -> bool;

    /// Reads the whole file (`range: None`) or just the given byte range,
    /// as a stream of chunks rather than one buffer - so a caller can
    /// start sending bytes before the whole body is read off disk.
    fn read<'a>(
        &'a self,
        path: &'a Path,
        range: Option<ByteRange>,
    ) -> Pin<Box<dyn Future<Output = io::Result<ByteStream>> + Send + 'a>>;
}
