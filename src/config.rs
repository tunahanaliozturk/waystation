//! Settings: where to listen, where requests go, and the limits and timeouts
//! on the way.
//!
//! A configuration file is read and checked as a whole before anything uses
//! it. The first problem found rejects the file, with the line it is on.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use toml::Spanned;

use crate::balance::{Group, Policy};
use crate::pool::PoolSettings;
use crate::route::{Host, Route, Router, path_is_valid};

/// Where a listener forwards to, with the limits and timeouts that apply.
///
/// Every timeout has a default and none of them is infinite. A clone shares
/// the upstream groups, their balancing state and their idle connections.
#[derive(Debug, Clone)]
pub struct Config {
    pub(crate) listen: SocketAddr,
    pub(crate) router: Router,
    pub(crate) groups: Vec<Arc<Group>>,
    pub(crate) connect_timeout: Duration,
    pub(crate) request_head_timeout: Duration,
    pub(crate) response_head_timeout: Duration,
    pub(crate) idle_timeout: Duration,
    pub(crate) request_timeout: Duration,
    pub(crate) max_header_bytes: usize,
}

const DEFAULT_LISTEN: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);

impl Config {
    /// Forwards every request to `upstream`, with the default limits and
    /// timeouts, and listens on 127.0.0.1:8080.
    #[must_use]
    pub fn new(upstream: SocketAddr) -> Self {
        let group =
            Group::new(Policy::default(), &[(upstream, 1)], PoolSettings::default()).map(Arc::new);
        let routes = group.iter().map(|group| Route {
            host: Host::Any,
            path: "/".to_owned(),
            group: Arc::clone(group),
        });
        Self::with_routes(
            DEFAULT_LISTEN,
            routes.collect(),
            group.into_iter().collect(),
        )
    }

    fn with_routes(listen: SocketAddr, routes: Vec<Route>, groups: Vec<Arc<Group>>) -> Self {
        Self {
            listen,
            router: Router::new(routes),
            groups,
            connect_timeout: Duration::from_secs(5),
            request_head_timeout: Duration::from_secs(10),
            response_head_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(60),
            request_timeout: Duration::from_secs(300),
            max_header_bytes: 32 * 1024,
        }
    }

    /// Reads a configuration file. The format is described in
    /// `docs/design.md`.
    ///
    /// # Errors
    ///
    /// Returns the first problem in the file, with its line and column:
    /// TOML that does not parse, a key that is not known, a value of the
    /// wrong type or out of range, a route to a group that does not exist,
    /// and the rest of the checks listed in the design.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let file: File = toml::from_str(text)
            .map_err(|error| ConfigError::at(text, error.span(), error.message()))?;
        file.check(text)
    }

    /// The address to listen on.
    #[must_use]
    pub fn listen(&self) -> SocketAddr {
        self.listen
    }

    /// Time allowed to open the TCP connection to the upstream. A connection
    /// that is not open in time gets 504. Default 5 seconds.
    #[must_use]
    pub fn with_connect_timeout(self, timeout: Duration) -> Self {
        Self {
            connect_timeout: timeout,
            ..self
        }
    }

    /// Time allowed for a client to send a whole request head, counted from
    /// when the proxy starts waiting for it, so it also closes idle keep-alive
    /// connections. Default 10 seconds.
    #[must_use]
    pub fn with_request_head_timeout(self, timeout: Duration) -> Self {
        Self {
            request_head_timeout: timeout,
            ..self
        }
    }

    /// Time allowed for the upstream to send its response head, counted from
    /// when the last byte of the request body was handed to it. An upload of
    /// any length does not use up this time. Default 30 seconds.
    #[must_use]
    pub fn with_response_head_timeout(self, timeout: Duration) -> Self {
        Self {
            response_head_timeout: timeout,
            ..self
        }
    }

    /// Longest wait for the next chunk of a body, in either direction. A
    /// client that stalls mid-upload gets 408; an upstream that stalls
    /// mid-response has the response cut off. Default 60 seconds.
    #[must_use]
    pub fn with_idle_timeout(self, timeout: Duration) -> Self {
        Self {
            idle_timeout: timeout,
            ..self
        }
    }

    /// Time allowed for a whole request, from its head arriving to the last
    /// byte of the response. Default 300 seconds.
    #[must_use]
    pub fn with_request_timeout(self, timeout: Duration) -> Self {
        Self {
            request_timeout: timeout,
            ..self
        }
    }

    /// Largest request or response head, start line included, in bytes. A
    /// larger request head gets 431; a larger response head gets 502. hyper's
    /// read buffer (about 400 KiB) caps this whatever the setting. Default
    /// 32 KiB.
    #[must_use]
    pub fn with_max_header_bytes(self, bytes: usize) -> Self {
        Self {
            max_header_bytes: bytes,
            ..self
        }
    }
}

/// Why a configuration file was rejected, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConfigError {
    line: usize,
    column: usize,
    message: String,
}

impl ConfigError {
    /// The error at byte offset `span.start` of `text`, or at the start of
    /// the file when there is no span.
    fn at(text: &str, span: Option<Range<usize>>, message: impl Into<String>) -> Self {
        let offset = span.map_or(0, |span| span.start);
        let before = text.get(..offset).unwrap_or(text);
        let line_start = before.rfind('\n').map_or(0, |at| at + 1);
        Self {
            line: before.matches('\n').count() + 1,
            column: before.get(line_start..).unwrap_or_default().chars().count() + 1,
            message: message.into(),
        }
    }

    /// The line the problem is on, counting from 1.
    #[must_use]
    pub fn line(&self) -> usize {
        self.line
    }

    /// The column the problem starts at, counting characters from 1.
    #[must_use]
    pub fn column(&self) -> usize {
        self.column
    }

    /// What is wrong, without the position.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl Error for ConfigError {}

/// The file as written, before it is checked.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    listen: Option<Spanned<String>>,
    #[serde(default)]
    timeouts: Timeouts,
    #[serde(default)]
    limits: Limits,
    #[serde(default)]
    pool: PoolFile,
    #[serde(default)]
    upstreams: BTreeMap<String, GroupFile>,
    #[serde(default)]
    routes: Vec<Spanned<RouteFile>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Timeouts {
    connect: Option<Spanned<String>>,
    request_head: Option<Spanned<String>>,
    response_head: Option<Spanned<String>>,
    idle: Option<Spanned<String>>,
    request: Option<Spanned<String>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Limits {
    max_header_bytes: Option<Spanned<usize>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PoolFile {
    idle_timeout: Option<Spanned<String>>,
    max_idle_per_server: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupFile {
    #[serde(default)]
    balance: Policy,
    servers: Spanned<Vec<ServerFile>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerFile {
    address: Spanned<String>,
    weight: Option<Spanned<u32>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteFile {
    host: Option<Spanned<String>>,
    path: Option<Spanned<String>>,
    upstream: Spanned<String>,
}

/// The smallest head limit allowed: room for a request line, a `Host` and a
/// few ordinary headers.
const MIN_HEADER_BYTES: usize = 1024;

impl File {
    fn check(self, text: &str) -> Result<Config, ConfigError> {
        let check = Check(text);
        let listen = match &self.listen {
            Some(listen) => {
                address(listen).map_err(|message| check.error(listen.span(), message))?
            }
            None => DEFAULT_LISTEN,
        };
        let defaults = PoolSettings::default();
        let pool = PoolSettings {
            max_idle: self.pool.max_idle_per_server.unwrap_or(defaults.max_idle),
            idle_timeout: check
                .duration_or(self.pool.idle_timeout.as_ref(), defaults.idle_timeout)?,
        };
        let groups = check.groups(&self.upstreams, pool, listen)?;
        let routes = check.routes(&self.routes, &groups)?;

        let mut config = Config::with_routes(listen, routes, groups.into_values().collect());
        let timeouts = &self.timeouts;
        config.connect_timeout =
            check.duration_or(timeouts.connect.as_ref(), config.connect_timeout)?;
        config.request_head_timeout =
            check.duration_or(timeouts.request_head.as_ref(), config.request_head_timeout)?;
        config.response_head_timeout = check.duration_or(
            timeouts.response_head.as_ref(),
            config.response_head_timeout,
        )?;
        config.idle_timeout = check.duration_or(timeouts.idle.as_ref(), config.idle_timeout)?;
        config.request_timeout =
            check.duration_or(timeouts.request.as_ref(), config.request_timeout)?;
        if let Some(bytes) = &self.limits.max_header_bytes {
            if *bytes.get_ref() < MIN_HEADER_BYTES {
                return Err(check.error(
                    bytes.span(),
                    format!("max_header_bytes is at least {MIN_HEADER_BYTES}"),
                ));
            }
            config.max_header_bytes = *bytes.get_ref();
        }
        Ok(config)
    }
}

/// The checks on a file, which need its text to say where a problem is.
struct Check<'a>(&'a str);

impl Check<'_> {
    fn error(&self, span: Range<usize>, message: String) -> ConfigError {
        ConfigError::at(self.0, Some(span), message)
    }

    fn duration_or(
        &self,
        value: Option<&Spanned<String>>,
        default: Duration,
    ) -> Result<Duration, ConfigError> {
        let Some(value) = value else {
            return Ok(default);
        };
        duration(value.get_ref()).ok_or_else(|| {
            self.error(
                value.span(),
                format!(
                    "`{}` is not a duration above zero, such as `300ms`, `5s` or `2m`",
                    value.get_ref()
                ),
            )
        })
    }

    fn groups<'g>(
        &self,
        upstreams: &'g BTreeMap<String, GroupFile>,
        pool: PoolSettings,
        listen: SocketAddr,
    ) -> Result<BTreeMap<&'g str, Arc<Group>>, ConfigError> {
        let mut groups = BTreeMap::new();
        for (name, group) in upstreams {
            let mut servers: Vec<(SocketAddr, u32)> = Vec::new();
            for server in group.servers.get_ref() {
                let addr = address(&server.address)
                    .map_err(|message| self.error(server.address.span(), message))?;
                if is_listener(addr, listen) {
                    return Err(self.error(
                        server.address.span(),
                        format!(
                            "server {addr} is the address this proxy listens on, so its requests would come back to it"
                        ),
                    ));
                }
                if servers.iter().any(|&(seen, _)| seen == addr) {
                    return Err(self.error(
                        server.address.span(),
                        format!("server {addr} is listed twice in upstream group `{name}`"),
                    ));
                }
                let weight = server.weight.as_ref().map_or(1, |weight| *weight.get_ref());
                if weight == 0 {
                    let span = server.weight.as_ref().map_or(0..0, Spanned::span);
                    return Err(self.error(span, "a weight is at least 1".to_owned()));
                }
                servers.push((addr, weight));
            }
            let built = Group::new(group.balance, &servers, pool).ok_or_else(|| {
                let message = if servers.is_empty() {
                    format!("upstream group `{name}` has no servers")
                } else {
                    format!(
                        "the weights in upstream group `{name}` add up to more than {}",
                        u64::MAX
                    )
                };
                self.error(group.servers.span(), message)
            })?;
            groups.insert(name.as_str(), Arc::new(built));
        }
        Ok(groups)
    }

    fn routes(
        &self,
        routes: &[Spanned<RouteFile>],
        groups: &BTreeMap<&str, Arc<Group>>,
    ) -> Result<Vec<Route>, ConfigError> {
        let mut checked = Vec::new();
        let mut seen: BTreeMap<(Host, String), usize> = BTreeMap::new();
        for spanned in routes {
            let route = spanned.get_ref();
            let host = match &route.host {
                Some(host) => Host::parse(host.get_ref()).ok_or_else(|| {
                    self.error(
                        host.span(),
                        format!(
                            "`{}` is not a host name, an IP address or `*.` and a name; a route host has no port",
                            host.get_ref()
                        ),
                    )
                })?,
                None => Host::Any,
            };
            let path = match &route.path {
                Some(path) if !path_is_valid(path.get_ref()) => {
                    return Err(self.error(
                        path.span(),
                        format!(
                            "`{}` is not a route path: it starts with `/` and has no query, fragment or spaces",
                            path.get_ref()
                        ),
                    ));
                }
                Some(path) => path.get_ref().clone(),
                None => "/".to_owned(),
            };
            let name = route.upstream.get_ref();
            let group = groups.get(name.as_str()).ok_or_else(|| {
                self.error(
                    route.upstream.span(),
                    format!("there is no upstream group `{name}`"),
                )
            })?;
            let line = self.error(spanned.span(), String::new()).line;
            if let Some(first) = seen.insert((host.clone(), path.clone()), line) {
                return Err(self.error(
                    route
                        .host
                        .as_ref()
                        .map_or(route.upstream.span(), Spanned::span),
                    format!("the route for this host and `{path}` is already on line {first}"),
                ));
            }
            checked.push(Route {
                host,
                path,
                group: Arc::clone(group),
            });
        }
        if checked.is_empty() {
            return Err(ConfigError::at(
                self.0,
                None,
                "there are no [[routes]], so every request would get 404",
            ));
        }
        Ok(checked)
    }
}

/// An IP address and port, or what is wrong with the text.
fn address(text: &Spanned<String>) -> Result<SocketAddr, String> {
    text.get_ref().parse().map_err(|_| {
        format!(
            "`{}` is not an IP address and port such as `127.0.0.1:8080` or `[::1]:8080`; host names are not supported yet",
            text.get_ref()
        )
    })
}

/// Whether a connection to `server` would reach the proxy's own listener
/// on `listen`: the same address, or the same port on the loopback or
/// unspecified address of a listener on every address of that family.
/// Other addresses of this machine are not known here.
fn is_listener(server: SocketAddr, listen: SocketAddr) -> bool {
    let local = server.ip().is_loopback() || server.ip().is_unspecified();
    server == listen
        || (listen.ip().is_unspecified()
            && server.port() == listen.port()
            && server.is_ipv4() == listen.is_ipv4()
            && local)
}

/// A duration written as a whole number and a unit: `ms`, `s` or `m`. Zero
/// is refused, since a timeout of zero would fail everything at once.
fn duration(text: &str) -> Option<Duration> {
    let digits = text.find(|c: char| !c.is_ascii_digit())?;
    let (number, unit) = text.split_at(digits);
    let number: u64 = number.parse().ok()?;
    let duration = match unit {
        "ms" => Duration::from_millis(number),
        "s" => Duration::from_secs(number),
        "m" => Duration::from_secs(number.checked_mul(60)?),
        _ => return None,
    };
    (!duration.is_zero()).then_some(duration)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_take_a_whole_number_and_a_unit() {
        assert_eq!(duration("300ms"), Some(Duration::from_millis(300)));
        assert_eq!(duration("5s"), Some(Duration::from_secs(5)));
        assert_eq!(duration("2m"), Some(Duration::from_secs(120)));
        for bad in [
            "",
            "5",
            "s",
            "0s",
            "1.5s",
            "-1s",
            "5 s",
            "5h",
            "99999999999999999999s",
        ] {
            assert_eq!(duration(bad), None, "{bad}");
        }
    }

    #[test]
    fn an_error_position_counts_lines_and_characters_from_one() {
        let text = "a = 1\nbé = 2\n";
        let error = ConfigError::at(text, Some(10..11), "x");
        assert_eq!((error.line(), error.column()), (2, 4));
        assert_eq!(error.to_string(), "line 2, column 4: x");
        assert_eq!(ConfigError::at(text, None, "x").line(), 1);
    }
}
