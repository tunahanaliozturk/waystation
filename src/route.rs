//! Choosing the upstream group for a request by its host and path.
//!
//! The host decides first: routes for the exact host, then for the longest
//! wildcard that matches it, then routes for any host. Among the routes for
//! that one host, the longest path prefix wins. A host with routes of its
//! own never falls through to the routes for any host, so what a host serves
//! can be read off the routes written for it, as with nginx server blocks.

use std::sync::Arc;

use crate::balance::Group;

/// Every route of a configuration.
#[derive(Debug, Clone)]
pub(crate) struct Router {
    routes: Vec<Route>,
}

/// One route: which requests it takes and where they go.
#[derive(Debug, Clone)]
pub(crate) struct Route {
    pub(crate) host: Host,
    pub(crate) path: String,
    pub(crate) group: Arc<Group>,
}

/// The hosts a route takes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Host {
    /// Every host, and requests with none.
    Any,
    /// Every name ending in this suffix, which starts with a dot.
    Wildcard(String),
    /// This name only, in lower case.
    Exact(String),
}

impl Host {
    /// Reads a host as a route gives it: a name, an IP address or
    /// `*.suffix`, without a port. Returns `None` for anything else.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let text = text.to_ascii_lowercase();
        if let Some(suffix) = text.strip_prefix("*.") {
            return is_name(suffix).then(|| Self::Wildcard(format!(".{suffix}")));
        }
        let literal = text
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
            .is_some_and(|address| address.parse::<std::net::Ipv6Addr>().is_ok());
        (literal || is_name(&text)).then_some(Self::Exact(text))
    }

    /// How well this matches `host`, which is in lower case without a port.
    /// Higher is better; `None` is no match at all.
    fn rank(&self, host: Option<&str>) -> Option<(u8, usize)> {
        match (self, host) {
            (Self::Any, _) => Some((0, 0)),
            (Self::Wildcard(suffix), Some(host)) => host
                .strip_suffix(suffix.as_str())
                .is_some_and(|label| !label.is_empty())
                .then_some((1, suffix.len())),
            (Self::Exact(name), Some(host)) => (name == host).then_some((2, 0)),
            (Self::Wildcard(_) | Self::Exact(_), None) => None,
        }
    }
}

/// Whether `text` is a host name or an IPv4 address: dot-separated labels of
/// letters, digits, hyphens and underscores, none empty. DNS names have no
/// underscores, but the names of containers and internal services often do,
/// and clients send them.
fn is_name(text: &str) -> bool {
    !text.is_empty()
        && text.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
}

/// Whether a route path is one waystation can match: it starts with a slash
/// and holds no query, fragment, space or control character.
pub(crate) fn path_is_valid(path: &str) -> bool {
    path.starts_with('/')
        && !path.bytes().any(|byte| {
            matches!(byte, b'?' | b'#') || byte.is_ascii_whitespace() || byte.is_ascii_control()
        })
}

impl Router {
    pub(crate) fn new(routes: Vec<Route>) -> Self {
        Self { routes }
    }

    /// The group for a request to `host` (as the client sent it, port and
    /// all; `None` for an HTTP/1.0 request without one) and `path`.
    // ponytail: a linear scan over every route per request; a map by host
    // would do if configurations grow to thousands of routes.
    pub(crate) fn find(&self, host: Option<&str>, path: &str) -> Option<&Arc<Group>> {
        let host = host.map(normalize);
        let host = host.as_deref();
        let best = self
            .routes
            .iter()
            .filter_map(|route| route.host.rank(host))
            .max()?;
        self.routes
            .iter()
            .filter(|route| {
                route.host.rank(host) == Some(best) && prefix_matches(&route.path, path)
            })
            .max_by_key(|route| route.path.len())
            .map(|route| &route.group)
    }
}

/// A host as routes compare it: lower case, without the port and without a
/// trailing dot.
fn normalize(host: &str) -> String {
    let name = match host.find(']') {
        Some(end) if host.starts_with('[') => host.get(..=end).unwrap_or(host),
        _ => host.split(':').next().unwrap_or(host),
    };
    name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase()
}

/// Whether `path` falls under `prefix`, on a segment boundary: `/api` takes
/// `/api` and `/api/users` but not `/apis`.
fn prefix_matches(prefix: &str, path: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || prefix.ends_with('/') || rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::balance::Policy;
    use crate::pool::PoolSettings;
    use std::net::SocketAddr;

    /// A router whose groups are told apart by the port of their one server.
    fn router(routes: &[(&str, &str, u16)]) -> Router {
        Router::new(
            routes
                .iter()
                .map(|&(host, path, port)| Route {
                    host: if host.is_empty() {
                        Host::Any
                    } else {
                        Host::parse(host).unwrap()
                    },
                    path: path.to_owned(),
                    group: Arc::new(
                        Group::new(
                            Policy::RoundRobin,
                            &[(SocketAddr::from(([127, 0, 0, 1], port)), 1)],
                            PoolSettings::default(),
                        )
                        .unwrap(),
                    ),
                })
                .collect(),
        )
    }

    fn port(router: &Router, host: Option<&str>, path: &str) -> Option<u16> {
        router
            .find(host, path)
            .map(|group| group.servers()[0].addr.port())
    }

    #[test]
    fn an_exact_host_beats_a_wildcard_and_a_wildcard_beats_any_host() {
        let router = router(&[
            ("", "/", 1),
            ("*.example.test", "/", 2),
            ("*.api.example.test", "/", 3),
            ("www.example.test", "/", 4),
        ]);
        assert_eq!(port(&router, Some("www.example.test"), "/"), Some(4));
        assert_eq!(port(&router, Some("WWW.Example.Test:8080"), "/"), Some(4));
        assert_eq!(port(&router, Some("www.example.test."), "/"), Some(4));
        assert_eq!(port(&router, Some("img.example.test"), "/"), Some(2));
        assert_eq!(port(&router, Some("v1.api.example.test"), "/"), Some(3));
        // A wildcard needs at least one label in front of it.
        assert_eq!(port(&router, Some("example.test"), "/"), Some(1));
        assert_eq!(port(&router, Some("other.test"), "/"), Some(1));
        assert_eq!(port(&router, None, "/"), Some(1));
    }

    #[test]
    fn the_longest_prefix_wins_on_a_segment_boundary() {
        let router = router(&[("", "/", 1), ("", "/api", 2), ("", "/api/v2/", 3)]);
        assert_eq!(port(&router, None, "/api"), Some(2));
        assert_eq!(port(&router, None, "/api/users"), Some(2));
        assert_eq!(port(&router, None, "/apis"), Some(1));
        assert_eq!(port(&router, None, "/api/v2/x"), Some(3));
        assert_eq!(port(&router, None, "/api/v2"), Some(2));
        assert_eq!(port(&router, None, "/"), Some(1));
    }

    #[test]
    fn a_host_with_routes_of_its_own_does_not_fall_through() {
        let router = router(&[("", "/", 1), ("a.test", "/admin", 2)]);
        assert_eq!(port(&router, Some("a.test"), "/admin/x"), Some(2));
        assert_eq!(port(&router, Some("a.test"), "/"), None);
        assert_eq!(port(&router, Some("b.test"), "/"), Some(1));
    }

    #[test]
    fn an_ipv6_host_is_matched_without_its_port() {
        let router = router(&[("[::1]", "/", 1)]);
        assert_eq!(port(&router, Some("[::1]:8080"), "/"), Some(1));
        assert_eq!(port(&router, Some("[::2]"), "/"), None);
    }

    #[test]
    fn route_hosts_and_paths_are_checked() {
        for bad in [
            "",
            "*",
            "*.",
            "a..test",
            "a.test:80",
            "a b.test",
            "x.*.test",
            "[zz]",
        ] {
            assert_eq!(Host::parse(bad), None, "{bad}");
        }
        assert_eq!(
            Host::parse("*.Example.test"),
            Some(Host::Wildcard(".example.test".into()))
        );
        assert_eq!(
            Host::parse("my_service.internal"),
            Some(Host::Exact("my_service.internal".into()))
        );
        assert_eq!(
            Host::parse("192.0.2.1"),
            Some(Host::Exact("192.0.2.1".into()))
        );
        for bad in ["", "api", "/a?b", "/a#b", "/a b"] {
            assert!(!path_is_valid(bad), "{bad}");
        }
        assert!(path_is_valid("/a/b/"));
    }
}
