# AGENTS.md

Guide for coding agents and people working in this repository. What the project is and how to run it: [README.md](README.md).

## What this is

`editor` (crate `nyra-editor`) is a nano-compatible terminal editor written from scratch in Rust, for Nyra OS. The target is to behave exactly like GNU nano 8.4, the version in Debian 13: same keys, same screens, same files left behind. It is not GNU nano and is not built from its code.

## Map (read in this order)

1. `README.md`: what works today and what is refused with "not available yet".
2. `src/main.rs`, `src/config.rs`, `src/rcfile.rs`: start-up, the command line, the nanorc files.
3. `src/commands.rs`, `src/bindings.rs`, `src/keys.rs`: the editor state, what each key does, key bindings.
4. `src/buffer.rs`, `src/search.rs`, `src/replace.rs`, `src/complete.rs`, `src/wrap.rs`, `src/syntax.rs`: the text (a rope), search and replace, completion, soft wrapping, syntax colouring.
5. `src/screen.rs`: drawing and the terminal.
6. `src/files/`: reading, safe saving, lock files, backups, emergency copies.
7. `docs/REGEX.md`: how nano's regular expressions map to the `regex` crate.
8. `tests/cli.rs` and `tests/compare/` (scenarios run on GNU nano and on the editor; see `tests/compare/README.md`).

## Build, test, lint

Rust 1.85 or newer. Run what CI runs (`.github/workflows/ci.yml`):

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo +1.85 test --locked      # the oldest supported Rust
cargo deny --locked check      # needs cargo-deny
```

- The saving tests in `src/files` cover more as root (bind mounts, a full file system, other users' files); CI runs them as root in a privileged container. How to run them so locally: README, "Build and test".
- The comparison tests need GNU nano 8.4 (`apt install nano` on Debian 13, or `tools/build-nano.sh DIR`) and run in CI; the command lines are in `tests/compare/README.md`. Without nano 8.4 they are skipped.
- Check a new behaviour with a comparison scenario, so the editor is checked against nano, not against our reading of it.

## Working rules

- Work on a branch, never on `main` (it is protected). One small, focused change per pull request, with a clear reason.
- Before you start, read the recent changes (`git log`) and the docs of the area.
- Commit messages: a short imperative subject in plain English. The only trailer is `Co-Authored-By: <model name> <noreply@anthropic.com>`. No "Generated with ..." line and no session link in commits or pull request text.
- A test that fails sometimes is a bug: find the cause, do not rerun until green. Never weaken or skip a test to make it pass.
- An option the editor cannot honour yet is refused with a message, not silently ignored.
- Pin what CI runs: actions by commit SHA, container images by digest. The crates in `[dependencies]` are pinned with `=`.
- English for code, comments, commits and docs. New files start with an SPDX header (`MIT`).

## Code rules (KISS: code a person can read)

- Read top to bottom. Functions about 50 lines at most, files short. Each file starts with a short header saying what it does and why it exists.
- Plain `match` and loops instead of long iterator chains. No macros.
- No generics, explicit lifetimes or `dyn` unless needed, with the reason written next to them.
- No `unsafe`: the code has none today (system calls go through `rustix`). Keep it so.
- Comments explain why, not what.
- A dependency that makes the code clearer is fine, with its reason in `Cargo.toml`; `cargo deny` must stay green. Do not write your own replacement without a reason.
- No options or layers "for later".

## Security-sensitive areas

The editor opens and writes files with the rights of whoever runs it. Changes here need a security review before merge: saving, locks, backups and emergency copies (`src/files/`: no lost data, the right file, the right permissions), the handling of pasted text and escape sequences (a paste is never run as keys), the reading of nanorc files, and the colouring of text (`src/syntax.rs`, `colour_cells` and `sgr` in `src/screen.rs`: only numbers from the nanorc reach the terminal, and the work per screen is bounded). Never put secrets, keys or tokens in the repository, tests or logs; secrets in tests are throwaway. Report vulnerabilities privately: [SECURITY.md](SECURITY.md).

## Docs

`README.md`, `docs/REGEX.md`, `docs/sh.nanorc` (example syntax), `tests/compare/README.md`.
