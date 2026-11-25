//! End-to-end tests: a client, the proxy and an upstream, all in this process
//! on 127.0.0.1.

// `cfg(test)` is always set here. It is written out so that clippy treats the
// helpers in these modules as test code, where `unwrap` is allowed.
#[cfg(test)]
mod disconnect;
#[cfg(test)]
mod faults;
#[cfg(test)]
mod forwarding;
#[cfg(test)]
mod support;
