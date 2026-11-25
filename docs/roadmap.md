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

## Progress

- **M0**, 2026-11-17: done.
- **M1**, 2026-11-25: done. HTTP/1.1 in and out to one upstream, one upstream connection per request until the pools of M2. The exit test (`tests/memory.rs`) sends 1 GiB up and 1 GiB down through the proxy in one process with a counting allocator, and the heap of the whole process, client and upstream included, peaked at about 1.9 MB against a ceiling of 8 MiB. It takes 5 to 9 seconds in a debug build. Buffering the response body by hand pushed it over the ceiling. Four disconnect tests (`tests/proxy/disconnect.rs`) drop the client mid-upload, while waiting for the head, and mid-download from a stalled and from a fast upstream, with every timeout at a minute, and require the proxy runtime to be back to its accept loop and the upstream to have no open connection within 5 seconds. The decision that makes this hold is in ADR 0002. 16 more end-to-end tests cover headers, limits, status codes, each timeout and the faults of the test upstream (slow head, stall, trickle, reset, wrong length, hang). Every end-to-end test was checked against at least one mutation made by hand, listed below; each one compiled and turned at least one test red. Three things turned up on the way. Running the binary against Python's `http.server` showed that an HTTP/1.0 upstream made the proxy answer HTTP/1.1 clients in HTTP/1.0 and close their connections; the response version is now always the proxy's, and a test that failed before the fix keeps it that way. A test first claimed that spawning the upstream connection on its own task, as hyper's examples do, leaks it; it does not, because hyper ends that task once both halves are dropped, and the test was only failing on a task count it checked beforehand. And an upstream that sends less than its `Content-Length` and closes can make hyper close the client connection before the head is flushed, so the client may see nothing at all; the tests accept that as a cut-off response. Not done: the TLS handshake timeout and the rustls crypto provider belong to M3, and the connect timeout has no end-to-end test, because a connect that hangs cannot be produced on 127.0.0.1 the same way on every platform.

| Mutation made by hand | Caught by |
| --- | --- |
| Headers named in `Connection` kept | `the_upstream_sees_the_host_fresh_forwarding_headers_and_no_hop_by_hop_headers`, `the_client_sees_no_hop_by_hop_headers_from_the_upstream_and_the_body_reframed` |
| `X-Forwarded-For` appended instead of replaced | `the_upstream_sees_the_host_fresh_forwarding_headers_and_no_hop_by_hop_headers` |
| Response hop-by-hop headers kept | `the_client_sees_no_hop_by_hop_headers_from_the_upstream_and_the_body_reframed` |
| HTTP/1.1 without `Host` accepted | `a_request_without_a_valid_host_is_refused_before_any_upstream_connection` |
| No request head size limit | `a_request_head_over_the_limit_gets_431_and_never_reaches_the_upstream` |
| No response head size limit | `a_response_head_over_the_limit_gets_502` |
| Refused connect answered with 504 | `a_refused_upstream_connection_gets_502` |
| Response head timeout never armed | `an_upstream_slower_than_the_head_timeout_gets_504` |
| Response head timeout armed before the upload ends | `the_head_timeout_starts_only_when_the_upload_is_done` |
| Idle timeout 1,000 times too long | `a_client_that_stalls_mid_upload_gets_408`, `an_upstream_that_stalls_mid_body_has_its_response_cut_off` |
| A stalled client answered with 502 | `a_client_that_stalls_mid_upload_gets_408` |
| Request deadline ignored in bodies | `the_request_timeout_cuts_off_a_download_that_keeps_moving` |
| Upstream body errors turned into a clean end | `an_upstream_reset_mid_body_never_looks_like_a_complete_response` |
| The same, with `Content-Length` dropped and the size unknown | `an_upstream_body_shorter_than_its_length_is_cut_off_not_padded` and three others |
| Response body reports its end after the first chunk | `a_large_upload_and_download_arrive_intact` and eight others |
| Request head timeout 1,000 times too long | `a_client_too_slow_with_its_request_head_is_disconnected` |
| hyper half-close on, so a client's end of stream goes unnoticed | `a_client_that_disconnects_while_waiting_for_the_head_leaves_nothing_behind`, `a_client_that_disconnects_mid_download_from_a_stalled_upstream_leaves_nothing_behind` |
| The rest of a response drained after the client left | `a_client_that_disconnects_mid_download_from_a_fast_upstream_leaves_nothing_behind`, `a_client_that_disconnects_mid_download_from_a_stalled_upstream_leaves_nothing_behind` |
| Upstream connection spawned, and a broken client body waited out | `a_client_that_disconnects_mid_upload_leaves_nothing_behind` and three others |
| Upstream response version passed to the client (the bug itself, before the fix) | `an_http_1_0_upstream_still_answers_an_http_1_1_client_in_http_1_1` |
| Whole response body buffered before it is sent | `a_gibibyte_each_way_streams_under_a_fixed_memory_ceiling` |

The code and the tests were written together rather than test first; the mutations stand in for seeing each test fail. The script that applied them is not in the repository.

## Order of work inside a milestone

1. Write the exit test first, and see it fail.
2. Implement until it passes.
3. Record any decision that is hard to reverse as an ADR.

## After 0.1

Not planned, listed so they are not forgotten: WebSocket and CONNECT upgrades, HTTP/2 to upstreams, DNS-based upstream discovery, thread-per-core runtime if the benchmarks point there, certificate reload from disk without a config change.
