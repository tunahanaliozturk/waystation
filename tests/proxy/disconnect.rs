//! A client that goes away mid-request leaves nothing behind: no task on the
//! proxy's runtime and no open connection to the upstream.
//!
//! The idle and request timeouts are a minute here, so they cannot be what
//! cleans up. The cleanup has to follow from the client going away.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use waystation::Config;

use crate::support::{Fault, Proxy, Upstream, eventually, get, read_head, write_pattern};

const LONG: Duration = Duration::from_secs(60);
/// Far below the timeouts, far above what the cleanup takes.
const SETTLE: Duration = Duration::from_secs(5);

fn proxy_for(upstream: &Upstream) -> Proxy {
    Proxy::start(
        Config::new(upstream.addr)
            .with_idle_timeout(LONG)
            .with_response_head_timeout(LONG)
            .with_request_timeout(LONG),
    )
}

/// Checks that the request is in flight: a task for the connection on the
/// proxy and one open upstream connection. Without this, the test could pass
/// because the request never got going.
fn assert_in_flight(proxy: &Proxy, upstream: &Upstream) {
    // At least the accept loop and the connection; a proxy that spawns more
    // per request is allowed, as long as they are gone afterwards.
    assert!(proxy.tasks() >= 2, "{} tasks", proxy.tasks());
    assert_eq!(upstream.open(), 1);
}

async fn assert_nothing_left(proxy: &Proxy, upstream: &Upstream) {
    eventually(SETTLE, "proxy back to its accept loop alone", || {
        proxy.tasks() == 1
    })
    .await;
    eventually(SETTLE, "upstream connection closed", || {
        upstream.open() == 0
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_disconnects_mid_upload_leaves_nothing_behind() {
    let upstream = Upstream::start(Fault::Echo).await;
    let proxy = proxy_for(&upstream);
    let mut client = TcpStream::connect(proxy.addr).await.unwrap();
    client
        .write_all(b"POST / HTTP/1.1\r\nhost: example.test\r\ncontent-length: 104857600\r\n\r\n")
        .await
        .unwrap();
    write_pattern(&mut client, 1024 * 1024).await.unwrap();
    eventually(SETTLE, "upload under way", || {
        upstream.received() >= 512 * 1024
    })
    .await;
    assert_in_flight(&proxy, &upstream);

    drop(client);

    assert_nothing_left(&proxy, &upstream).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_disconnects_while_waiting_for_the_head_leaves_nothing_behind() {
    let upstream = Upstream::start(Fault::Hang).await;
    let proxy = proxy_for(&upstream);
    let mut client = TcpStream::connect(proxy.addr).await.unwrap();
    client.write_all(&get("/")).await.unwrap();
    eventually(SETTLE, "request at the upstream", || {
        upstream.accepted() == 1
    })
    .await;
    assert_in_flight(&proxy, &upstream);

    drop(client);

    assert_nothing_left(&proxy, &upstream).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_disconnects_mid_download_from_a_stalled_upstream_leaves_nothing_behind() {
    let upstream = Upstream::start(Fault::Stall {
        len: 100 * 1024 * 1024,
        sent: 256 * 1024,
    })
    .await;
    let proxy = proxy_for(&upstream);
    let mut client = TcpStream::connect(proxy.addr).await.unwrap();
    client.write_all(&get("/")).await.unwrap();
    let (head, mut body) = read_head(&mut client).await.unwrap();
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let mut buf = vec![0; 64 * 1024];
    while body.len() < 256 * 1024 {
        let n = client.read(&mut buf).await.unwrap();
        assert_ne!(n, 0);
        body.extend_from_slice(&buf[..n]);
    }
    // Everything the upstream sent has arrived, and it sends no more.
    assert_in_flight(&proxy, &upstream);

    drop(client);

    assert_nothing_left(&proxy, &upstream).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_disconnects_mid_download_from_a_fast_upstream_leaves_nothing_behind() {
    let upstream = Upstream::start(Fault::Download(16 * 1024 * 1024 * 1024)).await;
    let proxy = proxy_for(&upstream);
    let mut client = TcpStream::connect(proxy.addr).await.unwrap();
    client.write_all(&get("/")).await.unwrap();
    let (head, _) = read_head(&mut client).await.unwrap();
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let mut buf = vec![0; 64 * 1024];
    for _ in 0..16 {
        assert_ne!(client.read(&mut buf).await.unwrap(), 0);
    }
    assert_in_flight(&proxy, &upstream);

    drop(client);

    assert_nothing_left(&proxy, &upstream).await;
}
