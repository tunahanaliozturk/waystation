//! What changes in a request head and a response head as they pass through.

use std::net::{IpAddr, SocketAddr};

use http::request::Parts;
use http::uri::Authority;
use hyper::header::{self, HeaderMap, HeaderName, HeaderValue};
use hyper::{Method, StatusCode, Uri, Version};

/// Headers that belong to one connection and are never forwarded: the list in
/// RFC 9110 section 7.6.1, plus `Proxy-Connection`, which old clients still
/// send, and the two proxy authentication headers, which are meant for this
/// proxy and not for the upstream.
const HOP_BY_HOP: [HeaderName; 9] = [
    header::CONNECTION,
    HeaderName::from_static("keep-alive"),
    HeaderName::from_static("proxy-connection"),
    header::PROXY_AUTHENTICATE,
    header::PROXY_AUTHORIZATION,
    header::TE,
    header::TRAILER,
    header::TRANSFER_ENCODING,
    header::UPGRADE,
];

const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");
const X_FORWARDED_HOST: HeaderName = HeaderName::from_static("x-forwarded-host");
const X_FORWARDED_PROTO: HeaderName = HeaderName::from_static("x-forwarded-proto");

/// Turns a client's request head into the head sent upstream.
///
/// The host is taken from the request, the hop-by-hop headers are removed,
/// and the forwarding headers are written fresh. Until there is a list of
/// trusted proxies (milestone M7), no client is trusted, so any forwarding
/// headers it sent are replaced, not extended.
///
/// # Errors
///
/// Returns the status to answer with when the request cannot be forwarded:
/// 400 for a missing, repeated or malformed `Host`, 501 for `CONNECT`.
pub(crate) fn prepare_request(
    head: &mut Parts,
    peer: SocketAddr,
    upstream: SocketAddr,
) -> Result<(), StatusCode> {
    if head.method == Method::CONNECT {
        return Err(StatusCode::NOT_IMPLEMENTED);
    }
    let host = request_host(head, upstream).ok_or(StatusCode::BAD_REQUEST)?;
    strip_hop_by_hop(&mut head.headers);
    // hyper already answered `100-continue` to the client when the body was
    // first read, so the upstream must not be asked to answer it again.
    head.headers.remove(header::EXPECT);
    set_forwarding(&mut head.headers, peer, &host)?;
    head.headers.insert(
        header::HOST,
        HeaderValue::from_str(&host).map_err(|_| StatusCode::BAD_REQUEST)?,
    );
    head.uri = head
        .uri
        .path_and_query()
        .cloned()
        .map_or_else(|| Uri::from_static("/"), Uri::from);
    head.version = Version::HTTP_11;
    Ok(())
}

/// Turns an upstream's response head into the head sent to the client.
pub(crate) fn prepare_response(headers: &mut HeaderMap) {
    strip_hop_by_hop(headers);
}

/// The host the client asked for, or `None` if the request must be refused.
///
/// An absolute URI wins over `Host` (RFC 9112 section 3.2.2). Otherwise
/// HTTP/1.1 needs exactly one valid `Host` (RFC 9112 section 3.2). HTTP/1.0
/// may leave it out, and then the upstream's address stands in.
fn request_host(head: &Parts, upstream: SocketAddr) -> Option<String> {
    let mut values = head.headers.get_all(header::HOST).iter();
    let first = values.next();
    if values.next().is_some() {
        return None;
    }
    let host = if let Some(host) = head.uri.host() {
        // Built from host and port so that any user information is left out.
        match head.uri.port_u16() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        }
    } else if let Some(value) = first {
        value.to_str().ok()?.to_owned()
    } else if head.version == Version::HTTP_10 {
        upstream.to_string()
    } else {
        return None;
    };
    // An authority with no user information is exactly what `Host` may hold,
    // and it can contain no quote, backslash or space, which is what makes it
    // safe to quote in `Forwarded` below. The `http` crate does not check
    // that a port is a number, so that is checked here.
    let port_is_numeric = match host.rsplit_once(':') {
        Some((_, port)) if !port.contains(']') => port.parse::<u16>().is_ok(),
        _ => true,
    };
    let valid = port_is_numeric && !host.contains('@') && host.parse::<Authority>().is_ok();
    valid.then_some(host)
}

/// Removes the hop-by-hop headers, including any that `Connection` names.
fn strip_hop_by_hop(headers: &mut HeaderMap) {
    let named: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect();
    for name in named.iter().chain(&HOP_BY_HOP) {
        headers.remove(name);
    }
}

/// Replaces `Forwarded` and `X-Forwarded-*` with what this proxy saw, and
/// adds this hop to `Via` (RFC 9110 section 7.6.3).
fn set_forwarding(headers: &mut HeaderMap, peer: SocketAddr, host: &str) -> Result<(), StatusCode> {
    let ip = peer.ip().to_canonical();
    // RFC 7239: an IPv6 address goes in brackets, and then the value needs quotes.
    let node = match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("\"[{ip}]\""),
    };
    let forwarded = format!("for={node};host=\"{host}\";proto=http");
    headers.insert(
        header::FORWARDED,
        HeaderValue::from_str(&forwarded).map_err(|_| StatusCode::BAD_REQUEST)?,
    );
    headers.insert(
        X_FORWARDED_FOR,
        HeaderValue::from_str(&ip.to_string()).map_err(|_| StatusCode::BAD_REQUEST)?,
    );
    headers.insert(
        X_FORWARDED_HOST,
        HeaderValue::from_str(host).map_err(|_| StatusCode::BAD_REQUEST)?,
    );
    headers.insert(X_FORWARDED_PROTO, HeaderValue::from_static("http"));
    headers.append(header::VIA, HeaderValue::from_static("1.1 waystation"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::Request;

    const PEER: &str = "192.0.2.7:51000";
    const UPSTREAM: &str = "127.0.0.1:9000";

    fn head(request: http::request::Builder) -> Parts {
        request.body(()).unwrap().into_parts().0
    }

    fn prepared(request: http::request::Builder) -> Result<Parts, StatusCode> {
        let mut head = head(request);
        prepare_request(&mut head, PEER.parse().unwrap(), UPSTREAM.parse().unwrap())?;
        Ok(head)
    }

    fn values<'a>(head: &'a Parts, name: &str) -> Vec<&'a str> {
        head.headers
            .get_all(name)
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect()
    }

    #[test]
    fn headers_named_in_connection_are_removed_whatever_their_case() {
        let head = prepared(
            Request::get("/a?b=c")
                .header("host", "example.test")
                .header("connection", "Keep-Alive, X-Secret ,x-other")
                .header("x-secret", "1")
                .header("x-other", "2")
                .header("x-kept", "3"),
        )
        .unwrap();
        assert!(head.headers.get("x-secret").is_none());
        assert!(head.headers.get("x-other").is_none());
        assert!(head.headers.get("connection").is_none());
        assert_eq!(values(&head, "x-kept"), ["3"]);
        assert_eq!(head.uri, "/a?b=c");
    }

    #[test]
    fn host_survives_a_connection_header_that_names_it() {
        let head = prepared(
            Request::get("/")
                .header("host", "example.test")
                .header("connection", "host"),
        )
        .unwrap();
        assert_eq!(values(&head, "host"), ["example.test"]);
    }

    #[test]
    fn forwarding_headers_from_the_client_are_replaced() {
        let head = prepared(
            Request::get("/")
                .header("host", "example.test:8080")
                .header("x-forwarded-for", "203.0.113.1")
                .header("x-forwarded-host", "evil.test")
                .header("x-forwarded-proto", "https")
                .header("forwarded", "for=203.0.113.1")
                .header("via", "1.0 earlier"),
        )
        .unwrap();
        assert_eq!(values(&head, "x-forwarded-for"), ["192.0.2.7"]);
        assert_eq!(values(&head, "x-forwarded-host"), ["example.test:8080"]);
        assert_eq!(values(&head, "x-forwarded-proto"), ["http"]);
        assert_eq!(
            values(&head, "forwarded"),
            ["for=192.0.2.7;host=\"example.test:8080\";proto=http"]
        );
        assert_eq!(values(&head, "via"), ["1.0 earlier", "1.1 waystation"]);
    }

    #[test]
    fn an_ipv6_client_is_bracketed_and_quoted_in_forwarded() {
        let mut head = head(Request::get("/").header("host", "example.test"));
        prepare_request(
            &mut head,
            "[2001:db8::1]:4000".parse().unwrap(),
            UPSTREAM.parse().unwrap(),
        )
        .unwrap();
        assert_eq!(
            values(&head, "forwarded"),
            ["for=\"[2001:db8::1]\";host=\"example.test\";proto=http"]
        );
        assert_eq!(values(&head, "x-forwarded-for"), ["2001:db8::1"]);
    }

    #[test]
    fn an_ipv4_client_on_a_dual_stack_socket_is_written_as_ipv4() {
        let mut head = head(Request::get("/").header("host", "example.test"));
        prepare_request(
            &mut head,
            "[::ffff:192.0.2.9]:4000".parse().unwrap(),
            UPSTREAM.parse().unwrap(),
        )
        .unwrap();
        assert_eq!(values(&head, "x-forwarded-for"), ["192.0.2.9"]);
    }

    #[test]
    fn a_host_that_is_missing_repeated_or_malformed_is_refused() {
        let refused = [
            Request::get("/"),
            Request::get("/")
                .header("host", "a.test")
                .header("host", "b.test"),
            Request::get("/").header("host", ""),
            Request::get("/").header("host", "user@a.test"),
            Request::get("/").header("host", "a.test\";x=\"y"),
            Request::get("/").header("host", "a.test:port"),
        ];
        for request in refused {
            assert_eq!(prepared(request).unwrap_err(), StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn an_absolute_uri_wins_over_host_and_loses_its_user_information() {
        let head =
            prepared(Request::get("http://user@origin.test:81/p?q").header("host", "other.test"))
                .unwrap();
        assert_eq!(values(&head, "host"), ["origin.test:81"]);
        assert_eq!(head.uri, "/p?q");
    }

    #[test]
    fn http_1_0_without_host_gets_the_upstream_address() {
        let head = prepared(Request::get("/").version(Version::HTTP_10)).unwrap();
        assert_eq!(values(&head, "host"), [UPSTREAM]);
        assert_eq!(head.version, Version::HTTP_11);
    }

    #[test]
    fn connect_is_not_implemented() {
        let request = Request::connect("example.test:443").header("host", "example.test:443");
        assert_eq!(prepared(request).unwrap_err(), StatusCode::NOT_IMPLEMENTED);
    }

    #[test]
    fn response_hop_by_hop_headers_are_removed() {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("connection", "x-internal"),
            ("x-internal", "1"),
            ("keep-alive", "timeout=5"),
            ("transfer-encoding", "chunked"),
            ("content-type", "text/plain"),
        ] {
            headers.insert(name, HeaderValue::from_static(value));
        }
        prepare_response(&mut headers);
        let names: Vec<&str> = headers.keys().map(HeaderName::as_str).collect();
        assert_eq!(names, ["content-type"]);
    }
}
