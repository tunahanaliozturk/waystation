//! Requests reach the group their host and path route them to, and a request
//! that no route takes gets 404 and reaches nobody.

use waystation::Config;

use crate::support::{Fault, Proxy, Upstream, exchange};

#[tokio::test(flavor = "multi_thread")]
async fn each_request_reaches_the_group_its_host_and_path_choose() {
    let api = Upstream::start(Fault::Echo).await;
    let wildcard = Upstream::start(Fault::Echo).await;
    let default = Upstream::start(Fault::Echo).await;
    let config = Config::from_toml(&format!(
        r#"
        [upstreams.api]
        servers = [{{ address = "{}" }}]
        [upstreams.wildcard]
        servers = [{{ address = "{}" }}]
        [upstreams.default]
        servers = [{{ address = "{}" }}]

        [[routes]]
        host = "api.example.test"
        path = "/v1"
        upstream = "api"

        [[routes]]
        host = "*.example.test"
        upstream = "wildcard"

        [[routes]]
        upstream = "default"
        "#,
        api.addr, wildcard.addr, default.addr
    ))
    .unwrap();
    let proxy = Proxy::start(config);

    let cases: [(&str, u16, [usize; 3]); 7] = [
        (
            "GET /v1/users HTTP/1.1\r\nhost: API.example.test:8080\r\n",
            200,
            [1, 0, 0],
        ),
        // The host has routes of its own, and none of them takes `/v2`.
        (
            "GET /v2 HTTP/1.1\r\nhost: api.example.test\r\n",
            404,
            [1, 0, 0],
        ),
        // `/v1x` is not under `/v1`.
        (
            "GET /v1x HTTP/1.1\r\nhost: api.example.test\r\n",
            404,
            [1, 0, 0],
        ),
        (
            "GET /v1 HTTP/1.1\r\nhost: www.example.test\r\n",
            200,
            [1, 1, 0],
        ),
        ("GET /v1 HTTP/1.1\r\nhost: example.test\r\n", 200, [1, 1, 1]),
        // An absolute request URI wins over `Host`.
        (
            "GET http://api.example.test/v1 HTTP/1.1\r\nhost: other.test\r\n",
            200,
            [2, 1, 1],
        ),
        // HTTP/1.0 without a host goes to the routes for any host.
        ("GET /v1 HTTP/1.0\r\n", 200, [2, 1, 2]),
    ];
    for (head, status, counts) in cases {
        let request = format!("{head}connection: close\r\n\r\n");
        let reply = exchange(proxy.addr, request.as_bytes()).await;
        assert_eq!(reply.status, status, "{head}");
        assert_eq!(
            [api.requests(), wildcard.requests(), default.requests()],
            counts,
            "{head}"
        );
    }
}
