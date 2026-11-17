# waystation design

This document is the contract. Code that disagrees with it is a bug in one of the two, and the fix says which.

## Goals

1. Terminate HTTP/1.1 and HTTP/2 (TLS with ALPN) and forward to HTTP/1.1 upstreams over pooled connections.
2. Route by host and path prefix to named upstream groups.
3. Balance load, detect unhealthy upstreams and retry safely.
4. Reload configuration and shut down without failing a request in flight.
5. Use bounded memory per connection, whatever the body size.
6. Publish latency and throughput against nginx and Envoy, measured the same way.

## Non-goals

- HTTP/3 and QUIC.
- Caching, compression, request or response rewriting beyond headers.
- A scripting or plugin interface.
- Service discovery beyond static addresses and DNS.
- WebSocket and other upgrades in 0.1. They are listed for later because they need a different connection lifecycle.
- A web application firewall.

## Shape

```
listener ─> TLS (rustls) ─> HTTP/1.1 or HTTP/2 (hyper) ─> router ─> filters ─> balancer ─> pool ─> upstream
                                                             │
                                             admin: metrics, health, reload
```

One process, one tokio multi-thread runtime. Whether a thread-per-core layout is faster is a question for the benchmark milestone, not a starting assumption. hyper does the HTTP parsing and framing; waystation does everything around it. See [ADR 0001](adr/0001-hyper-not-a-proxy-framework.md).

## Configuration

One TOML file: listeners, routes, upstream groups, limits. Loading has two steps that never mix:

1. Parse and validate the whole file into an immutable `Config`. Any error rejects the file; nothing is half applied.
2. Swap the active `Arc<Config>` atomically.

A request reads the active config once, when it arrives, and keeps that snapshot until it finishes. Connections opened under an old config finish under it.

Reload is triggered by a `POST /reload` on the admin listener and by `SIGHUP` on Unix. The admin listener binds to loopback by default.

Changes to upstream groups reuse existing pools when the address is unchanged, so a reload does not throw away warm connections.

## Request path

1. Accept, with a limit on concurrent connections per listener.
2. TLS handshake with a timeout. Certificates are chosen by SNI.
3. Read the request head, with limits on header count, header bytes and time to first byte.
4. Reject ambiguous framing (see Security).
5. Match a route: exact host, then wildcard host, then longest path prefix.
6. Apply the rate limit for the route.
7. Pick an upstream and send. The body is streamed in both directions; waystation never buffers a whole body.
8. Strip hop-by-hop headers both ways. Add `Forwarded` and `X-Forwarded-*`. Pass `traceparent` through unchanged.
9. Record metrics and the access log line when the response ends or the client goes away.

### Backpressure

Bodies are streams. A chunk is read from one side only when the other side can take it, so a slow reader slows the writer instead of filling memory. Per-connection buffers have a fixed upper size.

### Cancellation

A client that disconnects cancels the upstream request. The upstream connection is returned to the pool only if the response was read to the end; otherwise it is closed. Every `select!` in the request path is reviewed for what is lost when the other branch wins, and that review is written next to the code.

## Load balancing

- Round robin.
- Least requests with power of two choices: pick two upstreams at random, send to the one with fewer requests in flight. This is the default, because it avoids the herd behaviour of a full least-requests scan and needs no shared lock.

Weights are supported for both.

## Health

- **Active**: a periodic HTTP check per upstream, with separate thresholds for marking unhealthy and healthy again.
- **Passive**: an upstream that returns consecutive connection errors or 5xx responses is ejected for a period that grows with repeated ejections. At most a configured share of a group can be ejected, so a bad deploy cannot empty the pool.

When every upstream in a group is unhealthy, waystation sends to all of them anyway. Refusing all traffic because the checks are wrong is worse than trying.

## Timeouts and retries

Timeouts: connect, TLS handshake, request head, idle between body chunks, upstream response head, whole request. Every one has a default; none is infinite.

A request is retried only when no byte of it reached an upstream, or when the method is idempotent and no byte of the response reached the client. Retries go to a different upstream. A retry budget per group caps retries at a share of recent requests, so a failing group does not get twice the load.

## Rate limiting

Token bucket per route, keyed by client address or by a configured header. State is in process; limits are per waystation instance. Over the limit returns 429 with `Retry-After`.

## Graceful shutdown

On `SIGTERM` or Ctrl-C: stop accepting, send `GOAWAY` on HTTP/2, send `Connection: close` on the next HTTP/1.1 response, wait for requests in flight up to a deadline, then close what is left and exit. The exit code says whether the deadline was hit.

## Observability

- Prometheus text metrics on the admin listener: requests by route and status class, latency histograms, in-flight requests, pool sizes, health state, retries, rate-limit rejections, reload results.
- Access log as one JSON line per request to stdout, written through a bounded channel so a slow log sink cannot block the request path. Dropped lines are counted.
- No request or response body, cookie or authorization header is ever logged.

## Security

- **Request smuggling**: a request with both `Content-Length` and `Transfer-Encoding`, with several differing `Content-Length` values, or with obsolete line folding, is answered with 400 and the connection is closed. waystation never forwards a request whose length two parsers could disagree about.
- **Limits**: header count and size, URI length, concurrent streams per HTTP/2 connection, and resets per connection (against rapid-reset floods).
- **Slow clients**: head and idle timeouts close connections that trickle bytes.
- **TLS**: rustls defaults, TLS 1.2 and 1.3. Private keys are read once at load and never logged.
- **Client-supplied forwarding headers** are replaced unless the peer address is in a configured trusted list.
- The configuration parser is fuzzed.

## Testing

- Integration tests start waystation and real upstreams in process on ephemeral ports.
- A fault-injecting upstream: slow head, slow body, reset mid-body, wrong length, hang.
- The smuggling suite, written as raw bytes on a socket because a well-behaved client cannot send them.
- h2spec in CI on Linux.
- Reload and shutdown tests run under load and assert zero failed requests.

## Benchmarks

Published in `docs/benchmarks.md` at M8, on Linux, with the hardware and the commands:

- Fixed-rate load with a generator that corrects for coordinated omission; p50, p99, p99.9 and maximum.
- Same upstream, same machine, same TLS settings for waystation, nginx and Envoy, plus the upstream called directly as the floor.
- Small responses, large streamed responses, and many idle keep-alive connections (memory per connection).

No target numbers are claimed before they are measured. If waystation is slower, the document says by how much and where the time goes.

## Dependencies

tokio, hyper, hyper-util, http-body-util, rustls with tokio-rustls, a TOML parser with serde, arc-swap. Versions are pinned when the first one is added, at the newest stable release. `cargo deny` checks licences and advisories in CI.
