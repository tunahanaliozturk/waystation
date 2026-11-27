# 0004: Routes match the host first, then the longest path prefix

Status: accepted, 2026-11-25

## Context

A route names a host (or none), a path prefix and an upstream group. Two routes can both match a request, one by a better host and one by a longer path, and the proxy has to say which wins in a way a person can predict from the file.

There are two common answers. nginx picks a server block by host and then a location inside it, so a host never falls through to another host's locations. Some proxies score host and path together, so a longer path under a wildcard host can beat a shorter one under the exact host.

## Decision

The host decides first, and the path only among the routes for that one host.

1. Routes for the exact host, compared without case, port or a trailing dot.
2. Otherwise the routes of the longest wildcard that matches. `*.example.test` matches `a.example.test` and `a.b.example.test`, but not `example.test`.
3. Otherwise the routes with no host, which also take HTTP/1.0 requests that name none.

Among those routes the longest path prefix wins. A prefix matches on a segment boundary: `/api` takes `/api` and `/api/users` but not `/apis`, and `/api/` takes everything under it. If none of them matches, the answer is 404, even when a route with no host would have matched.

The path is matched as the client sent it, without decoding percent escapes or removing dot segments.

## Consequences

- What a host serves can be read off the routes written for it. Adding a route for `api.example.test` never changes where requests to other hosts go, and a route with no host never catches requests that a host's own routes do not.
- A host that should also get a catch-all needs a route with path `/` of its own.
- Matching the raw path means `/public/../admin` goes to the group for `/public`, and its upstream may resolve it to `/admin`. Since a route only chooses where a request goes and grants nothing, this moves a request between groups but not past a check. Normalising or refusing such paths belongs to the hardening of M7, with the other URI limits.
- Routes are scanned in a list for each request. That is fine for tens of routes; a map by host can replace it without changing what matches.
- The rules have unit tests in `src/route.rs` and an end-to-end test through the proxy (`tests/proxy/routing.rs`).
