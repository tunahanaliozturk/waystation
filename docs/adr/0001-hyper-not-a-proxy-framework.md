# 0001: Build on hyper and tokio, not on a proxy framework

Status: accepted, 2026-11-17

## Context

There are three levels at which a reverse proxy can be written in Rust:

1. On a proxy framework such as Pingora, which already provides connection pooling, load balancing, health checks, graceful restart and the request lifecycle.
2. On an HTTP library (hyper) and a runtime (tokio), writing the proxy logic around them.
3. On raw sockets, writing the HTTP/1.1 and HTTP/2 implementations as well.

The project exists to show how the hard parts of a proxy are designed and tested: backpressure, cancellation, pooling, reload without dropped requests, and refusing ambiguous requests.

## Decision

Level 2. hyper 1.x for HTTP/1.1 and HTTP/2, tokio for the runtime, rustls for TLS. Pooling, routing, balancing, health checking, retries, reload and drain are written in this repository.

## Consequences

- The parts the project wants to show are its own code, with its own tests.
- HTTP parsing and HTTP/2 framing come from hyper, which is widely deployed and fuzzed. Writing those again would add months and mostly produce bugs that are already fixed there.
- Smuggling defence depends partly on how hyper parses. menzil adds its own checks on the parsed head and tests the combined behaviour with raw bytes on a socket, so a change in hyper that loosens parsing is caught by the suite.
- The upstream connection pool is written here, not taken from hyper-util's legacy client, because the pool has to know about health state, draining and reload.
- No custom protocol work means no way to beat a hand-tuned C proxy on parsing speed. The benchmarks report where menzil stands against nginx and Envoy either way.
