# Contributing to HoverStare

Thanks for helping improve HoverStare! This guide covers the workflow and quality gates for the Rust workspace (`hoverstare` root crate + `crates/bugbot` alias crate).

For architecture rules and hard constraints, see [`AGENTS.md`](AGENTS.md). For design specs, see [`specs/README.md`](specs/README.md).

## Getting started

1. Clone the repository:
   ```bash
   git clone https://github.com/liuchong/hoverstare.git
   cd hoverstare
   ```
2. Install a recent stable Rust toolchain (the project tracks the latest stable release).
3. The workspace is configured at the repo root; all commands below are run from this directory.

## Quality gate

Every PR must pass the four commands below before being merged. Run them from the workspace root:

```bash
cargo build --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- `cargo build --workspace` ensures the whole workspace compiles.
- `cargo fmt --all -- --check` ensures code is formatted with `rustfmt`.
- `cargo clippy --workspace --all-targets -- -D warnings` runs Clippy and treats any warning as an error.
- `cargo test --workspace` runs all unit tests and contract tests.

If `cargo fmt --all -- --check` fails, run `cargo fmt --all` to apply the formatting.

## Conventional Commits

Use [Conventional Commits](https://www.conventionalcommits.org/) for all commit messages and PR titles. Common prefixes in this repo:

- `feat:` — new feature or behavior
- `fix:` — bug fix
- `docs:` — documentation-only changes
- `refactor:` — code change that neither fixes a bug nor adds a feature
- `test:` — adding or updating tests
- `chore:` — maintenance, tooling, or dependency updates

Examples:

- `feat: add mention command parser`
- `fix: align finding anchor for deleted lines`
- `docs: update quality gate commands in CONTRIBUTING.md`

## PR review process

PR reviews are performed by **HoverStare itself** (the bot). Repo collaborators can trigger or re-trigger a review by posting `@hoverstare review` in a PR. For a list of available commands, post `@hoverstare help`.

For details on bot commands, see the [`@hoverstare` commands](README.md#hoverstare-commands) section in `README.md`.

## Where to learn more

- [`AGENTS.md`](AGENTS.md) — project background, architecture rules, and hard constraints.
- [`specs/README.md`](specs/README.md) — design specs and milestone plan.
- [`README.md`](README.md) — quick start, local dry-run examples, and the `@hoverstare` command table.

## License

By contributing, you agree that your contributions will be licensed under the [1PL — One Public License](https://license.pub/1pl/).
