# Changelog

All notable changes are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project follows [Semantic Versioning](https://semver.org/).

## Unreleased

- Repository scaffold, design document and roadmap.
- Forwarding (M1): HTTP/1.1 to one upstream with streaming bodies, hop-by-hop and forwarding headers, connect, request head, idle, response head and whole-request timeouts, a limit on head size, and cancellation when the client goes away.
- A request with both `Content-Length` and `Transfer-Encoding` gets 400 instead of being forwarded.
- An HTTP/1.1 request in absolute form without `Host`, and a `Host` whose brackets do not hold an IPv6 address, get 400.
- Any transfer coding other than `chunked` alone gets 501 in a request and 502 in an upstream response, instead of reaching the far side undeclared.
- The whole-request deadline also ends a request whose client stops reading the response.
