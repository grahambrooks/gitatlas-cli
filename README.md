# gitatlas

Multi-repo Git management CLI — a companion to the gitatlas GUI.

`gitatlas` scans a set of root directories for Git repositories, caches their
state, and lets you inspect or operate on them individually or in bulk from a
single command-line tool. It also ships with an interactive terminal UI.

## Features

- Discover repositories under one or more configured scan roots.
- See per-repo health (clean / dirty / diverged / error) at a glance.
- Bulk `fetch` and `pull --rebase` across many repos.
- Per-repo operations: `status`, `log`, `show`, `diff`, `add`, `reset`,
  `commit`, `squash`, `push`.
- Branch, stash, and remote management.
- View or update the per-repo `user.name` / `user.email` profile.
- Print a repo's README or a PR-creation URL for the current branch.
- Machine-readable `--json` output on every command for scripting.
- Interactive TUI (`gitatlas tui`).

## Install

Requires a stable Rust toolchain (1.75+ recommended).

```sh
# From a clone of this repo
cargo install --path .

# Or build a release binary in ./target/release/gitatlas
cargo build --release
```

## Quick start

```sh
# Add a scan root (defaults to ~/dev if none configured)
gitatlas config roots add ~/code

# Scan and cache repos
gitatlas scan

# List cached repos, filter by health or search
gitatlas list
gitatlas list --health dirty
gitatlas list -q server

# Inspect a single repo (by name or path)
gitatlas status my-repo
gitatlas log my-repo -n 20
gitatlas diff my-repo --staged

# Bulk operations
gitatlas fetch --all
gitatlas pull repo-a repo-b

# Launch the interactive TUI
gitatlas tui
```

Pass `--json` to any command to emit structured output for scripting:

```sh
gitatlas list --json | jq '.[] | select(.health == "dirty") | .name'
```

## Configuration

State is stored under the standard user config / data directories
(`dirs-next`):

- **Config** (`scan_roots`): edited via `gitatlas config roots {add,remove,set,list}`.
- **Cache** (scanned repo metadata): refreshed by `gitatlas scan` or
  `gitatlas list --refresh`.

Run `gitatlas config show` to see the active configuration.

## Development

```sh
cargo build
cargo test
```

CI builds and tests on Linux and macOS for every push and pull request to
`main` — see `.github/workflows/ci.yml`.

## License

TBD.
