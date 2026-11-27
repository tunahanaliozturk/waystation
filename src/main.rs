//! The waystation binary: reads a configuration file, listens where it says
//! and forwards requests by its routes.
//!
//! ```text
//! waystation --config waystation.toml
//! ```
//!
//! The file format is in `docs/design.md`. Reloading it without a restart
//! arrives in milestone M5.

use std::io::{self, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use tokio::net::TcpListener;
use waystation::Config;

const USAGE: &str = "usage: waystation --config FILE";

#[derive(Debug, PartialEq, Eq)]
struct Args {
    config: PathBuf,
}

impl Args {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        match (args.next().as_deref(), args.next(), args.next()) {
            (Some("--config"), Some(path), None) => Ok(Self {
                config: PathBuf::from(path),
            }),
            (Some("--config"), None, _) => Err("--config needs a file".to_owned()),
            (Some(flag), ..) => Err(format!("unexpected argument {flag}")),
            (None, ..) => Err("--config is required".to_owned()),
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(error) => return fail(&format!("{error}\n{USAGE}"), ExitCode::from(2)),
    };
    let path = args.config.display();
    let text = match std::fs::read_to_string(&args.config) {
        Ok(text) => text,
        Err(error) => return fail(&format!("cannot read {path}: {error}"), ExitCode::FAILURE),
    };
    let config = match Config::from_toml(&text) {
        Ok(config) => config,
        Err(error) => return fail(&format!("{path}: {error}"), ExitCode::FAILURE),
    };
    let listener = match TcpListener::bind(config.listen()).await {
        Ok(listener) => listener,
        Err(error) => {
            return fail(
                &format!("cannot listen on {}: {error}", config.listen()),
                ExitCode::FAILURE,
            );
        }
    };
    // Graceful shutdown arrives in milestone M5. Until then Ctrl-C drops
    // every connection at once.
    tokio::select! {
        never = waystation::serve(listener, config) => match never {},
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
    fn the_configuration_file_is_the_one_argument() {
        assert_eq!(
            parse(&["--config", "waystation.toml"]).unwrap(),
            Args {
                config: PathBuf::from("waystation.toml")
            }
        );
    }

    #[test]
    fn bad_arguments_are_refused() {
        for args in [
            &[][..],
            &["--config"],
            &["--upstream", "127.0.0.1:9000"],
            &["--config", "a.toml", "b.toml"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }
}
