# Editor

A nano-compatible terminal editor for Nyra OS, installed as the `editor` command; not GNU nano.
Written from scratch in Rust, for [Nyra OS](https://nyraos.com).

## Status

Early development: nothing is usable yet. `editor --version` is the only thing that works.

## Build and test

Rust 1.85 or newer:

```sh
cargo build
cargo test
```

The tests of saving (`src/files`) cover more as root: bind mounts, a full file system, files of
other users, and each test again as an ordinary user (uid 4242). CI runs them as root in a
privileged container; to run them so locally:

```sh
sudo "$(cargo test --bin nyra-editor --no-run 2>&1 | sed -n 's/^ *Executable .*(\(.*\))$/\1/p')" files::
```

The comparison tests run every scenario on GNU nano 8.4 and on the editor, and compare the screens
and the saved files: see [tests/compare/README.md](tests/compare/README.md).

## Security

See [SECURITY.md](SECURITY.md) to report a vulnerability.

## Licence

MIT, see [LICENSE](LICENSE).
