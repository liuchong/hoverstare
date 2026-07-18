# bugbot 🐛

Repo-aware AI code review for GitHub pull requests.

Bugbot reviews PRs like a human reviewer would: it reads your repository with
read-only tools (`read_file` / `grep` / `glob` / `show_base_file`), verifies
suspicions before reporting, votes across multiple independent review passes,
and posts precise inline comments — then tracks each finding across commits
until it's fixed.

## Install

```bash
cargo install bugbot
```

## Usage

```bash
# Review a PR inside GitHub Actions (or locally with --repo/--pr)
bugbot review

# Handle @bugbot comment commands
bugbot mention
```

## Documentation

- Setup guide (GitHub Action, configuration, FAQ):
  [github.com/liuchong/hoverstare](https://github.com/liuchong/hoverstare)
- Design specs:
  [specs/](https://github.com/liuchong/hoverstare/tree/master/specs)

## Note

`bugbot` is published together with
[`hoverstare`](https://crates.io/crates/hoverstare) — the two packages share
the same codebase and behave identically; use whichever name you prefer.

## License

[1PL — One Public License](https://license.pub/1pl/)
