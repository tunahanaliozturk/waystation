//! A second reading of the request framing on a client connection.
//!
//! hyper reads a request that has both `Content-Length` and
//! `Transfer-Encoding` by its transfer coding, which RFC 9112 section 6.3
//! allows, and drops the length before the proxy sees the head. The design
//! refuses such a request, because a parser elsewhere on the way may have
//! gone by the length instead. hyper does not say whether there was a length,
//! so the bytes from the client are read a second time here, on their way
//! into hyper.
//!
//! This reading decides nothing about what is forwarded: hyper alone frames
//! the request that goes upstream. It follows the framing of the requests
//! that hyper accepts, line by line, and keeps at most one short line. On
//! bytes that hyper would not accept it stops following, because hyper
//! closes the connection on them anyway.

use std::mem;

/// The longest part of a line that is kept: room for the name and value of
/// any length or chunk size hyper accepts. The rest of a longer line is
/// skipped.
const KEPT: usize = 256;

/// What the bytes from one client have shown so far.
#[derive(Debug)]
pub(crate) struct Framing {
    state: State,
    line: Vec<u8>,
    /// Whether the current line was longer than what is kept of it.
    cut: bool,
    /// Request heads read to their end.
    heads: usize,
    /// The index of the first request head that must be refused.
    refused: Option<usize>,
}

/// Where in the stream of requests the next byte falls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// In a request head, or before one.
    Head(Head),
    /// In a body of known length, with this many bytes to go.
    Fixed(u64),
    /// At the line that gives the size of the next chunk.
    ChunkSize,
    /// In a chunk, with this many bytes to go.
    Chunk(u64),
    /// At the line end after a chunk.
    ChunkEnd,
    /// In the trailer section after the last chunk.
    Trailers,
    /// Not following any more.
    Lost,
}

/// What a request head has said about its body so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Head {
    /// The request line has been read.
    started: bool,
    length: Length,
    /// A `Transfer-Encoding` field was present.
    coded: bool,
}

/// What the `Content-Length` fields of a head say.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Length {
    #[default]
    Absent,
    Valid(u64),
    /// Not a number, or two numbers that differ.
    Invalid,
}

impl Framing {
    pub(crate) fn new() -> Self {
        Self {
            state: State::Head(Head::default()),
            line: Vec::with_capacity(KEPT),
            cut: false,
            heads: 0,
            refused: None,
        }
    }

    /// The index of the first request on the connection, counting from
    /// zero, whose length two parsers could read differently.
    pub(crate) fn refused(&self) -> Option<usize> {
        self.refused
    }

    /// Follows `bytes`, the next ones the client sent.
    pub(crate) fn feed(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            bytes = match self.state {
                State::Lost => return,
                State::Fixed(left) | State::Chunk(left) => self.skip(left, bytes),
                State::Head(_) | State::ChunkSize | State::ChunkEnd | State::Trailers => {
                    self.read_line(bytes)
                }
            };
        }
    }

    /// Passes over body bytes, of which `left` are still to come.
    fn skip<'a>(&mut self, left: u64, bytes: &'a [u8]) -> &'a [u8] {
        let taken = usize::try_from(left).map_or(bytes.len(), |left| left.min(bytes.len()));
        let left = left.saturating_sub(u64::try_from(taken).unwrap_or(u64::MAX));
        self.state = match (self.state, left) {
            (State::Fixed(_), 0) => State::Head(Head::default()),
            (State::Fixed(_), left) => State::Fixed(left),
            (_, 0) => State::ChunkEnd,
            (_, left) => State::Chunk(left),
        };
        bytes.get(taken..).unwrap_or_default()
    }

    /// Adds `bytes` to the current line up to its end, and returns what
    /// comes after it.
    fn read_line<'a>(&mut self, bytes: &'a [u8]) -> &'a [u8] {
        let end = bytes.iter().position(|&byte| byte == b'\n');
        let part = end.map_or(bytes, |end| bytes.get(..end).unwrap_or_default());
        let room = KEPT.saturating_sub(self.line.len());
        self.line
            .extend_from_slice(part.get(..room).unwrap_or(part));
        self.cut |= part.len() > room;
        let Some(end) = end else {
            return &[];
        };
        let line = mem::take(&mut self.line);
        let cut = mem::take(&mut self.cut);
        self.state = self.line_ended(line.strip_suffix(b"\r").unwrap_or(&line), cut);
        self.line = line;
        self.line.clear();
        bytes.get(end + 1..).unwrap_or_default()
    }

    /// The state after a whole line, of which only the start was kept when
    /// `cut` is set.
    fn line_ended(&mut self, line: &[u8], cut: bool) -> State {
        match self.state {
            State::Head(head) => self.head_line(head, line, cut),
            State::ChunkSize => chunk_size(line, cut),
            State::ChunkEnd if line.is_empty() => State::ChunkSize,
            State::Trailers if line.is_empty() => State::Head(Head::default()),
            State::Trailers => State::Trailers,
            State::ChunkEnd | State::Fixed(_) | State::Chunk(_) | State::Lost => State::Lost,
        }
    }

    fn head_line(&mut self, mut head: Head, line: &[u8], cut: bool) -> State {
        if line.is_empty() {
            // Empty lines before a request line are skipped, as hyper does.
            return if head.started {
                self.head_ended(head)
            } else {
                State::Head(head)
            };
        }
        if !head.started {
            head.started = true;
            return State::Head(head);
        }
        let Some(colon) = line.iter().position(|&byte| byte == b':') else {
            return State::Head(head);
        };
        let name = line.get(..colon).unwrap_or_default();
        let value = line.get(colon + 1..).unwrap_or_default().trim_ascii();
        if name.eq_ignore_ascii_case(b"transfer-encoding") {
            head.coded = true;
        } else if name.eq_ignore_ascii_case(b"content-length") {
            let parsed = (!cut)
                .then(|| std::str::from_utf8(value).ok()?.parse::<u64>().ok())
                .flatten();
            head.length = match (head.length, parsed) {
                (Length::Absent, Some(length)) => Length::Valid(length),
                (Length::Valid(before), Some(length)) if before == length => head.length,
                _ => Length::Invalid,
            };
        }
        State::Head(head)
    }

    fn head_ended(&mut self, head: Head) -> State {
        let index = self.heads;
        self.heads += 1;
        match (head.coded, head.length) {
            (true, Length::Absent) => State::ChunkSize,
            (true, Length::Valid(_) | Length::Invalid) => {
                self.refused.get_or_insert(index);
                State::Lost
            }
            (false, Length::Absent | Length::Valid(0)) => State::Head(Head::default()),
            (false, Length::Valid(length)) => State::Fixed(length),
            // hyper answers 400 and closes the connection.
            (false, Length::Invalid) => State::Lost,
        }
    }
}

/// The state after a chunk size line.
fn chunk_size(line: &[u8], cut: bool) -> State {
    let mut parts = line.splitn(2, |&byte| byte == b';');
    let digits = parts.next().unwrap_or_default().trim_ascii_end();
    // Without the end of the digits, the size is not known.
    if cut && parts.next().is_none() {
        return State::Lost;
    }
    let size = std::str::from_utf8(digits)
        .ok()
        .and_then(|digits| u64::from_str_radix(digits, 16).ok());
    match size {
        Some(0) => State::Trailers,
        Some(size) => State::Chunk(size),
        None => State::Lost,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(bytes: &[u8]) -> Option<usize> {
        let mut framing = Framing::new();
        framing.feed(bytes);
        framing.refused()
    }

    /// The same, fed one byte at a time, so that every line is split.
    fn refused_bytewise(bytes: &[u8]) -> Option<usize> {
        let mut framing = Framing::new();
        for byte in bytes.chunks(1) {
            framing.feed(byte);
        }
        framing.refused()
    }

    const PLAIN: &[u8] = b"GET / HTTP/1.1\r\nhost: a.test\r\n\r\n";
    const BOTH: &[u8] =
        b"POST / HTTP/1.1\r\ncontent-length: 5\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n";

    #[test]
    fn a_head_with_a_length_and_a_transfer_coding_is_refused_whatever_the_order() {
        assert_eq!(refused(BOTH), Some(0));
        assert_eq!(
            refused(b"POST / HTTP/1.1\r\ntransfer-encoding: chunked\r\nContent-Length: 5\r\n\r\n"),
            Some(0)
        );
        assert_eq!(refused_bytewise(BOTH), Some(0));
    }

    #[test]
    fn plain_requests_are_not_refused() {
        assert_eq!(refused(PLAIN), None);
        assert_eq!(
            refused(b"POST / HTTP/1.1\r\ncontent-length: 3\r\n\r\nabc"),
            None
        );
        assert_eq!(
            refused(b"POST / HTTP/1.1\r\ntransfer-encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n"),
            None
        );
    }

    #[test]
    fn the_refused_request_is_counted_past_the_bodies_before_it() {
        // A length-framed body, a chunked body with an extension and a
        // trailer, and a body that holds text like a refused head: none of
        // them is taken for a head.
        let mut stream = Vec::new();
        stream.extend_from_slice(b"\r\n");
        stream.extend_from_slice(b"POST / HTTP/1.1\r\ncontent-length: ");
        stream.extend_from_slice(BOTH.len().to_string().as_bytes());
        stream.extend_from_slice(b"\r\n\r\n");
        stream.extend_from_slice(BOTH);
        stream.extend_from_slice(
            b"POST / HTTP/1.1\r\ntransfer-encoding: chunked\r\n\r\n\
              1a;name=value\r\nGET / HTTP/1.1\r\ncontent-le\r\n\
              0\r\nx-trailer: 1\r\n\r\n",
        );
        stream.extend_from_slice(PLAIN);
        stream.extend_from_slice(BOTH);
        assert_eq!(refused(&stream), Some(3));
        assert_eq!(refused_bytewise(&stream), Some(3));
    }

    #[test]
    fn a_long_line_is_skipped_without_being_kept() {
        let long = "a".repeat(10 * KEPT);
        let request = format!(
            "GET / HTTP/1.1\r\nx-long: {long}\r\ncontent-length: 1\r\n\r\nxGET / HTTP/1.1\r\n\r\n"
        );
        let mut framing = Framing::new();
        framing.feed(request.as_bytes());
        assert_eq!(framing.heads, 2);
        assert!(framing.line.capacity() <= KEPT);
    }

    #[test]
    fn bytes_hyper_would_refuse_end_the_reading() {
        for bytes in [
            &b"POST / HTTP/1.1\r\ncontent-length: x\r\n\r\n"[..],
            b"POST / HTTP/1.1\r\ntransfer-encoding: chunked\r\n\r\nzz\r\n",
            b"POST / HTTP/1.1\r\ntransfer-encoding: chunked\r\n\r\n1\r\nab\r\n",
        ] {
            let mut framing = Framing::new();
            framing.feed(bytes);
            framing.feed(BOTH);
            assert_eq!(framing.state, State::Lost);
            assert_eq!(framing.refused(), None);
        }
    }
}
