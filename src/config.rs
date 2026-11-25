//! Settings for one listener: where requests go, and the limits and timeouts on the way.

use std::net::SocketAddr;
use std::time::Duration;

/// Where a listener forwards to, with the limits and timeouts that apply.
///
/// Every timeout has a default and none of them is infinite. The configuration
/// file arrives in milestone M2; until then this is built in code.
#[derive(Debug, Clone)]
pub struct Config {
    pub(crate) upstream: SocketAddr,
    pub(crate) connect_timeout: Duration,
    pub(crate) request_head_timeout: Duration,
    pub(crate) response_head_timeout: Duration,
    pub(crate) idle_timeout: Duration,
    pub(crate) request_timeout: Duration,
    pub(crate) max_header_bytes: usize,
}

impl Config {
    /// Forwards to `upstream` with the default limits and timeouts.
    #[must_use]
    pub fn new(upstream: SocketAddr) -> Self {
        Self {
            upstream,
            connect_timeout: Duration::from_secs(5),
            request_head_timeout: Duration::from_secs(10),
            response_head_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(60),
            request_timeout: Duration::from_secs(300),
            max_header_bytes: 32 * 1024,
        }
    }

    /// Time allowed to open the TCP connection to the upstream. A connection
    /// that is not open in time gets 504. Default 5 seconds.
    #[must_use]
    pub fn with_connect_timeout(self, timeout: Duration) -> Self {
        Self {
            connect_timeout: timeout,
            ..self
        }
    }

    /// Time allowed for a client to send a whole request head, counted from
    /// when the proxy starts waiting for it, so it also closes idle keep-alive
    /// connections. Default 10 seconds.
    #[must_use]
    pub fn with_request_head_timeout(self, timeout: Duration) -> Self {
        Self {
            request_head_timeout: timeout,
            ..self
        }
    }

    /// Time allowed for the upstream to send its response head, counted from
    /// when the last byte of the request body was handed to it. An upload of
    /// any length does not use up this time. Default 30 seconds.
    #[must_use]
    pub fn with_response_head_timeout(self, timeout: Duration) -> Self {
        Self {
            response_head_timeout: timeout,
            ..self
        }
    }

    /// Longest wait for the next chunk of a body, in either direction. A
    /// client that stalls mid-upload gets 408; an upstream that stalls
    /// mid-response has the response cut off. Default 60 seconds.
    #[must_use]
    pub fn with_idle_timeout(self, timeout: Duration) -> Self {
        Self {
            idle_timeout: timeout,
            ..self
        }
    }

    /// Time allowed for a whole request, from its head arriving to the last
    /// byte of the response. Default 300 seconds.
    #[must_use]
    pub fn with_request_timeout(self, timeout: Duration) -> Self {
        Self {
            request_timeout: timeout,
            ..self
        }
    }

    /// Largest request or response head, start line included, in bytes. A
    /// larger request head gets 431; a larger response head gets 502. hyper's
    /// read buffer (about 400 KiB) caps this whatever the setting. Default
    /// 32 KiB.
    #[must_use]
    pub fn with_max_header_bytes(self, bytes: usize) -> Self {
        Self {
            max_header_bytes: bytes,
            ..self
        }
    }
}
