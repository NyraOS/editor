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

The comparison tests run every scenario on GNU nano 8.4 and on the editor, and compare the screens
and the saved files: see [tests/compare/README.md](tests/compare/README.md).

## Security

See [SECURITY.md](SECURITY.md) to report a vulnerability.

## Licence

MIT, see [LICENSE](LICENSE).
