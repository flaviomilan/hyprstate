# Contributing

Thanks for your interest in hyprstate. Bug reports, fixes and new discovery
signals for apps that aren't recognized yet are all welcome.

## Before you start

- For anything larger than a small fix, open an issue or a discussion first so
  we can agree on the approach.
- Check the "Not in scope" section of the README; those are deliberate
  non-goals.

## Development

```
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

CI runs the same three commands, plus a coverage check: **every line and every
function must be covered** (`cargo llvm-cov --fail-under-lines 100
--fail-under-functions 100`), with no files excluded. New code comes with the
tests that exercise it, error paths included. To see what is uncovered:

```
cargo install cargo-llvm-cov
cargo llvm-cov --open   # HTML report in the browser
```

Tests never talk to a real Hyprland:

- Unit tests live next to the code and use the fixtures in `tests/fixtures/`
  (real `hyprctl -j` output with titles redacted). If you add fixtures, redact
  titles, paths and anything personal.
- `tests/cli.rs` runs the real binary against a fake Hyprland that listens on
  the same Unix sockets, in a temporary `$HOME`. Use it for anything that
  depends on the environment, the sockets or the command line.

When you change discovery, restore or worksets, also try it on a real session,
always with `--dry-run` first.

## Pull requests

- `main` only accepts pull requests, and they are squash-merged, so the PR
  title becomes the commit message. Use [Conventional Commits](https://www.conventionalcommits.org/)
  style: `feat: …`, `fix: …`, `docs: …`.
- Keep PRs focused on one change.

## License

By contributing, you agree that your contributions are dual licensed under
MIT OR Apache-2.0, as described in the README.
