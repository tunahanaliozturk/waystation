//! The client's end of a connection, as hyper reads and writes it.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, ready};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

use crate::framing::Framing;

/// A client connection whose incoming bytes are read a second time for their
/// framing on the way into hyper. See [`crate::framing`].
#[derive(Debug)]
pub(crate) struct Downstream {
    stream: TcpStream,
    framing: Framing,
    shared: Arc<Shared>,
}

/// What the connection tells the requests that arrive on it.
#[derive(Debug)]
pub(crate) struct Shared {
    /// The index of the first request to refuse, or `usize::MAX`.
    refused: AtomicUsize,
    /// Requests handed to the service so far.
    requests: AtomicUsize,
}

impl Shared {
    pub(crate) fn new() -> Self {
        Self {
            refused: AtomicUsize::new(usize::MAX),
            requests: AtomicUsize::new(0),
        }
    }

    /// Counts one more request and says whether two parsers could read its
    /// length differently, so that it has to be refused.
    ///
    /// hyper parses a head only from bytes that went through
    /// [`Downstream`], so the framing of the request is known by the time it
    /// is counted here.
    pub(crate) fn next_request_is_ambiguous(&self) -> bool {
        let index = self.requests.fetch_add(1, Ordering::Relaxed);
        index >= self.refused.load(Ordering::Acquire)
    }
}

impl Downstream {
    pub(crate) fn new(stream: TcpStream, shared: Arc<Shared>) -> Self {
        Self {
            stream,
            framing: Framing::new(),
            shared,
        }
    }
}

impl AsyncRead for Downstream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        ready!(Pin::new(&mut this.stream).poll_read(cx, buf))?;
        this.framing
            .feed(buf.filled().get(before..).unwrap_or_default());
        if let Some(index) = this.framing.refused() {
            this.shared.refused.fetch_min(index, Ordering::Release);
        }
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for Downstream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}
