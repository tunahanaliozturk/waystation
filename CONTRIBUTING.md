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

## Code rules

The compiler and clippy enforce most of these. Warnings are errors, locally (`.cargo/config.toml`) and in CI. The lint set is in `Cargo.toml`: `clippy::all`, `clippy::pedantic`, `clippy::cargo` and a few stricter picks.

The rest follows the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/). The ones that come up most:

- **Naming.** `UpperCamelCase` types, `snake_case` functions, `SCREAMING_SNAKE_CASE` constants. Conversions are `as_` (free, borrowed), `to_` (costly) and `into_` (consuming). Getters have no `get_` prefix. Constructors are `new` or `with_*`.
- **Errors.** Fallible functions return `Result`. Error types implement `std::error::Error`, `Debug` and `Display`, and are `#[non_exhaustive]`. No `unwrap`, `expect` or `panic!` outside tests; a state that cannot happen is ruled out by the types, not asserted.
- **Types over checks.** A value with an invariant gets a newtype whose constructor checks it once. No bare `bool` or integer parameters where two could be swapped. Illegal states should not compile.
- **Common traits.** Public types derive `Debug`. They derive `Clone`, `PartialEq`, `Eq`, `Hash` and `Default` where those make sense, and nothing they cannot honour.
- **Visibility.** Everything is private until a caller needs it. Struct fields are private. Public enums and structs that may grow are `#[non_exhaustive]`.
- **Ownership.** Take `&str`, `&[T]` and `impl AsRef<Path>`, not `&String`, `&Vec<T>` and `&PathBuf`. Take ownership only when the function keeps the value. No `clone` to quiet the borrow checker.
- **Casts.** No lossy `as`. Use `From`, `TryFrom` or the explicit integer methods.
- **Docs.** Every public item has a doc comment. A function that can fail has an `# Errors` section, one that can panic has `# Panics`, an `unsafe` one has `# Safety`. Examples in docs are compiled and run as tests.
- **`#[must_use]`** on functions whose result is the point, and on guards.
- **Lints are fixed, not silenced.** An `#[expect(...)]` needs a `reason`. `#[allow]` is not used.
- **Features are additive.** Turning one on never removes an item or changes behaviour for code that did not ask.
- **Formatting** is whatever `cargo fmt` produces. No hand alignment.
- **Dependencies** are added for a reason written in the commit, at the newest stable version, with a permissive licence.

## Docs

English with British spelling, short sentences, no em dashes. Say why, not only what.

## Security

Do not open a public issue for a vulnerability. See [SECURITY.md](SECURITY.md).

By contributing you agree that your contribution is dual licensed under the MIT and Apache 2.0 licences, as the rest of the project is.
