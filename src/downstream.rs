//! The client's end of a connection, as hyper reads and writes it.
//!
//! Two things happen here that hyper cannot be asked to do. The bytes from
//! the client are read a second time for their framing (see
//! [`crate::framing`]). And a write to the client that cannot go on fails
//! once the request deadline has passed: hyper does not poll a response body
//! while it cannot write, so no timer in the body would ever see a client
//! that stopped reading.

use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, ready};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::time::{Instant, Sleep, sleep_until};

use crate::framing::Framing;

/// A client connection whose incoming bytes are read a second time for their
/// framing, and whose writes stop at the request deadline.
#[derive(Debug)]
pub(crate) struct Downstream {
    stream: TcpStream,
    framing: Framing,
    shared: Arc<Shared>,
    /// Runs to the request deadline while a write is blocked. Made the first
    /// time one is.
    timer: Option<Pin<Box<Sleep>>>,
}

/// What the connection tells the requests that arrive on it.
#[derive(Debug)]
pub(crate) struct Shared {
    /// The index of the first request to refuse, or `usize::MAX`.
    refused: AtomicUsize,
    /// Requests handed to the service so far.
    requests: AtomicUsize,
    /// The deadline of the request being answered.
    deadline: Mutex<Option<Instant>>,
}

impl Shared {
    pub(crate) fn new() -> Self {
        Self {
            refused: AtomicUsize::new(usize::MAX),
            requests: AtomicUsize::new(0),
            deadline: Mutex::new(None),
        }
    }

    /// Sets the deadline of the request now being answered. Writes to the
    /// client that are blocked past it fail.
    pub(crate) fn set_deadline(&self, deadline: Instant) {
        *self.deadline.lock().unwrap_or_else(PoisonError::into_inner) = Some(deadline);
    }

    fn deadline(&self) -> Option<Instant> {
        *self.deadline.lock().unwrap_or_else(PoisonError::into_inner)
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
            timer: None,
        }
    }

    /// What a blocked write to the client returns: still pending until the
    /// request deadline, and an error from then on. hyper ends the connection
    /// on the error, which drops the response body and with it the upstream
    /// connection.
    fn blocked<T>(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<T>> {
        let Some(deadline) = self.shared.deadline() else {
            return Poll::Pending;
        };
        let timer = self
            .timer
            .get_or_insert_with(|| Box::pin(sleep_until(deadline)));
        if timer.deadline() != deadline {
            timer.as_mut().reset(deadline);
        }
        ready!(timer.as_mut().poll(cx));
        Poll::Ready(Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "the client took nothing more before the request deadline",
        )))
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
        let this = self.get_mut();
        match Pin::new(&mut this.stream).poll_write(cx, buf) {
            Poll::Pending => this.blocked(cx),
            done @ Poll::Ready(_) => done,
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        match Pin::new(&mut this.stream).poll_write_vectored(cx, bufs) {
            Poll::Pending => this.blocked(cx),
            done @ Poll::Ready(_) => done,
        }
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match Pin::new(&mut this.stream).poll_flush(cx) {
            Poll::Pending => this.blocked(cx),
            done @ Poll::Ready(_) => done,
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}
