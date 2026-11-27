//! Request and response bodies on their way through the proxy.
//!
//! Neither body is buffered. A chunk is read from one side only when hyper
//! asks for one to write to the other side, so a slow reader slows the writer
//! down instead of filling memory.

use std::error::Error;
use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use hyper::body::{Body, Bytes, Frame, Incoming, SizeHint};
use hyper::client::conn::http1::{Connection, SendRequest};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::time::{Instant, Sleep, sleep_until};

use crate::balance::Lease;

/// The upstream end of one forwarded request.
pub(crate) type Upstream = Connection<TokioIo<TcpStream>, RequestBody>;

/// Why a body stopped before its end.
#[derive(Debug)]
pub(crate) enum BodyError {
    /// No chunk arrived within the idle timeout.
    Idle,
    /// The request ran past its deadline.
    Deadline,
    /// The peer broke the body: a reset, an early close or bad framing.
    Peer(hyper::Error),
}

impl fmt::Display for BodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => f.write_str("no body data within the idle timeout"),
            Self::Deadline => f.write_str("the request ran past its deadline"),
            Self::Peer(_) => f.write_str("the peer broke off the body"),
        }
    }
}

impl Error for BodyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Peer(error) => Some(error),
            Self::Idle | Self::Deadline => None,
        }
    }
}

/// How the request body ended, which decides whose fault a failed request is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sent {
    /// Every byte was handed to the upstream connection.
    Complete,
    /// The client stalled.
    Idle,
    /// The request deadline passed.
    Deadline,
    /// The client broke the body off.
    Broken,
}

impl From<&BodyError> for Sent {
    fn from(error: &BodyError) -> Self {
        match error {
            BodyError::Idle => Self::Idle,
            BodyError::Deadline => Self::Deadline,
            BodyError::Peer(_) => Self::Broken,
        }
    }
}

/// A body read from a peer, with a limit on the wait for each chunk and a
/// deadline for the whole request.
///
/// The idle timer runs only while the proxy is waiting for this body. While
/// the other side is slow to take what was already read, hyper does not poll
/// the body, so backpressure from a slow reader is never taken for a stalled
/// writer. The deadline is checked on every chunk as well, because a peer
/// that always has the next chunk ready never makes the proxy wait.
#[derive(Debug)]
pub(crate) struct Timed<B = Incoming> {
    inner: B,
    idle: Duration,
    deadline: Instant,
    timer: Pin<Box<Sleep>>,
    waiting: bool,
}

impl<B: Body<Data = Bytes, Error = hyper::Error> + Unpin> Timed<B> {
    pub(crate) fn new(inner: B, idle: Duration, deadline: Instant) -> Self {
        Self {
            inner,
            idle,
            deadline,
            timer: Box::pin(sleep_until(deadline)),
            waiting: false,
        }
    }

    fn poll_frame(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BodyError>>> {
        if Instant::now() >= self.deadline {
            return Poll::Ready(Some(Err(BodyError::Deadline)));
        }
        if let Poll::Ready(frame) = Pin::new(&mut self.inner).poll_frame(cx) {
            self.waiting = false;
            return Poll::Ready(frame.map(|frame| frame.map_err(BodyError::Peer)));
        }
        if !self.waiting {
            self.waiting = true;
            let idle_until = Instant::now()
                .checked_add(self.idle)
                .unwrap_or(self.deadline);
            self.timer.as_mut().reset(idle_until.min(self.deadline));
        }
        ready!(self.timer.as_mut().poll(cx));
        let error = if self.timer.deadline() < self.deadline {
            BodyError::Idle
        } else {
            BodyError::Deadline
        };
        Poll::Ready(Some(Err(error)))
    }
}

/// The client's request body on its way to the upstream.
///
/// It reports once how it ended, so that a failed request can be blamed on
/// the right side, and so that the response head timeout starts only when the
/// whole body has been sent.
#[derive(Debug)]
pub(crate) struct RequestBody {
    /// `None` for a request without a body.
    body: Option<Timed>,
    report: Option<oneshot::Sender<Sent>>,
}

impl RequestBody {
    /// Wraps `body`, or stands for no body at all, and returns the receiver
    /// that hears how it ended.
    pub(crate) fn new(body: Option<Timed>) -> (Self, oneshot::Receiver<Sent>) {
        let (sender, receiver) = oneshot::channel();
        let mut this = Self {
            body,
            report: Some(sender),
        };
        // hyper never polls a body that is empty from the start.
        if this.is_end_stream() {
            this.ended(Sent::Complete);
        }
        (this, receiver)
    }

    fn ended(&mut self, how: Sent) {
        if let Some(report) = self.report.take() {
            // The receiver is gone once the response head has arrived, and
            // then nobody needs to know.
            let _ = report.send(how);
        }
    }
}

impl Body for RequestBody {
    type Data = Bytes;
    type Error = BodyError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BodyError>>> {
        let this = self.get_mut();
        let Some(body) = this.body.as_mut() else {
            return Poll::Ready(None);
        };
        let frame = ready!(body.poll_frame(cx));
        match &frame {
            None => this.ended(Sent::Complete),
            // hyper stops polling after a frame that ends the stream.
            Some(Ok(_)) if body.inner.is_end_stream() => this.ended(Sent::Complete),
            Some(Ok(_)) => {}
            Some(Err(error)) => this.ended(error.into()),
        }
        Poll::Ready(frame)
    }

    fn is_end_stream(&self) -> bool {
        self.body
            .as_ref()
            .is_none_or(|body| body.inner.is_end_stream())
    }

    fn size_hint(&self) -> SizeHint {
        self.body
            .as_ref()
            .map_or_else(|| SizeHint::with_exact(0), |body| body.inner.size_hint())
    }
}

/// A response body as the client receives it.
#[derive(Debug)]
pub(crate) enum ResponseBody {
    /// Streamed from the upstream.
    Upstream(Streaming),
    /// Written by the proxy itself, for an error.
    Local(Option<Bytes>),
}

/// A response body on its way from the upstream, with the connection it
/// arrives on.
///
/// The upstream connection lives here, not in a task of its own (ADR 0002).
/// It has to be polled for the body to arrive, and the rest of an upload may
/// still be going out on it. When the client goes away, hyper drops this
/// body, and dropping the connection closes the socket to the upstream.
/// Nothing is left running. When the body was read to its end instead, the
/// connection goes back to the server's pool (ADR 0003).
#[derive(Debug)]
pub(crate) struct Streaming {
    body: Timed,
    /// The connection, until it has finished.
    connection: Option<Pin<Box<Upstream>>>,
    sender: Option<SendRequest<RequestBody>>,
    /// The request, counted against its server until this is dropped.
    lease: Lease,
    /// Whether the upstream's body ended the way its framing says it should.
    ended: bool,
}

impl Streaming {
    /// `connection` is `None` when it finished with the response head, and
    /// then there is nothing to give back.
    pub(crate) fn new(
        body: Timed,
        sender: SendRequest<RequestBody>,
        connection: Option<Pin<Box<Upstream>>>,
        lease: Lease,
    ) -> Self {
        Self {
            body,
            connection,
            sender: Some(sender),
            lease,
            ended: false,
        }
    }
}

impl Drop for Streaming {
    fn drop(&mut self) {
        // hyper does not poll a body that is empty from the start, so an
        // empty body is complete without having been read.
        if !(self.ended || self.body.inner.is_end_stream()) {
            return;
        }
        if let (Some(sender), Some(connection)) = (self.sender.take(), self.connection.take()) {
            self.lease.server().pool.put((sender, connection));
        }
    }
}

impl Body for ResponseBody {
    type Data = Bytes;
    type Error = BodyError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BodyError>>> {
        match self.get_mut() {
            Self::Upstream(streaming) => {
                // A connection that fails reports the failure into the body
                // as well, so its own result can be dropped here.
                let finished = streaming
                    .connection
                    .as_mut()
                    .is_some_and(|upstream| upstream.as_mut().poll(cx).is_ready());
                if finished {
                    streaming.connection = None;
                }
                let frame = ready!(streaming.body.poll_frame(cx));
                streaming.ended = match &frame {
                    None => true,
                    Some(Ok(_)) => streaming.body.inner.is_end_stream(),
                    Some(Err(_)) => false,
                };
                Poll::Ready(frame)
            }
            Self::Local(bytes) => Poll::Ready(bytes.take().map(|bytes| Ok(Frame::data(bytes)))),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            Self::Upstream(streaming) => streaming.body.inner.is_end_stream(),
            Self::Local(bytes) => bytes.is_none(),
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            Self::Upstream(streaming) => streaming.body.inner.size_hint(),
            Self::Local(bytes) => SizeHint::with_exact(
                bytes
                    .as_ref()
                    .map_or(0, |bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX)),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::poll_fn;

    /// A peer that always has the next chunk ready, so the proxy never waits.
    struct Endless;

    impl Body for Endless {
        type Data = Bytes;
        type Error = hyper::Error;

        fn poll_frame(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, hyper::Error>>> {
            Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"x")))))
        }
    }

    #[tokio::test]
    async fn a_body_that_is_always_ready_still_stops_at_the_deadline() {
        let deadline = Instant::now() + Duration::from_millis(50);
        let mut body = Timed::new(Endless, Duration::from_secs(60), deadline);
        let mut frames = 0_u64;
        let read = async {
            loop {
                match poll_fn(|cx| body.poll_frame(cx)).await {
                    Some(Ok(_)) => frames += 1,
                    Some(Err(error)) => break error,
                    None => panic!("an endless body ended"),
                }
                if frames % 1024 == 0 {
                    // Let the timeout below have a turn.
                    tokio::task::yield_now().await;
                }
            }
        };
        let error = tokio::time::timeout(Duration::from_secs(10), read)
            .await
            .expect("the body went on past the deadline");
        assert!(matches!(error, BodyError::Deadline), "{error}");
        assert!(Instant::now() >= deadline);
        assert!(frames > 0);
    }
}
