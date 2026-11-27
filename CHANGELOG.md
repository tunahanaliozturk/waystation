# Changelog

All notable changes are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project follows [Semantic Versioning](https://semver.org/).

## Unreleased

- Repository scaffold, design document and roadmap.
- Forwarding (M1): HTTP/1.1 to one upstream with streaming bodies, hop-by-hop and forwarding headers, connect, request head, idle, response head and whole-request timeouts, a limit on head size, and cancellation when the client goes away.
- A request with both `Content-Length` and `Transfer-Encoding` gets 400 instead of being forwarded.
- An HTTP/1.1 request in absolute form without `Host`, and a `Host` whose brackets do not hold an IPv6 address, get 400.
- Any transfer coding other than `chunked` alone gets 501 in a request and 502 in an upstream response, instead of reaching the far side undeclared.
- The whole-request deadline also ends a request whose client stops reading the response.
- Routing and balancing (M2): a TOML configuration file (`waystation --config FILE`), checked as a whole, with the line and column of the first problem in every error. Routes by exact host, wildcard host and path prefix. Upstream groups with weighted round robin and weighted least requests (power of two choices).
- Upstream connections are kept per server and reused. A connection goes back only after a whole response; one the upstream has closed, or sent stray bytes on, by the time it is taken from the pool is closed, not used (ADR 0003 describes the short window after that check); a request that fails on a reused connection is sent once more on a new one only when that cannot repeat it.
- The command line takes `--config FILE` in place of `--upstream` and `--listen`.
- A body whose next chunk is always ready stops at the request deadline; before, only a body the proxy had to wait for did.
- A configuration file whose upstream server is the proxy's own listen address is rejected, instead of sending each request back into the proxy.
- An upstream group whose weights add up to more than `u64::MAX` is rejected, and round robin no longer cuts a total above `i64::MAX` down to it.
- A request head timeout too long to add to the clock, such as `18446744073709551615s`, is cut down to a year; before, every client connection panicked.
