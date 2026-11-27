//! Accepting client connections and forwarding each request upstream.

use std::convert::Infallible;
use std::future::poll_fn;
use std::net::SocketAddr;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use http::request;
use hyper::body::{Body as _, Bytes, Incoming};
use hyper::client::conn::http1::{self as client, SendRequest};
use hyper::header::{self, HeaderValue};
use hyper::server::conn::http1 as server;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::{Instant, MissedTickBehavior, interval, sleep, sleep_until, timeout_at};

use crate::Config;
use crate::body::{RequestBody, ResponseBody, Sent, Streaming, Timed, Upstream};
use crate::downstream::{Downstream, Shared};
use crate::headers;
use crate::pool::Halves;

/// Pause after a failed accept, so that a lasting error such as running out of
/// file descriptors does not spin the loop.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);

/// How often idle upstream connections are checked for expiry and for being
/// closed by the upstream.
const SWEEP_EVERY: Duration = Duration::from_secs(1);

/// Accepts connections on `listener` and forwards every request on them by
/// the routes in `config`.
///
/// Each connection is served on a task of its own. The future never
/// completes; dropping it stops accepting, and connections already open run
/// to their end. Between accepts, once a second, the idle upstream
/// connections are swept, so that no task of its own is needed for that.
pub async fn serve(listener: TcpListener, config: Config) -> Infallible {
    let config = Arc::new(config);
    let mut sweep = interval(SWEEP_EVERY);
    sweep.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    tokio::spawn(connection(stream, peer, Arc::clone(&config)));
                }
                // Accept fails when a connection was reset before it was
                // taken or the process is out of file descriptors. Neither is
                // a reason to stop listening.
                Err(_) => sleep(ACCEPT_BACKOFF).await,
            },
            _ = sweep.tick() => {
                for server in config.groups.iter().flat_map(|group| group.servers()) {
                    server.pool.sweep();
                }
            }
        }
    }
}

/// Serves one client connection until the client closes it, breaks it, or
/// is too slow.
async fn connection(stream: TcpStream, peer: SocketAddr, config: Arc<Config>) {
    // Without this a response head can wait for Nagle's algorithm.
    let _ = stream.set_nodelay(true);
    let mut builder = server::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(config.request_head_timeout)
        .max_header_size(config.max_header_bytes);
    let shared = Arc::new(Shared::new());
    let stream = Downstream::new(stream, Arc::clone(&shared));
    let service =
        service_fn(move |request| handle(request, peer, Arc::clone(&config), Arc::clone(&shared)));
    // The error says why the connection ended. The client already has
    // whatever answer there was, and there is no access log yet (M6) to put
    // the reason in.
    let _ = builder
        .serve_connection(TokioIo::new(stream), service)
        .await;
}

async fn handle(
    request: Request<Incoming>,
    peer: SocketAddr,
    config: Arc<Config>,
    shared: Arc<Shared>,
) -> Result<Response<ResponseBody>, Infallible> {
    let deadline = after(config.request_timeout);
    shared.set_deadline(deadline);
    if shared.next_request_is_ambiguous() {
        return Ok(local_response(StatusCode::BAD_REQUEST));
    }
    Ok(forward(request, peer, &config, deadline)
        .await
        .unwrap_or_else(local_response))
}

/// The proxy's own answer when a request cannot be forwarded.
///
/// The connection is closed after it, because part of the request body may
/// still be unread, and the next bytes on the connection would then be taken
/// for a new request.
fn local_response(status: StatusCode) -> Response<ResponseBody> {
    let reason = status.canonical_reason().unwrap_or_default();
    let mut response = Response::new(ResponseBody::Local(Some(Bytes::from_static(
        reason.as_bytes(),
    ))));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    headers.insert(header::CONNECTION, HeaderValue::from_static("close"));
    response
}

/// Forwards one request to a server of the group its route names and
/// returns the response with its body still streaming.
///
/// The request goes on an idle connection to that server if there is one.
/// When a reused connection fails before a response head, the request is
/// sent once more on a new connection, but only if that cannot do it twice:
/// hyper gave it back unsent, or it is idempotent and has no body, so that
/// it can be built again from its head (ADR 0003).
async fn forward(
    request: Request<Incoming>,
    peer: SocketAddr,
    config: &Config,
    deadline: Instant,
) -> Result<Response<ResponseBody>, StatusCode> {
    let (mut head, body) = request.into_parts();
    let host = headers::check_request(&head)?;
    let group = config
        .router
        .find(host.as_deref(), head.uri.path())
        .ok_or(StatusCode::NOT_FOUND)?;
    let lease = group.pick().ok_or(StatusCode::BAD_GATEWAY)?;
    let addr = lease.server().addr;
    let host = host.unwrap_or_else(|| addr.to_string());
    headers::prepare_request(&mut head, peer, &host)?;

    let empty = body.is_end_stream();
    let copy = (empty && head.method.is_idempotent()).then(|| copy_head(&head));
    let body = (!empty).then(|| Timed::new(body, config.idle_timeout, deadline));
    let (body, mut sent) = RequestBody::new(body);
    let mut request = Request::from_parts(head, body);
    let mut ended = None;
    let mut reuse = true;
    loop {
        let pooled = if reuse {
            lease.server().pool.take()
        } else {
            None
        };
        let reused = pooled.is_some();
        let (mut sender, connection) = match pooled {
            Some(halves) => halves,
            None => connect(addr, config, deadline).await?,
        };
        let exchange = Exchange {
            sent: &mut sent,
            ended: &mut ended,
            config,
            deadline,
        };
        let failure = match exchange.send(&mut sender, connection, request).await {
            Ok((response, connection)) => {
                let (mut head, body) = response.into_parts();
                headers::prepare_response(&mut head)?;
                let body = Timed::new(body, config.idle_timeout, deadline);
                let body = ResponseBody::Upstream(Streaming::new(body, sender, connection, lease));
                return Ok(Response::from_parts(head, body));
            }
            Err(failure) => failure,
        };
        // Once only, and only from a reused connection: a new connection that
        // fails says something about the server, not about the pool.
        reuse = false;
        request = match again(failure, reused, copy.as_ref()) {
            Ok(Again::Unsent(unsent)) => *unsent,
            Ok(Again::Copy(copy)) => {
                let (body, receiver) = RequestBody::new(None);
                sent = receiver;
                ended = None;
                Request::from_parts(copy_head(copy), body)
            }
            Err(Failure::Status(status)) => return Err(status),
            Err(Failure::Unsent(_) | Failure::Broken) => {
                // The body reports how it ended before it hands hyper an
                // error, so when the client caused the failure, that is known
                // by now.
                return Err(match ended.or_else(|| sent.try_recv().ok()) {
                    Some(Sent::Idle) => StatusCode::REQUEST_TIMEOUT,
                    Some(Sent::Broken) => StatusCode::BAD_REQUEST,
                    Some(Sent::Deadline) => StatusCode::GATEWAY_TIMEOUT,
                    Some(Sent::Complete) | None => StatusCode::BAD_GATEWAY,
                });
            }
        };
    }
}

/// How sending a request failed.
enum Failure {
    /// hyper gave the request back without writing any of it.
    Unsent(Box<Request<RequestBody>>),
    /// The connection broke before a response head.
    Broken,
    /// A timeout ran out; the status says which.
    Status(StatusCode),
}

/// What goes out once more after a failed attempt.
enum Again<'a> {
    /// The request hyper gave back unwritten, whatever its method.
    Unsent(Box<Request<RequestBody>>),
    /// A request rebuilt from this copy of its head.
    Copy(&'a request::Parts),
}

/// Whether a request that failed with `failure` is sent once more, and
/// what is sent (ADR 0003). Only a failure on a reused connection is tried
/// again; `copy` is there only for an idempotent request without a body.
/// `Err` hands the failure back to be answered.
fn again(
    failure: Failure,
    reused: bool,
    copy: Option<&request::Parts>,
) -> Result<Again<'_>, Failure> {
    match (failure, copy) {
        (Failure::Unsent(unsent), _) if reused => Ok(Again::Unsent(unsent)),
        (Failure::Broken, Some(copy)) if reused => Ok(Again::Copy(copy)),
        (failure, _) => Err(failure),
    }
}

/// Opens a new connection to `addr`.
async fn connect(
    addr: SocketAddr,
    config: &Config,
    deadline: Instant,
) -> Result<Halves, StatusCode> {
    let connect_by = after(config.connect_timeout).min(deadline);
    let stream = match timeout_at(connect_by, TcpStream::connect(addr)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(_)) => return Err(StatusCode::BAD_GATEWAY),
        Err(_) => return Err(StatusCode::GATEWAY_TIMEOUT),
    };
    let _ = stream.set_nodelay(true);
    let (sender, connection) = client::Builder::new()
        .max_header_size(config.max_header_bytes)
        .handshake(TokioIo::new(stream))
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    Ok((sender, Box::pin(connection)))
}

/// What one attempt at sending a request shares with the next.
struct Exchange<'a> {
    /// Hears how the request body ended.
    sent: &'a mut oneshot::Receiver<Sent>,
    /// How it ended, once that is known.
    ended: &'a mut Option<Sent>,
    config: &'a Config,
    deadline: Instant,
}

impl Exchange<'_> {
    /// Sends `request` on a connection and waits for the response head,
    /// polling the connection meanwhile. Returns the head and the
    /// connection, `None` if it has already finished.
    async fn send(
        self,
        sender: &mut SendRequest<RequestBody>,
        connection: Pin<Box<Upstream>>,
        request: Request<RequestBody>,
    ) -> Result<(Response<Incoming>, Option<Pin<Box<Upstream>>>), Failure> {
        let Self {
            sent,
            ended,
            config,
            deadline,
        } = self;
        let mut upstream = Some(connection);
        let mut response = pin!(sender.try_send_request(request));
        // Until the whole request body is sent, the only limit is the request
        // deadline: an upload may take as long as it needs while it keeps
        // moving, and the idle timeout in the body catches it when it stops.
        // Once the body is sent, the response head timeout starts.
        let head_by = || after(config.response_head_timeout).min(deadline);
        let mut timer = pin!(sleep_until(if ended.is_some() {
            head_by()
        } else {
            deadline
        }));

        // What each branch drops when another one wins:
        // - `response` wins: nothing. The upstream connection moves into the
        //   response body and goes on serving it.
        // - `upstream` wins: the connection has finished, perhaps with an
        //   error after handing over a response head, so the loop goes on
        //   until `response` says which. Nothing is dropped.
        // - `sent` wins: nothing is dropped; it only moves the timer.
        // - `timer` wins: the request and the upstream connection are
        //   dropped, which closes the socket to the upstream, and the client
        //   gets 504 with `Connection: close`.
        let response = loop {
            tokio::select! {
                biased;
                result = &mut response => break result,
                _ = poll_fn(|cx| upstream.as_mut().map_or(Poll::Pending, |c| c.as_mut().poll(cx))) => {
                    upstream = None;
                }
                how = &mut *sent, if ended.is_none() => {
                    // A body dropped without a word was not the client's fault.
                    *ended = Some(how.unwrap_or(Sent::Complete));
                    timer.as_mut().reset(head_by());
                }
                () = timer.as_mut() => return Err(Failure::Status(StatusCode::GATEWAY_TIMEOUT)),
            }
        };
        match response {
            Ok(response) => Ok((response, upstream)),
            Err(mut error) => Err(error.take_message().map_or(Failure::Broken, |request| {
                Failure::Unsent(Box::new(request))
            })),
        }
    }
}

/// A copy of a request head, for sending a request without a body again.
fn copy_head(head: &request::Parts) -> request::Parts {
    let mut copy = Request::new(()).into_parts().0;
    copy.method = head.method.clone();
    copy.uri = head.uri.clone();
    copy.version = head.version;
    copy.headers = head.headers.clone();
    copy
}

/// The instant `timeout` from now, or a year from now for a timeout too
/// long to represent.
fn after(timeout: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(timeout)
        .unwrap_or_else(|| now + Duration::from_secs(365 * 24 * 60 * 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unsent() -> Failure {
        Failure::Unsent(Box::new(Request::new(RequestBody::new(None).0)))
    }

    fn head() -> request::Parts {
        Request::new(()).into_parts().0
    }

    // The race these cover, a close that arrives after the pool's check and
    // before hyper writes, cannot be produced on purpose end to end.
    #[test]
    fn a_request_hyper_never_wrote_goes_again_from_a_reused_connection_whatever_its_method() {
        assert!(matches!(again(unsent(), true, None), Ok(Again::Unsent(_))));
        assert!(matches!(
            again(unsent(), false, None),
            Err(Failure::Unsent(_))
        ));
    }

    #[test]
    fn a_broken_request_goes_again_only_with_a_copy_and_from_a_reused_connection() {
        let copy = head();
        assert!(matches!(
            again(Failure::Broken, true, Some(&copy)),
            Ok(Again::Copy(_))
        ));
        assert!(matches!(
            again(Failure::Broken, true, None),
            Err(Failure::Broken)
        ));
        assert!(matches!(
            again(Failure::Broken, false, Some(&copy)),
            Err(Failure::Broken)
        ));
    }

    #[test]
    fn a_timeout_never_goes_again() {
        let copy = head();
        let timeout = || Failure::Status(StatusCode::GATEWAY_TIMEOUT);
        assert!(matches!(
            again(timeout(), true, Some(&copy)),
            Err(Failure::Status(_))
        ));
    }
}
