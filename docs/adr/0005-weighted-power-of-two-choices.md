# 0005: Weights in both balancing policies, and how they combine with load

Status: accepted, 2026-11-25

## Context

The design asks for round robin and for least requests with the power of two choices, both with weights. Round robin with weights has a well-known form. The power of two choices with weights does not: the textbook version draws two servers uniformly and takes the one with fewer requests in flight, which ignores weights entirely. Envoy weights its least-request balancer by scheduling on weight divided by load, which needs a shared priority queue, and that is what the power of two choices is meant to avoid.

## Decision

**Round robin is smooth weighted round robin, as nginx does it.** Each pick adds every server's weight to its current value, takes the server with the highest value and takes the total weight off it. Weights 1, 2 and 5 give every cycle of eight requests in the proportions 1, 2 and 5, spread out rather than in runs. The current values sit behind one lock per group.

**Least requests draws two servers in proportion to their weights and takes the one with fewer requests in flight per unit of weight**, comparing `in_flight(a) × weight(b)` with `in_flight(b) × weight(a)` so nothing is divided. On a tie the first draw wins. The draw uses SplitMix64 on one atomic counter per group, seeded from the standard library's random hasher keys, so a pick takes no lock.

A request counts as in flight from the pick until its response body is dropped, finished or not.

Weights are whole numbers from 1 up. There is no weight of zero; a server that should get no traffic is left out of the file.

## Consequences

- With no load, or with servers that are alike, least requests follows the weights: the draws follow them, and the load per unit of weight evens out, so ties are the common case. The exit test checks this over 100,000 requests through the proxy, within 1% of all requests per server, and checks round robin to the request.
- Under uneven load, a slow server collects requests in flight and loses the comparisons, which is the point of the policy. A unit test pins that: a server with requests in flight against an idle one of the same weight gets about a quarter of new requests, the chance that both draws land on it.
- Round robin takes a lock per pick. If that shows in the benchmarks of M8, a precomputed schedule and an atomic index give the same order without one.
- The random numbers spread load; they are not meant to be unpredictable. Someone who could predict them could only choose which healthy server gets a request.
