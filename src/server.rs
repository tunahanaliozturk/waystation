//! Accepting client connections and forwarding each request upstream.

use std::convert::Infallible;
use std::future::poll_fn;
use std::net::SocketAddr;
use std::pin::pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use hyper::body::{Bytes, Incoming};
use hyper::client::conn::http1 as client;
use hyper::header::{self, HeaderValue};
use hyper::server::conn::http1 as server;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Instant, sleep, sleep_until, timeout_at};

use crate::Config;
use crate::body::{RequestBody, ResponseBody, Sent, Timed};
use crate::downstream::{Downstream, Shared};
use crate::headers;

/// Pause after a failed accept, so that a lasting error such as running out of
/// file descriptors does not spin the loop.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);

/// Accepts connections on `listener` and forwards every request on them to
/// the upstream in `config`.
///
/// Each connection is served on a task of its own. The future never
/// completes; dropping it stops accepting, and connections already open run
/// to their end.
pub async fn serve(listener: TcpListener, config: Config) -> Infallible {
    let config = Arc::new(config);
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                tokio::spawn(connection(stream, peer, Arc::clone(&config)));
            }
            // Accept fails when a connection was reset before it was taken
            // or the process is out of file descriptors. Neither is a reason
            // to stop listening.
            Err(_) => sleep(ACCEPT_BACKOFF).await,
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

/// Forwards one request on a new upstream connection and returns the
/// response with its body still streaming.
///
/// There is one upstream connection per request until connection pools
/// arrive in milestone M2.
async fn forward(
    request: Request<Incoming>,
    peer: SocketAddr,
    config: &Config,
    deadline: Instant,
) -> Result<Response<ResponseBody>, StatusCode> {
    let (mut head, body) = request.into_parts();
    headers::prepare_request(&mut head, peer, config.upstream)?;

    let connect_by = after(config.connect_timeout).min(deadline);
    let stream = match timeout_at(connect_by, TcpStream::connect(config.upstream)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(_)) => return Err(StatusCode::BAD_GATEWAY),
        Err(_) => return Err(StatusCode::GATEWAY_TIMEOUT),
    };
    let _ = stream.set_nodelay(true);
    let (mut sender, upstream) = client::Builder::new()
        .max_header_size(config.max_header_bytes)
        .handshake(TokioIo::new(stream))
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let mut upstream = Some(Box::pin(upstream));

    let (body, mut sent) = RequestBody::new(Timed::new(body, config.idle_timeout, deadline));
    let mut response = pin!(sender.send_request(Request::from_parts(head, body)));
    // Until the whole request body is sent, the only limit is the request
    // deadline: an upload may take as long as it needs while it keeps moving,
    // and the idle timeout in the body catches it when it stops. Once the
    // body is sent, the response head timeout starts.
    let mut timer = pin!(sleep_until(deadline));
    let mut ended = None;

    // What each branch drops when another one wins:
    // - `response` wins: nothing. The upstream connection moves into the
    //   response body and goes on serving it.
    // - `upstream` wins: the connection has finished, perhaps with an error
    //   after handing over a response head, so the loop goes on until
    //   `response` says which. Nothing is dropped.
    // - `sent` wins: nothing is dropped; it only moves the timer.
    // - `timer` wins: the request and the upstream connection are dropped,
    //   which closes the socket to the upstream, and the client gets 504
    //   with `Connection: close`.
    let response = loop {
        tokio::select! {
            biased;
            result = &mut response => break result.ok(),
            _ = poll_fn(|cx| upstream.as_mut().map_or(Poll::Pending, |c| c.as_mut().poll(cx))) => {
                upstream = None;
            }
            how = &mut sent, if ended.is_none() => {
                // A body dropped without a word was not the client's fault.
                ended = Some(how.unwrap_or(Sent::Complete));
                timer.as_mut().reset(after(config.response_head_timeout).min(deadline));
            }
            () = timer.as_mut() => return Err(StatusCode::GATEWAY_TIMEOUT),
        }
    };
    let Some(response) = response else {
        // The body reports how it ended before it hands hyper an error, so
        // when the client caused the failure, that is known by now.
        return Err(match ended.or_else(|| sent.try_recv().ok()) {
            Some(Sent::Idle) => StatusCode::REQUEST_TIMEOUT,
            Some(Sent::Broken) => StatusCode::BAD_REQUEST,
            Some(Sent::Deadline) => StatusCode::GATEWAY_TIMEOUT,
            Some(Sent::Complete) | None => StatusCode::BAD_GATEWAY,
        });
    };

    let (mut head, body) = response.into_parts();
    headers::prepare_response(&mut head)?;
    let body = ResponseBody::Upstream {
        body: Timed::new(body, config.idle_timeout, deadline),
        connection: upstream,
    };
    Ok(Response::from_parts(head, body))
}

/// The instant `timeout` from now, or a year from now for a timeout too
/// long to represent.
fn after(timeout: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(timeout)
        .unwrap_or_else(|| now + Duration::from_secs(365 * 24 * 60 * 60))
}
