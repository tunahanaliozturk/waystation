# waystation

An HTTP reverse proxy and load balancer for Rust, built on tokio, hyper and rustls.

A way station is a stop on a route where a courier changes horses and the message goes on. A proxy is that stop for requests.

## Status

Milestones M1 and M2 are done. waystation reads a TOML file, routes HTTP/1.1 requests by host and path to groups of upstream servers, balances them by weight with round robin or least requests (power of two choices), and reuses upstream connections. It streams bodies both ways, handles hop-by-hop and forwarding headers, and has every timeout from the design apart from the TLS handshake. TLS, HTTP/2, health checks and reload come in later milestones.

```
cargo run -- --config tests/config/valid/example.toml
```

The file format is in [docs/design.md](docs/design.md#configuration), and `tests/config/valid/example.toml` uses every setting.

What is tested so far, end to end, with a client, the proxy and an upstream in one process:

| Claim | Test |
| --- | --- |
| A 1 GiB upload and a 1 GiB download pass through intact while the heap of the whole test process stays under 8 MiB (about 1.9 MB measured). | `tests/memory.rs` |
| A client that disconnects mid-upload, while waiting for the response head, or mid-download leaves no task on the proxy and no open upstream connection, without waiting for a timeout. | `tests/proxy/disconnect.rs` |
| Hop-by-hop headers, including those named in `Connection`, never cross the proxy in either direction; client-supplied forwarding headers are replaced. | `tests/proxy/forwarding.rs` |
| An upstream that stalls, resets or sends less than it promised never produces a response that looks complete to the client. | `tests/proxy/faults.rs` |
| Each timeout fires on its own condition, and each has a test that waits it out: connect, request head, response head, idle in both directions, and the whole-request deadline on an upload, on a download that keeps moving and on a client that stops reading. A slow upload does not use up the response head timeout. The connect test needs a connection attempt to 192.0.2.1 to go unanswered, as it does on most networks; where it is answered at once, the test prints that it was skipped. | `tests/proxy/faults.rs` |
| A request with both `Content-Length` and `Transfer-Encoding`, two different lengths or a last coding other than `chunked` gets 400 and never reaches the upstream; any transfer coding other than `chunked` alone gets 501 from a client and 502 from the upstream. | `tests/proxy/forwarding.rs` |
| Over 100,000 requests through the proxy to servers weighted 1, 2 and 5, counted where they arrive, round robin gives each server exactly its share, and least requests gives each within 1% of all requests (at most 275 off in three local runs). The requests travel on at most 24 upstream connections (11 to 17 measured). | `tests/proxy/balancing.rs` |
| Every one of 29 invalid configuration files is rejected with a message that names the line of the problem. | `tests/config.rs`, `tests/config/invalid/` |
| A request goes to the routes of its exact host, else of the longest matching wildcard, else of any host, and there to the longest path prefix on a segment boundary; with no match it gets 404 and reaches no upstream. | `tests/proxy/routing.rs` |
| A reused upstream connection never hands a response the bytes left over from the one before. A connection the upstream closed while idle never fails a request, whatever its method. When the upstream closes a connection as a request arrives, a `GET` is sent once more on a new connection, and a `POST`, or a `PUT` with a body, gets 502 and reaches the upstream once. An idle connection the upstream closed is closed on the proxy's side by the next sweep, about a second later. | `tests/proxy/pool.rs` |
| A body whose next chunk is always ready still stops at the request deadline. | `a_body_that_is_always_ready_still_stops_at_the_deadline` in `src/body.rs` |

Each of these tests was checked against a mutation made by hand, a known-wrong change to the proxy that it has to catch. The lists are in [docs/roadmap.md](docs/roadmap.md), with the few mutations no test can catch and why.

More:

- [docs/design.md](docs/design.md): what it does, what it does not do, and how each promise will be tested.
- [docs/roadmap.md](docs/roadmap.md): milestones, each with an exit test.
- [docs/adr/](docs/adr/): decisions that are hard to reverse.

## What it will promise

The full list for 0.1. The first row is tested now; the rest arrive with their milestones. The check of a whole configuration file is tested now (see above); keeping the old one when a reload fails comes with M5.

| Promise | How it will be tested |
| --- | --- |
| Memory use does not grow with body size. A slow client or a slow upstream slows the other side down. | A 1 GiB upload and download through the proxy with a memory ceiling asserted in the test. |
| A configuration reload drops no request and no connection. | Reload repeatedly under constant load; the load generator must report zero errors. |
| A bad configuration is rejected as a whole and the old one keeps serving. | Reload with each kind of invalid file; routes and upstreams stay as they were. |
| Requests that two HTTP parsers could read differently are refused, not forwarded. | A request smuggling suite (conflicting `Content-Length` and `Transfer-Encoding`, duplicate lengths, obsolete line folding) expects 400 for every case. |
| An unhealthy upstream stops receiving traffic within a bounded time and gets it back when it recovers. | Integration tests that kill and revive upstreams and count misrouted requests. |
| HTTP/2 conforms to the specification. | h2spec runs in CI. |
| Latency overhead is measured, not claimed. | p50, p99 and p99.9 at a fixed request rate, against nginx and Envoy on the same machine, published with the commands. |

## Licence

Dual licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your option.
