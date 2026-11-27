# 0003: When an upstream connection is reused, and when a request is sent again

Status: accepted, 2026-11-25

## Context

From M2 on, a connection to an upstream server outlives the request that opened it. Three things can then go wrong that could not before.

1. **Bytes left over.** A connection carries whatever the last exchange left on it. If the response was not read to its end, or the upstream sent more than it promised (a body after a `HEAD` response, more bytes than its `Content-Length`), the next request reads those bytes as the start of its own response. That is response desync: one client gets another's answer.
2. **A connection the upstream closed while it sat idle.** Servers close idle keep-alive connections after a few seconds (Node.js after 5). A request sent on such a connection fails although nothing is wrong with the server.
3. **A connection closed just as a request arrives.** The close and the request cross on the wire. The request may have been read and acted on, or not; the proxy cannot tell. Sending a `POST` again could charge a card twice.

ADR 0002 rules out a task per connection, so nothing watches a connection while it is idle.

## Decision

**A connection goes back to its server's pool only from a response body that ended the way its framing says, and only if hyper then says it is ready for another request.** hyper is ready only between complete exchanges, with both the request and the response finished, and only when keep-alive is allowed (no `Connection: close`, not HTTP/1.0 without keep-alive). The body is where the end is seen (ADR 0002); its own check only saves polling a connection that cannot be ready, and hyper's readiness is the guard.

**An idle connection is polled once, without a waker, when it is put back, when it is taken out, and in a sweep once a second from the accept loop.** That poll lets hyper read what arrived meanwhile. An end of stream finishes the connection. Bytes on a connection with no request outstanding are an error in hyper (it refuses them instead of keeping them for the next response), which also finishes it. A connection that is finished, or idle for longer than the pool's idle timeout, is closed. The pool hands out the most recently used connection first, and keeps at most a configured number per server.

**hyper does the same check again just before it writes a request**: its connection reads before it writes, and a request it never wrote is handed back by `try_send_request`. Such a request goes out again on a new connection, whatever its method, since none of it reached the upstream.

**A request that may have reached the upstream is sent again only if repeating it is harmless:** the connection was reused, no response head arrived, the method is idempotent (RFC 9110 section 9.2.2), and the request has no body, so it can be rebuilt from a copy of its head. Anything else gets 502. The request goes again once, on a new connection to the same server; a new connection that fails says something about the server, and what to do then is the retry policy of M4.

## Consequences

- No response is read from a connection that had bytes left over by the time it was taken from the pool. Bytes that arrive after that check and before the request is written are read as its response, and no HTTP/1.1 client can tell those apart from a real one; the window is the time it takes to write one request head, and only an upstream that breaks the protocol opens it.
- A request on a connection the upstream closed while idle never fails for that reason, whatever its method (`a_connection_the_upstream_closed_while_idle_is_not_used`).
- A close that crosses a `GET` costs a second attempt and nothing more (`a_safe_request_on_a_connection_closed_under_it_is_sent_again_on_a_new_one`). A close that crosses a `POST` gets 502, and the upstream sees the `POST` once (`an_unsafe_request_on_a_connection_closed_under_it_is_not_sent_twice`). That is what nginx does without `proxy_next_upstream non_idempotent` and what Go's `http.Transport` does.
- A request with a body is never sent twice after any byte of it went out, because the body is streamed and not kept. Buffering small bodies to make them repeatable is possible later, and would need its own limit.
- The second attempt gets a fresh response head timeout. The whole-request deadline still bounds both.
- These resends are not the retries of M4: they go to the same server, are not counted against a retry budget, and happen at most once per request.
- The checks overlap with hyper's, on purpose. Polling a connection before it is taken out and hyper's own read before it writes find the same things, and the resend of an unwritten request then takes care of them, so an end-to-end test of a request fails only when both are taken away. Each is still tested on its own. The resend matters alone only when a close lands between the pool's poll and hyper's write, a race no test can produce on purpose, so the decision of what goes again is a function of its own (`again` in `src/server.rs`) with unit tests. The poll matters alone in the sweep: without it a socket the upstream closed stays open until the pool's idle timeout (`an_idle_connection_the_upstream_closed_is_closed_by_the_sweep_long_before_its_timeout`). The checks when a connection is put back and when it is taken out find the same things too, and hyper refuses a request on a connection that is not ready whatever the body said. The roadmap lists these mutations and what catches them. The early poll stays because it is what lets the sweep close sockets the upstream has closed, and it spares a failed attempt.
- An idempotent request with a body, such as a `PUT`, is not sent again after a break: the copy of its head would go out without the body (`an_unsafe_request_on_a_connection_closed_under_it_is_not_sent_twice` covers a `PUT` for that reason).
- Idle connections are swept by the accept loop, so the proxy's runtime still has the accept loop and one task per client connection, and nothing else.
