# Contributing

Bug reports, failing test cases and fixes are welcome. A pull request that adds a test which fails without the change is the most useful kind.

## Set up

You need a stable Rust toolchain (1.85 or newer) and git.

```
git clone https://github.com/tunahanaliozturk/waystation
cd waystation
cargo test
```

## Before you open a pull request

CI runs these; run them locally first:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check
```

- One change per pull request, with a test. Name tests after the behaviour (`a_torn_tail_is_dropped_on_recovery`), not the function.
- No `unsafe`. The crate forbids it.
- A change to the configuration format needs a note in [docs/design.md](docs/design.md).
- No new dependency unless there is no reasonable way without it. Every dependency must have a permissive licence; `cargo deny` enforces that.
- A decision that is hard to reverse gets an ADR in `docs/adr/`.
- Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `docs:`).

## Docs

English with British spelling, short sentences, no em dashes. Say why, not only what.

## Security

Do not open a public issue for a vulnerability. See [SECURITY.md](SECURITY.md).

By contributing you agree that your contribution is dual licensed under the MIT and Apache 2.0 licences, as the rest of the project is.
