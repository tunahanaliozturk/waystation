//! The M1 exit test: a gibibyte goes up through the proxy and another comes
//! down, and the heap of the whole process (client, proxy and upstream) never
//! holds more than a small fixed amount.
//!
//! This is its own test binary because the allocator counts every allocation
//! in the process, so nothing else may run beside it.

use std::time::Duration;

use cap::Cap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use waystation::Config;

#[expect(
    dead_code,
    reason = "this binary uses only the streaming parts of the support module"
)]
#[cfg(test)]
#[path = "proxy/support.rs"]
mod support;

use support::{Fault, PatternCheck, Proxy, Upstream, read_head, read_reply, write_pattern};

#[global_allocator]
static ALLOCATOR: Cap<std::alloc::System> = Cap::new(std::alloc::System, usize::MAX);

const GIB: u64 = 1024 * 1024 * 1024;
/// The most the heap may hold at once, over the whole run. A proxy that
/// buffered a whole body would need 128 times this much. The measured peak
/// on the machine this was written on is about 1.9 MB.
const CEILING: usize = 8 * 1024 * 1024;

#[tokio::test(flavor = "multi_thread")]
async fn a_gibibyte_each_way_streams_under_a_fixed_memory_ceiling() {
    // Both directions are bound by the request timeout like any other request.
    let timeouts = |config: Config| config.with_request_timeout(Duration::from_secs(600));

    let upstream = Upstream::start(Fault::Echo).await;
    let proxy = Proxy::start(timeouts(Config::new(upstream.addr)));
    let mut client = TcpStream::connect(proxy.addr).await.unwrap();
    let head = format!(
        "PUT /upload HTTP/1.1\r\nhost: example.test\r\ncontent-length: {GIB}\r\nconnection: close\r\n\r\n"
    );
    client.write_all(head.as_bytes()).await.unwrap();
    write_pattern(&mut client, GIB).await.unwrap();
    let reply = read_reply(&mut client).await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.text(), format!("bytes={GIB} pattern=ok"));
    let after_upload = ALLOCATOR.max_allocated();

    let upstream = Upstream::start(Fault::Download(GIB)).await;
    let proxy = Proxy::start(timeouts(Config::new(upstream.addr)));
    let mut client = TcpStream::connect(proxy.addr).await.unwrap();
    client.write_all(&support::get("/download")).await.unwrap();
    let (head, first) = read_head(&mut client).await.unwrap();
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let mut check = PatternCheck::default();
    check.feed(&first);
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = client.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        check.feed(&buf[..n]);
    }
    assert_eq!((check.len, check.intact), (GIB, true));

    let peak = ALLOCATOR.max_allocated();
    assert!(
        peak < CEILING,
        "heap peaked at {peak} bytes ({after_upload} during the upload), over the ceiling of {CEILING}"
    );
}
