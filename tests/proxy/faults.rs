//! Slow and broken peers, and the timeouts that deal with them.

use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use waystation::Config;

use crate::support::{
    Fault, PatternCheck, Proxy, Reply, Upstream, exchange, get, read_reply, write_pattern,
};

const SHORT: Duration = Duration::from_millis(300);
const LONG: Duration = Duration::from_secs(60);

#[tokio::test(flavor = "multi_thread")]
async fn an_upstream_slower_than_the_head_timeout_gets_504() {
    let upstream = Upstream::start(Fault::SlowHead(Duration::from_secs(5))).await;
    let proxy = Proxy::start(Config::new(upstream.addr).with_response_head_timeout(SHORT));

    let start = Instant::now();
    let reply = exchange(proxy.addr, &get("/")).await;

    assert_eq!(reply.status, 504);
    assert!(start.elapsed() >= SHORT);
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_head_timeout_starts_only_when_the_upload_is_done() {
    let upstream = Upstream::start(Fault::Echo).await;
    let proxy = Proxy::start(Config::new(upstream.addr).with_response_head_timeout(SHORT));
    let mut stream = TcpStream::connect(proxy.addr).await.unwrap();
    stream
        .write_all(b"POST / HTTP/1.1\r\nhost: example.test\r\ncontent-length: 10240\r\nconnection: close\r\n\r\n")
        .await
        .unwrap();

    // Ten pieces, 100 ms apart: an upload three times as long as the head
    // timeout, never idle for long.
    let pattern = crate::support::pattern();
    for piece in pattern[..10 * 1024].chunks(1024) {
        stream.write_all(piece).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let reply = read_reply(&mut stream).await;

    assert_eq!(reply.status, 200);
    assert_eq!(reply.text(), "bytes=10240 pattern=ok");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_stalls_mid_upload_gets_408() {
    let upstream = Upstream::start(Fault::Echo).await;
    let proxy = Proxy::start(
        Config::new(upstream.addr)
            .with_idle_timeout(SHORT)
            .with_request_timeout(LONG),
    );
    let mut stream = TcpStream::connect(proxy.addr).await.unwrap();
    stream
        .write_all(
            b"POST / HTTP/1.1\r\nhost: example.test\r\ncontent-length: 100\r\n\r\n0123456789",
        )
        .await
        .unwrap();

    let start = Instant::now();
    let reply = read_reply(&mut stream).await;

    assert_eq!(reply.status, 408);
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upstream_that_stalls_mid_body_has_its_response_cut_off() {
    let upstream = Upstream::start(Fault::Stall {
        len: 1_000_000,
        sent: 100_000,
    })
    .await;
    let proxy = Proxy::start(
        Config::new(upstream.addr)
            .with_idle_timeout(SHORT)
            .with_request_timeout(LONG),
    );

    let start = Instant::now();
    let reply = exchange(proxy.addr, &get("/")).await;

    assert_eq!(reply.status, 200);
    assert!(!reply.complete);
    assert_eq!(reply.body.len(), 100_000);
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_request_timeout_cuts_off_a_download_that_keeps_moving() {
    // 100 KiB at 1 KiB every 20 ms takes two seconds and is never idle for
    // long.
    let upstream = Upstream::start(Fault::Trickle {
        len: 100 * 1024,
        every: Duration::from_millis(20),
    })
    .await;
    let proxy = Proxy::start(
        Config::new(upstream.addr)
            .with_idle_timeout(SHORT)
            .with_request_timeout(Duration::from_millis(600)),
    );

    let start = Instant::now();
    let reply = exchange(proxy.addr, &get("/")).await;

    assert_eq!(reply.status, 200);
    assert!(!reply.complete);
    assert_ne!(reply.body.len(), 0);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upstream_reset_mid_body_never_looks_like_a_complete_response() {
    let upstream = Upstream::start(Fault::Reset { sent: 50_000 }).await;
    let proxy = Proxy::start(Config::new(upstream.addr));

    let reply = exchange(proxy.addr, &get("/")).await;

    assert_cut_off(&reply, 50_000);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upstream_body_shorter_than_its_length_is_cut_off_not_padded() {
    let upstream = Upstream::start(Fault::WrongLength {
        len: 100_000,
        sent: 40_000,
    })
    .await;
    let proxy = Proxy::start(Config::new(upstream.addr));

    let reply = exchange(proxy.addr, &get("/")).await;

    assert_cut_off(&reply, 40_000);
}

/// The client must see a response that is visibly unfinished. When the
/// upstream breaks off quickly, hyper may close the client connection before
/// it has flushed the head, and then the client sees nothing at all, which is
/// just as clear. Whatever did arrive must be the start of what the upstream
/// sent.
fn assert_cut_off(reply: &Reply, upstream_sent: u64) {
    assert!(
        !reply.complete,
        "the client saw a whole response: {}",
        reply.head
    );
    assert!(matches!(reply.status, 0 | 200), "{}", reply.head);
    if reply.status == 200 {
        let mut check = PatternCheck::default();
        check.feed(&reply.body);
        assert!(
            check.intact && check.len <= upstream_sent,
            "{} bytes",
            check.len
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_too_slow_with_its_request_head_is_disconnected() {
    let upstream = Upstream::start(Fault::Echo).await;
    let proxy = Proxy::start(Config::new(upstream.addr).with_request_head_timeout(SHORT));
    // The proxy's timer starts when it accepts the connection, so the clock
    // here starts before the connect.
    let start = Instant::now();
    let mut stream = TcpStream::connect(proxy.addr).await.unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nhost: exa")
        .await
        .unwrap();

    let mut rest = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut rest)).await;

    assert!(read.is_ok(), "the connection was still open after 3 s");
    assert!(start.elapsed() >= SHORT);
    assert_eq!(upstream.accepted(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_large_upload_and_download_arrive_intact() {
    const LEN: u64 = 32 * 1024 * 1024;
    let upstream = Upstream::start(Fault::Echo).await;
    let proxy = Proxy::start(Config::new(upstream.addr));
    let mut stream = TcpStream::connect(proxy.addr).await.unwrap();
    stream
        .write_all(format!("PUT / HTTP/1.1\r\nhost: example.test\r\ncontent-length: {LEN}\r\nconnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    write_pattern(&mut stream, LEN).await.unwrap();
    assert_eq!(
        read_reply(&mut stream).await.text(),
        format!("bytes={LEN} pattern=ok")
    );

    let upstream = Upstream::start(Fault::Download(LEN)).await;
    let proxy = Proxy::start(Config::new(upstream.addr));
    let reply = exchange(proxy.addr, &get("/")).await;
    let mut check = PatternCheck::default();
    check.feed(&reply.body);
    assert!(reply.complete);
    assert_eq!((check.len, check.intact), (LEN, true));
}
