//! Bounded in-memory reads of an HTTP response body.

use futures::{Stream, StreamExt};

/// Collect at most `max` bytes from `stream`, reporting whether more remained.
///
/// A `Content-Length` pre-check cannot stand in for this. The header is absent
/// on a chunked response and on a transport-decompressed one (a decompression
/// body reports no size hint at all, so there is no compressed-size bound to
/// scale up), which means the only place a decoded body can be bounded is while
/// it arrives. Peak memory is one chunk over `max` rather than the whole body.
///
/// Callers that treat an oversize body as an error check the flag and reject;
/// `robots.txt` keeps the prefix instead. Returning drops the stream, so the
/// rest of the body is never read, the connection is not returned to the pool,
/// and the origin stops sending.
///
/// The stream's own error type comes back untouched, because the right message
/// depends on the caller: several map it through [`crate::error::reqwest_message`]
/// to keep a credentialed request URL out of logs.
pub async fn read_capped<S, B, E>(stream: S, max: usize) -> Result<(Vec<u8>, bool), E>
where
    S: Stream<Item = Result<B, E>>,
    B: AsRef<[u8]>,
{
    let mut buf: Vec<u8> = Vec::new();
    let mut stream = std::pin::pin!(stream);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let chunk = chunk.as_ref();
        // `buf.len()` never passes `max`: the branch below returns on overflow.
        let room = max - buf.len();
        if chunk.len() > room {
            buf.extend_from_slice(&chunk[..room]);
            return Ok((buf, true));
        }
        buf.extend_from_slice(chunk);
    }
    Ok((buf, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream_of(chunks: &[&'static [u8]]) -> impl Stream<Item = Result<&'static [u8], String>> {
        futures::stream::iter(chunks.iter().copied().map(Ok).collect::<Vec<_>>())
    }

    #[tokio::test]
    async fn collects_a_whole_body_under_the_cap() {
        let (body, truncated) = read_capped(stream_of(&[b"ab", b"cd"]), 16).await.unwrap();
        assert_eq!(body, b"abcd");
        assert!(!truncated);
    }

    /// The boundary is inclusive, and — the bug this replaced — a body of
    /// exactly `max` is complete, not truncated. `robots.txt` trims its final
    /// line when truncated, so reporting a full read as truncated silently
    /// dropped a valid rule from any file that landed exactly on the cap.
    #[tokio::test]
    async fn a_body_of_exactly_the_cap_is_not_truncated() {
        let (body, truncated) = read_capped(stream_of(&[b"abcd"]), 4).await.unwrap();
        assert_eq!(body, b"abcd");
        assert!(!truncated);
    }

    #[tokio::test]
    async fn stops_at_the_cap_and_reports_truncation() {
        let (body, truncated) = read_capped(stream_of(&[b"abc", b"de"]), 4).await.unwrap();
        assert_eq!(
            body, b"abcd",
            "the prefix is kept, one chunk is not buffered whole"
        );
        assert!(truncated);
    }

    #[tokio::test]
    async fn an_empty_stream_is_not_an_error() {
        let (body, truncated) = read_capped(stream_of(&[]), 0).await.unwrap();
        assert!(body.is_empty());
        assert!(!truncated);
    }

    #[tokio::test]
    async fn the_stream_error_comes_back_untouched() {
        let s = futures::stream::iter([Ok(b"ab".as_slice()), Err("boom".to_string())]);
        assert_eq!(read_capped(s, 16).await.unwrap_err(), "boom");
    }
}
