//! `PassthroughSource`: the default, non-transcoding `ByteSource`. Opens
//! the file, serves a range as a direct byte-offset seek, zero
//! transformation — matching the project's no-transcoding non-goal. A
//! downstream fork wanting transcoding implements `ByteSource` and wires
//! that in here instead, without touching `core`.

use std::future::Future;
use std::io;
use std::path::Path;
use std::pin::Pin;

use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::core::byte_source::{ByteRange, ByteSource, ByteStream};

pub struct PassthroughSource;

impl ByteSource for PassthroughSource {
    fn supports_range(&self) -> bool {
        true
    }

    fn read<'a>(
        &'a self,
        path: &'a Path,
        range: Option<ByteRange>,
    ) -> Pin<Box<dyn Future<Output = io::Result<ByteStream>> + Send + 'a>> {
        Box::pin(async move {
            let mut file = tokio::fs::File::open(path).await?;
            let stream: ByteStream = match range {
                None => Box::pin(ReaderStream::new(file)),
                Some(range) => {
                    file.seek(io::SeekFrom::Start(range.start)).await?;
                    Box::pin(ReaderStream::new(file.take(range.len())))
                }
            };
            Ok(stream)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drains a `ByteStream` into one buffer, for tests that only care
    /// about the final bytes, not the chunking - production code (see
    /// `core::http`) is the one place that cares about laziness.
    async fn collect(mut stream: ByteStream) -> Vec<u8> {
        let mut buf = Vec::new();
        loop {
            match std::future::poll_fn(|cx| Pin::as_mut(&mut stream).poll_next(cx)).await {
                Some(chunk) => buf.extend_from_slice(&chunk.unwrap()),
                None => return buf,
            }
        }
    }

    #[tokio::test]
    async fn reads_the_whole_file_with_no_range() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("track.bin");
        std::fs::write(&path, b"0123456789").unwrap();

        let stream = PassthroughSource.read(&path, None).await.unwrap();
        assert_eq!(collect(stream).await, b"0123456789");
    }

    #[tokio::test]
    async fn reads_exactly_the_requested_range() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("track.bin");
        std::fs::write(&path, b"0123456789").unwrap();

        let stream = PassthroughSource
            .read(&path, Some(ByteRange { start: 2, end: 5 }))
            .await
            .unwrap();
        assert_eq!(collect(stream).await, b"2345");
    }

    #[tokio::test]
    async fn missing_file_is_an_io_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.bin");
        assert!(PassthroughSource.read(&path, None).await.is_err());
    }
}
