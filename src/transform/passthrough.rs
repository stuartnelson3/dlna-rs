//! `PassthroughSource`: the default, non-transcoding `ByteSource`. Opens
//! the file, serves a range as a direct byte-offset seek, zero
//! transformation — matching the project's no-transcoding non-goal. A
//! downstream fork wanting transcoding implements `ByteSource` and wires
//! that in here instead, without touching `core`.

use std::future::Future;
use std::io;
use std::path::Path;
use std::pin::Pin;

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::core::byte_source::{ByteRange, ByteSource};

pub struct PassthroughSource;

impl ByteSource for PassthroughSource {
    fn supports_range(&self) -> bool {
        true
    }

    fn read<'a>(
        &'a self,
        path: &'a Path,
        range: Option<ByteRange>,
    ) -> Pin<Box<dyn Future<Output = io::Result<Bytes>> + Send + 'a>> {
        Box::pin(async move {
            let mut file = tokio::fs::File::open(path).await?;
            match range {
                None => {
                    let mut buf = Vec::new();
                    file.read_to_end(&mut buf).await?;
                    Ok(Bytes::from(buf))
                }
                Some(range) => {
                    file.seek(io::SeekFrom::Start(range.start)).await?;
                    let len = usize::try_from(range.len()).unwrap_or(usize::MAX);
                    let mut buf = vec![0u8; len];
                    file.read_exact(&mut buf).await?;
                    Ok(Bytes::from(buf))
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_the_whole_file_with_no_range() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("track.bin");
        std::fs::write(&path, b"0123456789").unwrap();

        let bytes = PassthroughSource.read(&path, None).await.unwrap();
        assert_eq!(&bytes[..], b"0123456789");
    }

    #[tokio::test]
    async fn reads_exactly_the_requested_range() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("track.bin");
        std::fs::write(&path, b"0123456789").unwrap();

        let bytes = PassthroughSource
            .read(&path, Some(ByteRange { start: 2, end: 5 }))
            .await
            .unwrap();
        assert_eq!(&bytes[..], b"2345");
    }

    #[tokio::test]
    async fn missing_file_is_an_io_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.bin");
        assert!(PassthroughSource.read(&path, None).await.is_err());
    }
}
