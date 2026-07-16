//! NDJSON line framing for the control socket (SPEC §7.1).
//!
//! Reads one `\n`-terminated line at a time with a hard byte cap, so a peer
//! cannot exhaust memory by never sending a newline. Exceeding the cap is an
//! error; the caller closes the connection (SPEC §7.1: "oversize → close").

use std::io;

use tokio::io::{AsyncBufReadExt, BufReader};

/// A capped, buffered line reader over any async byte stream.
pub struct LineReader<R> {
    inner: BufReader<R>,
}

impl<R: tokio::io::AsyncRead + Unpin> LineReader<R> {
    /// Wrap a reader with an internal buffer.
    pub fn new(reader: R) -> Self {
        Self {
            inner: BufReader::new(reader),
        }
    }

    /// Read the next line (without the trailing `\n`), or `None` at clean EOF.
    ///
    /// Returns [`io::ErrorKind::InvalidData`] if the line would exceed `cap`
    /// bytes, having consumed no more than `cap + 1` bytes into the buffer.
    pub async fn next_line(&mut self, cap: usize) -> io::Result<Option<Vec<u8>>> {
        let mut line = Vec::new();
        loop {
            let available = self.inner.fill_buf().await?;
            if available.is_empty() {
                // EOF: a trailing unterminated line is still delivered once.
                return Ok(if line.is_empty() { None } else { Some(line) });
            }

            if let Some(pos) = available.iter().position(|&b| b == b'\n') {
                if line.len() + pos > cap {
                    return Err(oversize(cap));
                }
                line.extend_from_slice(&available[..pos]);
                self.inner.consume(pos + 1); // drop the newline too
                return Ok(Some(line));
            }

            let consumed = available.len();
            if line.len() + consumed > cap {
                return Err(oversize(cap));
            }
            line.extend_from_slice(available);
            self.inner.consume(consumed);
        }
    }
}

fn oversize(cap: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("control line exceeds {cap} bytes"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn read_all(bytes: &[u8], cap: usize) -> io::Result<Vec<Vec<u8>>> {
        let mut reader = LineReader::new(bytes);
        let mut lines = Vec::new();
        while let Some(line) = reader.next_line(cap).await? {
            lines.push(line);
        }
        Ok(lines)
    }

    #[tokio::test]
    async fn splits_on_newlines() {
        let lines = read_all(b"one\ntwo\nthree\n", 64).await.unwrap();
        assert_eq!(
            lines,
            vec![b"one".to_vec(), b"two".to_vec(), b"three".to_vec()]
        );
    }

    #[tokio::test]
    async fn delivers_unterminated_final_line() {
        let lines = read_all(b"a\nb", 64).await.unwrap();
        assert_eq!(lines, vec![b"a".to_vec(), b"b".to_vec()]);
    }

    #[tokio::test]
    async fn empty_input_is_none() {
        let lines = read_all(b"", 64).await.unwrap();
        assert!(lines.is_empty());
    }

    #[tokio::test]
    async fn oversize_line_is_rejected() {
        // 100-byte line with a 16-byte cap, no newline within the cap.
        let big = vec![b'x'; 100];
        let err = read_all(&big, 16).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn line_exactly_at_cap_is_ok() {
        let lines = read_all(b"1234567890123456\n", 16).await.unwrap();
        assert_eq!(lines, vec![b"1234567890123456".to_vec()]);
    }
}
