# gitatlas Roadmap

## Bug Fixes

- **`config roots remove` path mismatch** — paths are stored canonicalized at `add` time but
  removal does a literal string comparison, so passing `~/code` instead of `/Users/you/code`
  silently fails. Canonicalize the input before the comparison.

## Improvements

- **Parallelize `fetch` and `pull`** — both commands loop sequentially today even though `rayon`
  is already used for scanning. Running them in parallel would give the same speed-up.

- **Cache staleness warning** — `list` silently shows data that may be days old. Warn the user
  when the cache was last refreshed more than ~1 hour ago and suggest `--refresh`.

- **XDG / platform config directories** — `dirs-next` is already a dependency but `home_dir()`
  is used directly, writing to `~/.gitatlas`. Switch to `config_dir()` / `data_dir()` so the
  tool respects XDG on Linux and platform conventions on macOS.

- **Resolve the license** — `LICENSE` is TBD; pick and commit to one before publishing.

- **Add tests** — model parsing, resolver logic, and cache round-trips should have unit coverage
  at minimum.

## New Features

### High priority

- **`gitatlas run <cmd>`** — execute an arbitrary shell command across every (or a filtered set
  of) cached repos. This is the single most-requested capability in multi-repo tools.
  ```
  gitatlas run -- cargo update
  gitatlas run --health dirty -- git status
  ```

- **Shell completions** — wire up `clap_complete` to generate completions for bash, zsh, and
  fish via a hidden `gitatlas completions <shell>` command.

### Medium priority

- **`gitatlas clone <url>`** — clone a repository into the first configured scan root (or a
  `--dest` override) and add it to the cache automatically.

- **`gitatlas tag`** subcommand — list, create, and delete tags; currently no tag management
  exists despite tags being a first-class Git concept.
  ```
  gitatlas tag <repo>               # list
  gitatlas tag <repo> create <name>
  gitatlas tag <repo> delete <name>
  ```

- **Cache age display** — show the last-refreshed timestamp in `gitatlas list` footer and
  `gitatlas config show`.

- **`--dry-run` flag** on `fetch`, `pull`, and `push` — print what would happen without
  performing network operations; important safety valve for bulk ops.

### Lower priority

- **`gitatlas pr --open`** — the `pr` command already builds the PR-creation URL; add `--open`
  to launch it in the default browser (`open` / `xdg-open`).

- **`NO_COLOR` env var** — `console` supports colour disabling but it should be explicitly
  checked so the tool plays nicely with CI and colour-blind terminals.

## Features to Reconsider

- **`squash`** — squashes N commits with no confirmation. It is a destructive history rewrite
  and easy to trigger accidentally in bulk workflows. Add a `dialoguer` confirmation prompt
  (the crate is already a dependency) or require an explicit `--force` flag.

- **`readme`** — prints the raw README to stdout, a niche use-case easily replaced by a shell
  alias. Consider removing in a future cleanup pass unless the TUI makes active use of it.
