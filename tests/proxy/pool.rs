//! Reusing upstream connections: a reused connection carries nothing over
//! from the request before, a connection the upstream closed is not taken
//! for an open one, and a request that is not safe to repeat is never sent
//! twice.

use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use waystation::Config;

use crate::support::{Fault, Proxy, Upstream, eventually, exchange, get, keep_alive_request};

#[tokio::test(flavor = "multi_thread")]
async fn an_idle_connection_is_reused_for_the_next_request() {
    let upstream = Upstream::start(Fault::Echo).await;
    let proxy = Proxy::start(Config::new(upstream.addr));

    for _ in 0..3 {
        let reply = exchange(proxy.addr, &get("/")).await;
        assert_eq!((reply.status, reply.text()), (200, "bytes=0 pattern=ok"));
    }

    assert_eq!(upstream.requests(), 3);
    assert_eq!(upstream.accepted(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connection_the_upstream_closed_while_idle_is_not_used() {
    let upstream = Upstream::start(Fault::CloseWhenIdle(Duration::from_millis(100))).await;
    let proxy = Proxy::start(Config::new(upstream.addr));
    let mut client = TcpStream::connect(proxy.addr).await.unwrap();

    // A request that may be sent again, and two that may not: a POST with
    // a body and one without. Each finds the connection the one before it
    // used closed by the upstream, and none of them may see an error.
    let requests: [&[u8]; 3] = [
        b"GET / HTTP/1.1\r\nhost: example.test\r\n\r\n",
        b"POST / HTTP/1.1\r\nhost: example.test\r\ncontent-length: 0\r\n\r\n",
        b"POST / HTTP/1.1\r\nhost: example.test\r\ncontent-length: 5\r\n\r\nhello",
    ];
    for (index, request) in requests.into_iter().enumerate() {
        let reply = keep_alive_request(&mut client, request).await;
        assert_eq!(reply.status, 200, "request {index}: {}", reply.head);
        eventually(
            Duration::from_secs(5),
            "the upstream closed the idle connection",
            || upstream.open() == 0,
        )
        .await;
    }

    assert_eq!(upstream.requests(), 3);
    assert_eq!(upstream.accepted(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_safe_request_on_a_connection_closed_under_it_is_sent_again_on_a_new_one() {
    let upstream = Upstream::start(Fault::AnswerFirstOnly).await;
    let proxy = Proxy::start(Config::new(upstream.addr));
    let mut client = TcpStream::connect(proxy.addr).await.unwrap();
    let request = b"GET / HTTP/1.1\r\nhost: example.test\r\n\r\n";
    assert_eq!(keep_alive_request(&mut client, request).await.status, 200);

    // The idle connection looks fine, and the upstream closes it when the
    // request arrives.
    let reply = keep_alive_request(&mut client, request).await;

    assert_eq!(reply.status, 200, "{}", reply.head);
    assert_eq!(upstream.requests(), 3);
    assert_eq!(upstream.accepted(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unsafe_request_on_a_connection_closed_under_it_is_not_sent_twice() {
    let requests: [&[u8]; 2] = [
        // Nothing would stop this one from being sent again but its method.
        b"POST / HTTP/1.1\r\nhost: example.test\r\ncontent-length: 0\r\n\r\n",
        b"POST / HTTP/1.1\r\nhost: example.test\r\ncontent-length: 5\r\n\r\nhello",
    ];
    for request in requests {
        let upstream = Upstream::start(Fault::AnswerFirstOnly).await;
        let proxy = Proxy::start(Config::new(upstream.addr));
        let mut client = TcpStream::connect(proxy.addr).await.unwrap();
        let first = b"GET / HTTP/1.1\r\nhost: example.test\r\n\r\n";
        assert_eq!(keep_alive_request(&mut client, first).await.status, 200);

        let reply = keep_alive_request(&mut client, request).await;

        assert_eq!(reply.status, 502, "{}", String::from_utf8_lossy(request));
        let posts = upstream
            .heads()
            .iter()
            .filter(|head| head.starts_with("POST "))
            .count();
        assert_eq!(posts, 1, "{}", String::from_utf8_lossy(request));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn bytes_left_over_from_a_response_are_never_taken_for_the_next_one() {
    // Right behind the response, while the proxy still reads its body, and
    // later, while the connection sits idle in the pool.
    for pause in [Duration::ZERO, Duration::from_millis(20)] {
        let upstream = Upstream::start(Fault::ExtraBytes(pause)).await;
        let proxy = Proxy::start(Config::new(upstream.addr));
        let mut client = TcpStream::connect(proxy.addr).await.unwrap();
        let request = b"GET / HTTP/1.1
host: example.test

";

        for index in 0..3 {
            let reply = keep_alive_request(&mut client, request).await;
            assert_eq!(
                (reply.status, reply.text()),
                (200, "bytes=0 pattern=ok"),
                "request {index} after {pause:?}"
            );
            // The bytes nobody asked for are on their way before the next
            // request, and have time to arrive.
            eventually(Duration::from_secs(5), "extra bytes written", || {
                upstream.extra_sent() > index
            })
            .await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // Every connection that had bytes left over was closed, not reused.
        assert_eq!(upstream.accepted(), 3, "after {pause:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_is_sent_again_at_most_once_and_on_a_new_connection() {
    let upstream = Upstream::start(Fault::AnswerFirstOnly).await;
    let proxy = Proxy::start(Config::new(upstream.addr));

    // Two idle connections in the pool: an upload holds one connection
    // open while a second request needs another.
    let mut upload = TcpStream::connect(proxy.addr).await.unwrap();
    upload
        .write_all(
            b"POST / HTTP/1.1
host: example.test
content-length: 5

",
        )
        .await
        .unwrap();
    eventually(Duration::from_secs(5), "upload under way", || {
        upstream.requests() == 1
    })
    .await;
    let mut client = TcpStream::connect(proxy.addr).await.unwrap();
    let request = b"GET / HTTP/1.1
host: example.test

";
    assert_eq!(keep_alive_request(&mut client, request).await.status, 200);
    assert_eq!(keep_alive_request(&mut upload, b"hello").await.status, 200);
    assert_eq!((upstream.accepted(), upstream.requests()), (2, 2));

    // Both idle connections now close when a request arrives. The request
    // fails on one of them and goes once more, on a new connection, not on
    // the other.
    let reply = keep_alive_request(&mut client, request).await;

    assert_eq!(reply.status, 200, "{}", reply.head);
    assert_eq!((upstream.accepted(), upstream.requests()), (3, 4));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_idle_connection_is_closed_after_the_pool_idle_timeout() {
    let upstream = Upstream::start(Fault::Echo).await;
    let config = Config::from_toml(&format!(
        r#"
        [pool]
        idle_timeout = "200ms"

        [upstreams.app]
        servers = [{{ address = "{}" }}]

        [[routes]]
        upstream = "app"
        "#,
        upstream.addr
    ))
    .unwrap();
    let proxy = Proxy::start(config);

    assert_eq!(exchange(proxy.addr, &get("/")).await.status, 200);
    assert_eq!(upstream.open(), 1);

    // The sweep runs once a second, so this is at most about 1.2 s.
    eventually(Duration::from_secs(10), "idle connection closed", || {
        upstream.open() == 0
    })
    .await;
}
