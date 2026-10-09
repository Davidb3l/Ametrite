# Contributing to Ametrite

Thank you for considering a contribution. Ametrite is a small project with a single maintainer; that means PRs get reviewed personally and merged when they are right.

## Code of conduct

This project adopts the [Contributor Covenant 2.1](CODE_OF_CONDUCT.md). See that file for how to report a violation.

## Developer Certificate of Origin (DCO)

Every commit must be signed off:

```sh
git commit -s -m "your message"
```

The sign-off line (`Signed-off-by: Your Name <you@example.com>`) certifies that you wrote the change or otherwise have the right to submit it under the project's MIT license. See [developercertificate.org](https://developercertificate.org) for the full text. We do not require a CLA.

## Development environment

Ametrite is a monorepo with two stacks:

- **`crates/amt/`** — the Rust engine and the `amt` binary (CLI + MCP server). Install [rustup](https://rustup.rs/); `rust-toolchain.toml` pins the toolchain, so `cargo build` from the repo root picks the right one.
- **`apps/web/`** — the optional web board, Bun + TypeScript. Run `bun install` from the repo root.

Typical dev loop:

```sh
cargo build                     # debug binary at target/debug/amt
./target/debug/amt --help

bun run web                     # web board → http://localhost:1776
```

When you exercise the CLI by hand, point it at a scratch registry so test workspaces don't land in your real `~/.ametrite/registry.json`:

```sh
export AMT_REGISTRY="$(mktemp -d)/registry.json"
```

## Tests

CI (`.github/workflows/ci.yml`) runs these on every pull request and on pushes to `main`; run them locally before opening a PR:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                      # Linux, macOS, and Windows in CI
bun test                        # web board
```

CI also runs a release-build performance gate (`cargo run --release --example bench`) that fails if a hot read path regresses past its budget.

## Pull request process

1. Open an issue first if your change is non-trivial — saves you wasted work if the direction is wrong.
2. Branch from `main`. Keep branches focused; one logical change per PR.
3. Add tests for behavior changes. CLI contracts (such as `--json` printing exactly one JSON object) are tested by driving the real binary; see `crates/amt/tests/cli_*.rs`.
4. Run the checks above.
5. Update the docs where behavior changed: `README.md`, CLI help text, `AGENTS.md`, and the skill at `.claude/skills/ametrite/SKILL.md`. Add user-visible changes to `CHANGELOG.md` under "Unreleased".
6. Sign off every commit (`git commit -s`). A CI check rejects pull requests with unsigned commits.
7. Open the PR with the template.

The maintainer aims to respond within a few days. If the PR is not the right fit, you will hear why and what would change that.

## Questions

See [SUPPORT.md](SUPPORT.md) for where to ask.

## Reporting bugs

Use the bug report issue template, and include `amt --version`, your OS, how you installed `amt`, and the output of `amt doctor`.

## Security issues

Do not open a public issue. See [SECURITY.md](SECURITY.md).
