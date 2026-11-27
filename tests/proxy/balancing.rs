//! The M2 exit test: over 100,000 requests through the proxy, each server
//! of a group gets the share its weight gives it, and the requests travel on
//! a handful of reused upstream connections.
//!
//! The requests are counted where they arrive, at the upstreams, so the test
//! sees what the proxy did and not what it meant to do.

use tokio::net::TcpStream;
use waystation::Config;

use crate::support::{Fault, Proxy, Upstream, keep_alive_request};

const REQUESTS: usize = 100_000;
/// Client connections sending at the same time, each its share of the
/// requests one after another.
const CLIENTS: usize = 8;
const WEIGHTS: [usize; 3] = [1, 2, 5];
const TOTAL_WEIGHT: usize = 8;

/// Sends every request through a proxy whose one group balances with
/// `policy`. Returns the requests each upstream received and the upstream
/// connections opened in all.
async fn run(policy: &str) -> ([usize; 3], usize) {
    let upstreams = [
        Upstream::start(Fault::Echo).await,
        Upstream::start(Fault::Echo).await,
        Upstream::start(Fault::Echo).await,
    ];
    let servers: Vec<String> = upstreams
        .iter()
        .zip(WEIGHTS)
        .map(|(upstream, weight)| {
            format!("{{ address = \"{}\", weight = {weight} }}", upstream.addr)
        })
        .collect();
    let config = Config::from_toml(&format!(
        "[upstreams.app]\nbalance = \"{policy}\"\nservers = [{}]\n\n[[routes]]\nupstream = \"app\"\n",
        servers.join(", ")
    ))
    .unwrap();
    let proxy = Proxy::start(config);

    let clients: Vec<_> = (0..CLIENTS)
        .map(|_| {
            let addr = proxy.addr;
            tokio::spawn(async move {
                let mut stream = TcpStream::connect(addr).await.unwrap();
                for _ in 0..REQUESTS / CLIENTS {
                    let reply = keep_alive_request(
                        &mut stream,
                        b"GET / HTTP/1.1\r\nhost: example.test\r\n\r\n",
                    )
                    .await;
                    assert_eq!(reply.status, 200, "{}", reply.head);
                }
            })
        })
        .collect();
    for client in clients {
        client.await.unwrap();
    }

    let counts = [
        upstreams[0].requests(),
        upstreams[1].requests(),
        upstreams[2].requests(),
    ];
    let connections = upstreams.iter().map(Upstream::accepted).sum();
    (counts, connections)
}

fn expected(weight: usize) -> usize {
    REQUESTS * weight / TOTAL_WEIGHT
}

/// No server needs more connections than there are requests in flight at
/// once. A proxy that did not reuse connections would open one per request.
fn assert_reused(connections: usize) {
    assert!(
        connections <= CLIENTS * WEIGHTS.len(),
        "{connections} upstream connections for {REQUESTS} requests"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn round_robin_follows_the_weights_exactly_over_100_000_requests() {
    let (counts, connections) = run("round_robin").await;

    // Smooth round robin hands out each cycle of eight requests as 1, 2 and
    // 5, whatever order the clients' requests arrive in.
    assert_eq!(counts, WEIGHTS.map(expected));
    assert_reused(connections);
}

#[tokio::test(flavor = "multi_thread")]
async fn least_requests_follows_the_weights_within_one_percent_over_100_000_requests() {
    let (counts, connections) = run("least_requests").await;

    // The servers are alike, so the load they carry follows the weights, and
    // so do the random draws. The stated tolerance is 1% of all requests,
    // 1,000, for each server.
    for (count, weight) in counts.iter().zip(WEIGHTS) {
        assert!(
            count.abs_diff(expected(weight)) <= REQUESTS / 100,
            "{counts:?} against {:?}",
            WEIGHTS.map(expected)
        );
    }
    assert_eq!(counts.iter().sum::<usize>(), REQUESTS);
    assert_reused(connections);
}
