# 0002: The upstream connection lives in the response body, not on a task

Status: accepted, 2026-11-25

## Context

hyper's client hands back two halves for each connection: a `SendRequest` to send requests on, and a `Connection` future that does the reading and writing and has to be polled for anything to happen. hyper's examples spawn the `Connection` on a task of its own.

A proxy has to stop work for a client that went away. A spawned connection is a task that can outlive the request. hyper does end it once both the request and the response body have been dropped, but only as long as every body on the way reports a broken peer as an error. A body that waits instead, or code that drains the rest of a response to keep the connection, leaves the task and the upstream socket open until a timeout.

The response body is the one thing that lives exactly as long as the exchange with the client. hyper drops it when the response is finished, and also when the client connection fails.

## Decision

The request handler polls the `Connection` itself until the response head arrives, and then moves it into the response body. The body polls the connection each time hyper asks it for a chunk, and drops it at the end.

Cancellation then needs no code of its own. A client that goes away makes hyper drop the request handler, or the response body, and the `Connection` goes with it, which closes the socket to the upstream. There is no task to find and cancel.

Until the connection pools of milestone M2, every request opens a new upstream connection and closes it when the response ends.

## Consequences

- A client that disconnects mid-upload, while waiting for the head, or mid-download leaves no task and no upstream connection behind. Four tests in `tests/proxy/disconnect.rs` check this through the proxy runtime's task count and the upstream's count of open connections, with timeouts set to a minute so that only the disconnect can explain the cleanup.
- Spawning the connection on its own, as hyper's examples do, does not fail those tests: hyper cleans that up too. Spawning it together with a request body that waits out a broken client does fail them, and so does draining the rest of a response after the client left. Both are listed with the mutations in the roadmap.
- The upstream connection is polled only when hyper wants a chunk for the client. That is the backpressure the design asks for: the upstream is not read while the client is slow to take what it already has.
- The pool in M2 has to take a connection back from a body that finished cleanly, and only from one that did. The body is already the place that knows.
- hyper notices a client's end of stream while a response is pending only with half-close off, which is hyper's default. Turning it on brings back leaked requests, and two of the disconnect tests catch that.
