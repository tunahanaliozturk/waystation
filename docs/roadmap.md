# Roadmap

Each milestone ends with a test that has to pass. A milestone is not done because the code exists.

| # | Milestone | Scope | Exit test |
| --- | --- | --- | --- |
| M0 | Scaffold | Repository, lints, CI, design, roadmap. | CI green on an empty crate. |
| M1 | Forwarding | HTTP/1.1 in, HTTP/1.1 out, one upstream. Streaming bodies, hop-by-hop headers, forwarding headers, all timeouts, client-disconnect cancellation. Fault-injecting test upstream. | 1 GiB upload and download with a memory ceiling. A client that disconnects mid-body leaves no task and no upstream connection behind. |
| M2 | Routing and balancing | TOML config with whole-file validation. Host and path routing. Upstream groups, connection pools, round robin and power of two choices, weights. | Request distribution matches weights within a stated tolerance over 100,000 requests. Every invalid config example is rejected with a message that names the line. |
| M3 | TLS and HTTP/2 | rustls termination, SNI, ALPN, HTTP/2 downstream, stream and reset limits. | h2spec green in CI. |
| M4 | Health and retries | Active and passive health checks, ejection cap, retries with a budget. | Killing one of three upstreams under load fails no idempotent request. A group that returns only 5xx gets no more than the budgeted extra load. |
| M5 | Reload and shutdown | Atomic config swap, pool reuse across reloads, admin listener, graceful drain for HTTP/1.1 and HTTP/2. | 100 reloads under constant load with zero failed requests. Shutdown under load completes every request in flight before the deadline. |
| M6 | Limits and observability | Token bucket rate limiting, Prometheus metrics, JSON access log through a bounded channel. | Rate limit holds within a stated tolerance. A blocked log sink does not change request latency; dropped lines are counted. |
| M7 | Hardening | Smuggling suite, header and URI limits, slow-client timeouts, trusted-proxy handling, config parser fuzzing. | Every smuggling case gets 400. A slow-loris run with 10,000 connections keeps memory under a stated ceiling. |
| M8 | Numbers and release | Benchmarks against nginx and Envoy, flamegraph pass on the hot path, architecture diagrams, README with measured results. | `docs/benchmarks.md` reproducible from the listed commands. Tag v0.1.0. |

## Order of work inside a milestone

1. Write the exit test first, and see it fail.
2. Implement until it passes.
3. Record any decision that is hard to reverse as an ADR.

## After 0.1

Not planned, listed so they are not forgotten: WebSocket and CONNECT upgrades, HTTP/2 to upstreams, DNS-based upstream discovery, thread-per-core runtime if the benchmarks point there, certificate reload from disk without a config change.
