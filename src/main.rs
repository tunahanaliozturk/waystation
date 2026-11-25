//! The waystation binary: listens on one address and forwards every request
//! to one upstream.
//!
//! ```text
//! waystation --upstream 127.0.0.1:9000 [--listen 127.0.0.1:8080]
//! ```
//!
//! Routing, several upstreams and a configuration file arrive in milestone M2.

use std::io::{self, Write as _};
use std::net::SocketAddr;
use std::process::ExitCode;

use tokio::net::TcpListener;
use waystation::Config;

const USAGE: &str = "usage: waystation --upstream ADDR [--listen ADDR]";
const DEFAULT_LISTEN: SocketAddr = SocketAddr::V4(std::net::SocketAddrV4::new(
    std::net::Ipv4Addr::LOCALHOST,
    8080,
));

#[derive(Debug, PartialEq, Eq)]
struct Args {
    listen: SocketAddr,
    upstream: SocketAddr,
}

impl Args {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut listen = None;
        let mut upstream = None;
        while let Some(flag) = args.next() {
            let slot = match flag.as_str() {
                "--listen" => &mut listen,
                "--upstream" => &mut upstream,
                _ => return Err(format!("unknown argument {flag}")),
            };
            let value = args
                .next()
                .ok_or_else(|| format!("{flag} needs an address"))?;
            let address = value
                .parse()
                .map_err(|error| format!("{flag} {value}: {error}"))?;
            *slot = Some(address);
        }
        Ok(Self {
            listen: listen.unwrap_or(DEFAULT_LISTEN),
            upstream: upstream.ok_or("--upstream is required")?,
        })
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(error) => return fail(&format!("{error}\n{USAGE}"), ExitCode::from(2)),
    };
    let listener = match TcpListener::bind(args.listen).await {
        Ok(listener) => listener,
        Err(error) => {
            return fail(
                &format!("cannot listen on {}: {error}", args.listen),
                ExitCode::FAILURE,
            );
        }
    };
    // Graceful shutdown arrives in milestone M5. Until then Ctrl-C drops
    // every connection at once.
    tokio::select! {
        never = waystation::serve(listener, Config::new(args.upstream)) => match never {},
        signal = tokio::signal::ctrl_c() => match signal {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(&format!("cannot wait for Ctrl-C: {error}"), ExitCode::FAILURE),
        },
    }
}

fn fail(message: &str, code: ExitCode) -> ExitCode {
    // If stderr is gone too, the exit code is all that is left to say it.
    let _ = writeln!(io::stderr(), "waystation: {message}");
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        Args::parse(args.iter().map(ToString::to_string))
    }

    #[test]
    fn arguments_are_read_in_any_order_and_listen_has_a_default() {
        let upstream: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        assert_eq!(
            parse(&["--upstream", "127.0.0.1:9000"]).unwrap(),
            Args {
                listen: DEFAULT_LISTEN,
                upstream
            }
        );
        assert_eq!(
            parse(&["--listen", "0.0.0.0:80", "--upstream", "127.0.0.1:9000"]).unwrap(),
            Args {
                listen: "0.0.0.0:80".parse().unwrap(),
                upstream
            }
        );
    }

    #[test]
    fn bad_arguments_are_refused() {
        for args in [
            &[][..],
            &["--upstream"],
            &["--upstream", "nowhere"],
            &["--port", "1"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }
}
