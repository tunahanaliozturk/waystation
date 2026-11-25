//! What a request and its response look like on the far side of the proxy.

use std::time::Duration;

use waystation::Config;

use crate::support::{Fault, Proxy, Upstream, exchange, get, header, headers};

#[tokio::test(flavor = "multi_thread")]
async fn the_upstream_sees_the_host_fresh_forwarding_headers_and_no_hop_by_hop_headers() {
    let upstream = Upstream::start(Fault::Echo).await;
    let proxy = Proxy::start(Config::new(upstream.addr));
    let request = b"GET /path?q=1 HTTP/1.1\r\n\
        host: example.test:8080\r\n\
        connection: keep-alive, x-hop\r\n\
        x-hop: secret\r\n\
        keep-alive: timeout=5\r\n\
        te: trailers\r\n\
        proxy-authorization: Basic c2VjcmV0\r\n\
        x-forwarded-for: 203.0.113.66\r\n\
        x-forwarded-host: spoofed.test\r\n\
        forwarded: for=203.0.113.66\r\n\
        traceparent: 00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01\r\n\
        \r\n";

    let mut stream = tokio::net::TcpStream::connect(proxy.addr).await.unwrap();
    tokio::io::AsyncWriteExt::write_all(&mut stream, request)
        .await
        .unwrap();
    let (reply, _) = crate::support::read_head(&mut stream).await.unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");

    let seen = upstream.last_head();
    assert!(seen.starts_with("GET /path?q=1 HTTP/1.1\r\n"), "{seen}");
    assert_eq!(header(&seen, "host"), Some("example.test:8080"));
    for name in [
        "x-hop",
        "keep-alive",
        "te",
        "proxy-authorization",
        "connection",
    ] {
        assert_eq!(
            header(&seen, name),
            None,
            "{name} reached the upstream:\n{seen}"
        );
    }
    assert_eq!(headers(&seen, "x-forwarded-for"), ["127.0.0.1"]);
    assert_eq!(headers(&seen, "x-forwarded-host"), ["example.test:8080"]);
    assert_eq!(headers(&seen, "x-forwarded-proto"), ["http"]);
    assert_eq!(
        headers(&seen, "forwarded"),
        ["for=127.0.0.1;host=\"example.test:8080\";proto=http"]
    );
    assert_eq!(headers(&seen, "via"), ["1.1 waystation"]);
    assert_eq!(
        header(&seen, "traceparent"),
        Some("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_http_1_0_upstream_still_answers_an_http_1_1_client_in_http_1_1() {
    // No length: the body ends when the upstream closes the connection.
    let upstream =
        Upstream::start(Fault::Raw(b"HTTP/1.0 200 OK\r\nx-kept: yes\r\n\r\nhello")).await;
    let proxy = Proxy::start(Config::new(upstream.addr));
    let mut client = tokio::net::TcpStream::connect(proxy.addr).await.unwrap();
    let request = b"GET / HTTP/1.1\r\nhost: example.test\r\n\r\n";

    // Two requests on one connection: the version decides whether the
    // client may keep it open.
    for _ in 0..2 {
        tokio::io::AsyncWriteExt::write_all(&mut client, request)
            .await
            .unwrap();
        let (head, mut body) = crate::support::read_head(&mut client).await.unwrap();
        assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
        assert_eq!(header(&head, "transfer-encoding"), Some("chunked"));
        while !body.ends_with(b"0\r\n\r\n") {
            let mut buf = [0; 1024];
            let n = tokio::io::AsyncReadExt::read(&mut client, &mut buf)
                .await
                .unwrap();
            assert_ne!(n, 0, "connection closed after {head}");
            body.extend_from_slice(&buf[..n]);
        }
        assert_eq!(body, b"5\r\nhello\r\n0\r\n\r\n");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_client_sees_no_hop_by_hop_headers_from_the_upstream_and_the_body_reframed() {
    let upstream = Upstream::start(Fault::Raw(
        b"HTTP/1.1 200 OK\r\n\
        connection: x-internal\r\n\
        x-internal: 1\r\n\
        keep-alive: timeout=5\r\n\
        x-kept: yes\r\n\
        transfer-encoding: chunked\r\n\
        \r\n\
        5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
    ))
    .await;
    let proxy = Proxy::start(Config::new(upstream.addr));

    let reply = exchange(proxy.addr, &get("/")).await;

    assert_eq!(reply.status, 200);
    assert_eq!(reply.header("x-internal"), None, "{}", reply.head);
    assert_eq!(reply.header("keep-alive"), None, "{}", reply.head);
    assert_eq!(reply.header("x-kept"), Some("yes"));
    assert!(reply.complete);
    assert_eq!(reply.text(), "hello world");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_without_a_valid_host_is_refused_before_any_upstream_connection() {
    let upstream = Upstream::start(Fault::Echo).await;
    let proxy = Proxy::start(Config::new(upstream.addr));

    for request in [
        &b"GET / HTTP/1.1\r\nconnection: close\r\n\r\n"[..],
        b"GET / HTTP/1.1\r\nhost: a.test\r\nhost: b.test\r\nconnection: close\r\n\r\n",
        b"GET / HTTP/1.1\r\nhost: user@a.test\r\nconnection: close\r\n\r\n",
    ] {
        let reply = exchange(proxy.addr, request).await;
        assert_eq!(reply.status, 400, "{}", String::from_utf8_lossy(request));
    }
    assert_eq!(upstream.accepted(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_head_over_the_limit_gets_431_and_never_reaches_the_upstream() {
    let upstream = Upstream::start(Fault::Echo).await;
    let proxy = Proxy::start(Config::new(upstream.addr).with_max_header_bytes(16 * 1024));
    let big = "a".repeat(20 * 1024);
    let request = format!(
        "GET / HTTP/1.1\r\nhost: example.test\r\nx-big: {big}\r\nconnection: close\r\n\r\n"
    );

    let reply = exchange(proxy.addr, request.as_bytes()).await;

    assert_eq!(reply.status, 431);
    assert_eq!(upstream.accepted(), 0);

    // Just under the limit still goes through.
    let small = "a".repeat(15 * 1024);
    let request = format!(
        "GET / HTTP/1.1\r\nhost: example.test\r\nx-big: {small}\r\nconnection: close\r\n\r\n"
    );
    assert_eq!(exchange(proxy.addr, request.as_bytes()).await.status, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_response_head_over_the_limit_gets_502() {
    static HEAD: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    let head = HEAD.get_or_init(|| {
        let big = "a".repeat(20 * 1024);
        format!("HTTP/1.1 200 OK\r\nx-big: {big}\r\ncontent-length: 0\r\n\r\n").into_bytes()
    });
    let upstream = Upstream::start(Fault::Raw(head)).await;
    let proxy = Proxy::start(Config::new(upstream.addr).with_max_header_bytes(16 * 1024));

    let reply = exchange(proxy.addr, &get("/")).await;

    assert_eq!(reply.status, 502);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_upstream_connection_gets_502() {
    // A port that was free a moment ago and has nobody listening now.
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    // Windows retries a refused connection for about two seconds before it
    // gives up, so the connect timeout has to be longer than that here.
    let proxy = Proxy::start(Config::new(addr).with_connect_timeout(Duration::from_secs(10)));

    let reply = exchange(proxy.addr, &get("/")).await;

    assert_eq!(reply.status, 502);
    assert_eq!(reply.header("connection"), Some("close"));
}
