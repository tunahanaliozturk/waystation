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
- **M1**, 2026-11-25: done. HTTP/1.1 in and out to one upstream, one upstream connection per request until the pools of M2. The exit test (`tests/memory.rs`) sends 1 GiB up and 1 GiB down through the proxy in one process with a counting allocator, and the heap of the whole process, client and upstream included, peaked at about 1.9 MB against a ceiling of 8 MiB. It takes 5 to 9 seconds in a debug build. Buffering the response body by hand pushed it over the ceiling. Four disconnect tests (`tests/proxy/disconnect.rs`) drop the client mid-upload, while waiting for the head, and mid-download from a stalled and from a fast upstream, with every timeout at a minute, and require the proxy runtime to be back to its accept loop and the upstream to have no open connection within 5 seconds. The decision that makes this hold is in ADR 0002. 16 more end-to-end tests cover headers, limits, status codes, each timeout and the faults of the test upstream (slow head, stall, trickle, reset, wrong length, hang). Every end-to-end test was checked against at least one mutation made by hand, listed below; each one compiled and turned at least one test red. Three things turned up on the way. Running the binary against Python's `http.server` showed that an HTTP/1.0 upstream made the proxy answer HTTP/1.1 clients in HTTP/1.0 and close their connections; the response version is now always the proxy's, and a test that failed before the fix keeps it that way. A test first claimed that spawning the upstream connection on its own task, as hyper's examples do, leaks it; it does not, because hyper ends that task once both halves are dropped, and the test was only failing on a task count it checked beforehand. And an upstream that sends less than its `Content-Length` and closes can make hyper close the client connection before the head is flushed, so the client may see nothing at all; the tests accept that as a cut-off response. Not done: the TLS handshake timeout and the rustls crypto provider belong to M3. The connect timeout had no end-to-end test at first, because a connect that hangs cannot be produced on 127.0.0.1 the same way on every platform; the review round below added one.
- **M1 review round**, 2026-11-25: a reviewer read M1 without being able to run it, and each finding was first turned into a test that had to fail. Six changed the code. A request with both `Content-Length` and `Transfer-Encoding` was forwarded, because hyper drops the length before the proxy sees the head; the bytes from the client are now read a second time for their framing, and such a request gets 400. An HTTP/1.1 request in absolute form without `Host` was accepted, and so was a `Host` like `[zz]` or `[::1]x`; both get 400 now. A `gzip, chunked` message lost the declaration of its gzip on the way through; any transfer coding other than `chunked` alone now gets 501 from a client and 502 from an upstream. A client that stopped reading the response held the request open past its deadline, because hyper does not poll a body it cannot write; a write to the client that is blocked at the deadline now fails. And the test client took a chunked body for complete before its closing blank line. Two findings did not reproduce: two different lengths and a last coding other than `chunked` already got 400 from hyper, and an upload trickling just under the idle timeout already got 504 at the deadline. Their tests stay. The connect timeout now has a test against 192.0.2.1, which skips itself where a connect there does not hang. The new tests are in the table, each checked against its fix undone.

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
| A request with a length and a transfer coding forwarded (the review finding) | `a_request_whose_length_two_parsers_could_read_differently_gets_400_and_goes_nowhere`, `a_request_body_that_looks_like_a_request_head_is_not_taken_for_one` |
| Transfer codings other than `chunked` passed through (the review finding) | `a_request_with_a_transfer_coding_besides_chunked_gets_501_and_goes_nowhere`, `a_response_with_a_transfer_coding_besides_chunked_gets_502` |
| A blocked write to the client never times out (the review finding) | `the_request_timeout_cuts_off_a_client_that_stops_reading` |
| Connect timeout ignored | `an_upstream_connect_that_hangs_gets_504` |

The code and the tests were written together rather than test first; the mutations stand in for seeing each test fail. The script that applied them is not in the repository.

- **M2**, 2026-11-25: done. A TOML file (`waystation --config FILE`) holds the listen address, timeouts, limits, pool settings, upstream groups and routes, and is checked as a whole: the first problem rejects it with its line and column. Routes match the exact host, then the longest wildcard, then any host, and among the routes for that host the longest path prefix on a segment boundary (ADR 0004). Groups balance by smooth weighted round robin or by weighted least requests with the power of two choices (ADR 0005). Each server keeps its idle connections, and ADR 0003 says when one is reused and when a request goes out a second time. The exit test has two halves. `tests/proxy/balancing.rs` sends 100,000 requests through the proxy from eight keep-alive clients to three servers weighted 1, 2 and 5, and counts them where they arrive. Round robin gave exactly 12,500, 25,000 and 62,500. Least requests stayed within the stated tolerance of 1% of all requests (1,000) for each server, and was at most 275 off in three local runs. Both runs used at most 24 upstream connections (11 and 17 measured), so connections are reused; each takes about 6 seconds in a debug build. `tests/config.rs` loads the 29 invalid files in `tests/config/invalid/`, each of which names in its first line the line its error must name, and checks every one. The one error with no line of its own, a file with no routes, names line 1. The guarantees of M1 hold with pooling in place: the memory test still peaks at about 2.0 MB against its ceiling of 8 MiB, every M1 test passes, with one that counted upstream connections where it meant requests now counting requests and the test upstream answering more than one request per connection as real servers do, and pooling a connection whose response was not read to its end turns two disconnect tests red (mutation X below). The open item from M1 is settled: the request deadline in a body was a timer that ran only while the proxy waited, so a peer whose next chunk was always ready never met it. It is now checked on every chunk, and a unit test with a body that is never pending fails on its own ten-second timeout without that check. Found on the way: hyper's client reads an idle connection before it writes a request on it, and hands back a request it never wrote, so the proxy's own poll of a connection taken from the pool and its resend of an unwritten request back each other up, and an end-to-end request test fails only when both are gone. A resumed pass closed that gap from both sides: the decision of what goes out again is now a function with unit tests of its own, since the race where the resend alone matters (a close landing between the pool's poll and hyper's write) cannot be produced on purpose, and a new test has the upstream close an idle connection and requires the sweep to close the proxy's end within seconds, not at the minute-long idle timeout, which only the poll can do. The same pass found that nothing caught a `PUT` with a body being rebuilt from its head and sent again without the body; the code was right, and the test for unsafe requests now sends a `PUT` too. Not done here: host names as server addresses (they need DNS), path normalisation (M7), reload (M5), and retries to other servers (M4).

| Mutation made by hand (M2) | Caught by |
| --- | --- |
| Request deadline not checked on a body that is always ready (the M1 open item) | `a_body_that_is_always_ready_still_stops_at_the_deadline` |
| Every weight read as 1 | both tests in `tests/proxy/balancing.rs` |
| Round robin adds 1 instead of the weight | `round_robin_follows_the_weights_exactly_over_100_000_requests` |
| Least requests draws servers uniformly | `least_requests_follows_the_weights_within_one_percent_over_100_000_requests` |
| Least requests takes the busier of the two | that test, `least_requests_sends_new_requests_away_from_a_loaded_server`, `load_is_compared_per_unit_of_weight` |
| Load not divided by weight | `load_is_compared_per_unit_of_weight` |
| A request never stops counting as in flight | `least_requests_sends_new_requests_away_from_a_loaded_server`, `load_is_compared_per_unit_of_weight` (the exit test does not notice: counts that only grow still follow the weights) |
| Exact host ranked as any host; a host falling through to other hosts' routes; prefixes matching inside a segment; the request host compared with its case | `each_request_reaches_the_group_its_host_and_path_choose`, each of the four |
| Duplicate routes allowed; lines counted from zero; weight zero let through; unknown server keys ignored | `every_invalid_example_is_rejected_with_the_line_it_is_on`, each of the four |
| A connection never put back in the pool | `an_idle_connection_is_reused_for_the_next_request` and the exit test |
| A broken safe request not sent again | `a_safe_request_on_a_connection_closed_under_it_is_sent_again_on_a_new_one` |
| A broken request sent again from a new connection too | `a_broken_request_goes_again_only_with_a_copy_and_from_a_reused_connection` |
| A request hyper never wrote not sent again | `a_request_hyper_never_wrote_goes_again_from_a_reused_connection_whatever_its_method` |
| The same, from a new connection too | that test |
| A `POST` without a body sent again | `an_unsafe_request_on_a_connection_closed_under_it_is_not_sent_twice` |
| An idempotent request with a body rebuilt from its head and sent again without it | the same test, with its `PUT` (nothing caught this before) |
| The pool's poll of an idle connection removed | `an_idle_connection_the_upstream_closed_is_closed_by_the_sweep_long_before_its_timeout` |
| The second attempt allowed onto another pooled connection | `a_request_is_sent_again_at_most_once_and_on_a_new_connection` |
| The sweep ignores the idle timeout; no sweep at all | `an_idle_connection_is_closed_after_the_pool_idle_timeout` |
| No poll of a connection taken from the pool, and no resend of an unwritten request | `a_connection_the_upstream_closed_while_idle_is_not_used`, `bytes_left_over_from_a_response_are_never_taken_for_the_next_one` |
| A connection pooled whatever state its response was in (X) | `a_client_that_disconnects_mid_download_from_a_stalled_upstream_leaves_nothing_behind`, `a_client_that_disconnects_mid_download_from_a_fast_upstream_leaves_nothing_behind` |

Three mutations turn no test red on their own, and each is backed by something else rather than missed. Without the poll only where a connection is taken out: hyper reads an idle connection before it writes on it and hands back a request it never wrote, which then goes again (ADR 0003); taking away that resend as well is caught above. Without the readiness check when a connection is put back: the same check when it is taken out finds what it would have. Putting a connection back whether or not its body ended: hyper is ready for another request only after a whole exchange, so the body's own check only saves a poll. The resend of an unwritten request, taken away alone, is caught only by its unit test; no end-to-end test can time a close that finely.

## Order of work inside a milestone

1. Write the exit test first, and see it fail.
2. Implement until it passes.
3. Record any decision that is hard to reverse as an ADR.

## After 0.1

Not planned, listed so they are not forgotten: WebSocket and CONNECT upgrades, HTTP/2 to upstreams, DNS-based upstream discovery, thread-per-core runtime if the benchmarks point there, certificate reload from disk without a config change.
