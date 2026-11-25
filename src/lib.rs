//! waystation: an HTTP reverse proxy and load balancer.
//!
//! The proxy lives in this library and the binary runs it. It is a library so
//! that the integration tests can run the proxy in their own process; it is
//! not a stable API. The design is in `docs/design.md` and the milestones are
//! in `docs/roadmap.md`.

mod body;
mod config;
mod headers;
mod server;

pub use config::Config;
pub use server::serve;
