# menzil

An HTTP reverse proxy and load balancer for Rust, built on tokio, hyper and rustls.

A *menzil* was a relay station on the Ottoman post roads: a courier arrived, changed horses and the message went on. A proxy does the same with requests.

## Status

Planning. The repository holds the design and the roadmap; no functional code yet.

- [docs/design.md](docs/design.md): what it does, what it does not do, and how each promise will be tested.
- [docs/roadmap.md](docs/roadmap.md): milestones, each with an exit test.
- [docs/adr/](docs/adr/): decisions that are hard to reverse.

## What it will promise

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
