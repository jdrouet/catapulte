use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, ReadBuf};

pub struct LimitedReader<R> {
    inner: R,
    remaining: u64,
    exhausted: bool,
}

impl<R> LimitedReader<R> {
    pub fn new(inner: R, max_bytes: u64) -> Self {
        Self {
            inner,
            remaining: max_bytes,
            exhausted: false,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for LimitedReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.exhausted {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "attachment exceeded size limit",
            )));
        }

        if self.remaining == 0 {
            // Probe one byte to distinguish true EOF from excess data.
            let mut probe = [0u8; 1];
            let mut probe_buf = ReadBuf::new(&mut probe);
            match Pin::new(&mut self.inner).poll_read(cx, &mut probe_buf) {
                Poll::Ready(Ok(())) => {
                    if probe_buf.filled().is_empty() {
                        Poll::Ready(Ok(()))
                    } else {
                        self.exhausted = true;
                        Poll::Ready(Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "attachment exceeded size limit",
                        )))
                    }
                }
                Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                Poll::Pending => Poll::Pending,
            }
        } else {
            let cap = usize::try_from(self.remaining)
                .unwrap_or(usize::MAX)
                .min(buf.remaining());
            let dst = buf.initialize_unfilled_to(cap);
            let mut child = ReadBuf::new(&mut dst[..cap]);
            match Pin::new(&mut self.inner).poll_read(cx, &mut child) {
                Poll::Ready(Ok(())) => {
                    let n = child.filled().len();
                    buf.advance(n);
                    self.remaining = self.remaining.saturating_sub(n as u64);
                    Poll::Ready(Ok(()))
                }
                other => other,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncReadExt;

    use super::LimitedReader;

    #[tokio::test]
    async fn reads_data_within_cap_succeeds_and_returns_all_bytes() {
        let data = b"hello world";
        let cursor = std::io::Cursor::new(data);
        let mut reader = LimitedReader::new(cursor, 100);
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, data);
    }

    #[tokio::test]
    async fn reads_at_exactly_cap_succeed() {
        let data = b"exactly ten";
        let cursor = std::io::Cursor::new(data);
        let mut reader = LimitedReader::new(cursor, data.len() as u64);
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, data);
    }

    #[tokio::test]
    async fn reads_exceeding_cap_return_error() {
        let data = b"hello world";
        let cursor = std::io::Cursor::new(data);
        // Cap at 5 bytes; source has 11.
        let mut reader = LimitedReader::new(cursor, 5);
        let mut buf = Vec::new();
        let err = reader.read_to_end(&mut buf).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }
}
