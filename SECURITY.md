# Security policy

## Reporting a vulnerability

Report it privately through GitHub: the **Security** tab of this repository, then **Report a vulnerability**. Please do not open a public issue, discussion or pull request for it.

Include what you found and how to reproduce it. You will get an answer within 7 days, and a fix or a plan within 30 days for confirmed issues. You are credited in the advisory unless you ask not to be.

## Supported versions

Security fixes go into the latest release only.

## What counts

- A request that waystation forwards although its length or boundaries are ambiguous (request smuggling).
- Input from a client or an upstream that makes waystation panic, hang, or use memory without bound.
- A way around a configured limit, timeout or rate limit.
- A secret (private key, authorization header, cookie) appearing in a log or a metric.
- Reaching the admin listener from a non-loopback address under the default configuration.

Out of scope: denial of service by a volume of valid traffic larger than the configured limits allow for.
