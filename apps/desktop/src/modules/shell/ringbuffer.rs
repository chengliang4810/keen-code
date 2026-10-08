use std::collections::VecDeque;

/// Byte-oriented bounded ring buffer with monotonic offsets.
///
/// Callers tail the buffer using `since_offset`: each `push` advances
/// `next_offset` by the number of bytes appended, even when older bytes are
/// dropped to fit the cap. `read_from(since)` returns the slice of bytes from
/// the requested offset (clamped to whatever is still resident) plus the new
/// offset for the next call.
pub struct BoundedRingBuffer {
    buf: VecDeque<u8>,
    cap: usize,
    next_offset: u64,
    /// Bytes that were dropped to keep the buffer ≤ cap. Helps the caller
    /// detect overflow ("you missed N bytes").
    dropped: u64,
}

impl BoundedRingBuffer {
    pub fn new(cap: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(cap.min(64 * 1024)),
            cap,
            next_offset: 0,
            dropped: 0,
        }
    }

    pub fn push(&mut self, data: &[u8]) {
        self.next_offset = self.next_offset.saturating_add(data.len() as u64);
        if data.len() >= self.cap {
            // Incoming chunk alone exceeds cap: keep only its tail.
            let keep_from = data.len() - self.cap;
            self.dropped = self
                .dropped
                .saturating_add((self.buf.len() + keep_from) as u64);
            self.buf.clear();
            self.buf.extend(&data[keep_from..]);
            return;
        }
        let overflow = (self.buf.len() + data.len()).saturating_sub(self.cap);
        if overflow > 0 {
            for _ in 0..overflow {
                self.buf.pop_front();
            }
            self.dropped = self.dropped.saturating_add(overflow as u64);
        }
        self.buf.extend(data);
    }

    pub fn read_from(&self, since: u64) -> (Vec<u8>, u64, u64) {
        let (bytes, offset, dropped, _) = self.read_page(since, usize::MAX);
        (bytes, offset, dropped)
    }

    pub fn read_page(&self, since: u64, max_bytes: usize) -> (Vec<u8>, u64, u64, bool) {
        let oldest = self.next_offset.saturating_sub(self.buf.len() as u64);
        let start = since.max(oldest).min(self.next_offset);
        let skip = (start - oldest) as usize;
        let length = self.buf.len().saturating_sub(skip).min(max_bytes);
        let (front, back) = self.buf.as_slices();
        let mut out = Vec::with_capacity(length);
        if skip < front.len() {
            let front_length = length.min(front.len() - skip);
            out.extend_from_slice(&front[skip..skip + front_length]);
            out.extend_from_slice(&back[..length - front_length]);
        } else {
            let back_skip = skip - front.len();
            if back_skip < back.len() {
                out.extend_from_slice(&back[back_skip..back_skip + length]);
            }
        }
        let next_offset = start.saturating_add(out.len() as u64);
        (
            out,
            next_offset,
            self.dropped,
            next_offset < self.next_offset,
        )
    }

    pub fn read_utf8_page(
        &self,
        since: u64,
        max_bytes: usize,
        ended: bool,
    ) -> (Vec<u8>, u64, u64, bool) {
        let (mut bytes, mut offset, dropped, mut more) = self.read_page(since, max_bytes.max(4));
        if more || !ended {
            if let Err(error) = std::str::from_utf8(&bytes) {
                if error.error_len().is_none() {
                    let removed = bytes.len() - error.valid_up_to();
                    bytes.truncate(error.valid_up_to());
                    offset -= removed as u64;
                    more = true;
                }
            }
        }
        (bytes, offset, dropped, more)
    }
}

#[cfg(test)]
mod tests {
    use super::BoundedRingBuffer;

    #[test]
    fn read_from_returns_all_when_within_cap() {
        let mut buf = BoundedRingBuffer::new(16);
        buf.push(b"hello world");
        let (bytes, off, dropped) = buf.read_from(0);
        assert_eq!(bytes, b"hello world");
        assert_eq!(off, 11);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn read_from_skips_consumed_prefix() {
        let mut buf = BoundedRingBuffer::new(16);
        buf.push(b"hello world");
        let (bytes, off, _) = buf.read_from(6);
        assert_eq!(bytes, b"world");
        assert_eq!(off, 11);
    }

    #[test]
    fn read_from_handles_wraparound() {
        let mut buf = BoundedRingBuffer::new(8);
        buf.push(b"abcdefgh");
        buf.push(b"ijkl");
        let (bytes, off, dropped) = buf.read_from(0);
        assert_eq!(bytes, b"efghijkl");
        assert_eq!(off, 12);
        assert_eq!(dropped, 4);
    }

    #[test]
    fn read_from_clamps_to_oldest() {
        let mut buf = BoundedRingBuffer::new(8);
        buf.push(b"abcdefgh");
        buf.push(b"ijkl");
        let (bytes, _, _) = buf.read_from(0);
        let (bytes2, _, _) = buf.read_from(99);
        assert_eq!(bytes, b"efghijkl");
        assert!(bytes2.is_empty());
    }

    #[test]
    fn push_larger_than_cap_keeps_tail() {
        let mut buf = BoundedRingBuffer::new(4);
        buf.push(b"abcdefgh");
        let (bytes, off, dropped) = buf.read_from(0);
        assert_eq!(bytes, b"efgh");
        assert_eq!(off, 8);
        assert_eq!(dropped, 4);
    }

    #[test]
    fn paginated_tail_advances_only_consumed_bytes_after_wraparound() {
        let mut buffer = BoundedRingBuffer::new(8);
        buffer.push(b"abcdefgh");
        buffer.push(b"ijkl");
        assert_eq!(buffer.read_page(0, 3), (b"efg".to_vec(), 7, 4, true));
        assert_eq!(buffer.read_page(7, 3), (b"hij".to_vec(), 10, 4, true));
        assert_eq!(buffer.read_page(10, 3), (b"kl".to_vec(), 12, 4, false));
        assert_eq!(buffer.read_page(99, 3), (Vec::new(), 12, 4, false));
    }

    #[test]
    fn unicode_pagination_reconstructs_text_with_small_and_large_pages() {
        let text = "界a测试z\u{1f600}".repeat(20_000);
        let mut buffer = BoundedRingBuffer::new(text.len());
        buffer.push(text.as_bytes());
        for limit in [1, 2, 4, 64 * 1024] {
            let mut output = Vec::new();
            let mut since = 0;
            loop {
                let (bytes, offset, _, more) = buffer.read_utf8_page(since, limit, true);
                assert!(std::str::from_utf8(&bytes).is_ok());
                assert!(offset > since);
                output.extend(bytes);
                since = offset;
                if !more {
                    break;
                }
            }
            assert_eq!(String::from_utf8(output).unwrap(), text);
        }
    }

    #[test]
    fn streaming_partial_utf8_tail_does_not_advance_until_complete() {
        let mut buffer = BoundedRingBuffer::new(32);
        buffer.push(&[b'a', 0xe7]);
        assert_eq!(
            buffer.read_utf8_page(0, 16, false),
            (b"a".to_vec(), 1, 0, true)
        );
        assert_eq!(
            buffer.read_utf8_page(1, 16, false),
            (Vec::new(), 1, 0, true)
        );
        buffer.push(&[0x95, 0x8c]);
        assert_eq!(
            buffer.read_utf8_page(1, 16, false),
            ("界".as_bytes().to_vec(), 4, 0, false)
        );
    }
}
