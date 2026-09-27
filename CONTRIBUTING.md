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
cargo test          # unit tests plus executor tests against a fake compositor
```

CI runs the same three commands, plus a coverage check: line coverage must
stay above the floor set in `.github/workflows/ci.yml` (`--fail-under-lines`).
New code should come with tests; to see what is uncovered locally:

```
cargo install cargo-llvm-cov
cargo llvm-cov --open   # HTML report in the browser
```

When a PR raises coverage, bump the floor in the same PR. Tests never talk to a real Hyprland; they use
the fixtures in `tests/fixtures/` (real `hyprctl -j` output with titles
redacted). If you add fixtures, redact titles, paths and anything personal.

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
