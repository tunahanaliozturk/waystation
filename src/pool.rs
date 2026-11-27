//! Idle upstream connections kept for the next request to the same server.
//!
//! A connection comes back only from a response body that was read to its
//! end, and only when hyper says it is ready for another request (see ADR
//! 0003). Nothing polls an idle connection in the background: there is no
//! task per connection (ADR 0002). Instead it is polled once, without a
//! waker, whenever it is put back, taken out or swept, and that poll is what
//! finds out that the upstream closed it or sent bytes nobody asked for.

use std::pin::Pin;
use std::sync::{Mutex, PoisonError};
use std::task::{Context, Waker};
use std::time::Duration;

use hyper::client::conn::http1::SendRequest;
use tokio::time::Instant;

use crate::body::{RequestBody, Upstream};

/// Limits on the idle connections kept per server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PoolSettings {
    /// The most idle connections kept for one server. Zero turns reuse off.
    pub(crate) max_idle: usize,
    /// How long a connection may stay idle before it is closed.
    pub(crate) idle_timeout: Duration,
}

impl Default for PoolSettings {
    fn default() -> Self {
        Self {
            max_idle: 64,
            idle_timeout: Duration::from_secs(60),
        }
    }
}

/// Both halves of an upstream connection: hyper's handle for sending a
/// request, and the connection itself, which has to be polled for anything to
/// happen on it.
pub(crate) type Halves = (SendRequest<RequestBody>, Pin<Box<Upstream>>);

/// The idle connections of one server.
#[derive(Debug)]
pub(crate) struct Pool {
    settings: PoolSettings,
    idle: Mutex<Vec<Idle>>,
}

#[derive(Debug)]
struct Idle {
    sender: SendRequest<RequestBody>,
    connection: Pin<Box<Upstream>>,
    since: Instant,
}

impl Pool {
    pub(crate) fn new(settings: PoolSettings) -> Self {
        Self {
            settings,
            idle: Mutex::new(Vec::new()),
        }
    }

    /// The most recently used idle connection that is still usable, if any.
    /// Connections found closed, expired or holding unasked-for bytes on the
    /// way are closed.
    pub(crate) fn take(&self) -> Option<Halves> {
        loop {
            let mut idle = self.lock().pop()?;
            if idle.since.elapsed() < self.settings.idle_timeout
                && usable(&mut idle.sender, &mut idle.connection)
            {
                return Some((idle.sender, idle.connection));
            }
        }
    }

    /// Keeps a connection whose last response was read to its end, if hyper
    /// says it can take another request and there is room. Otherwise it is
    /// closed.
    pub(crate) fn put(&self, (mut sender, mut connection): Halves) {
        if !usable(&mut sender, &mut connection) {
            return;
        }
        let mut idle = self.lock();
        if idle.len() < self.settings.max_idle {
            idle.push(Idle {
                sender,
                connection,
                since: Instant::now(),
            });
        }
    }

    /// Closes the idle connections that expired or that the upstream closed.
    pub(crate) fn sweep(&self) {
        let timeout = self.settings.idle_timeout;
        self.lock().retain_mut(|idle| {
            idle.since.elapsed() < timeout && usable(&mut idle.sender, &mut idle.connection)
        });
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Idle>> {
        self.idle.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Whether a connection can carry another request: it is still open and
/// hyper wants a request on it.
///
/// The poll lets hyper read what arrived while nobody was looking. An end of
/// stream finishes the connection, and so do bytes on a connection with no
/// request outstanding, which hyper refuses rather than keep them for the
/// next response. Either way the connection is then not pending any more.
/// No waker is needed: the next look at the connection polls it again.
fn usable(sender: &mut SendRequest<RequestBody>, connection: &mut Pin<Box<Upstream>>) -> bool {
    let mut cx = Context::from_waker(Waker::noop());
    connection.as_mut().poll(&mut cx).is_pending() && sender.is_ready()
}
