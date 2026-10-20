# Editor

A nano-compatible terminal editor for Nyra OS, installed as the `editor` command; not GNU nano.
Written from scratch in Rust, for [Nyra OS](https://nyraos.com).

## Status

Early development. What works, the same way as in GNU nano 8.4:

- opening a file (or a new buffer), `+LINE,COLUMN`, `-l`, `-c`, `-x`, `-T`, `-L`;
- moving: arrows, Home and End, `^A` `^E` `^P` `^N`, PgUp and PgDn (`^Y` `^V`), ^Home and ^End
  (`M-\` `M-/`), ^Left and ^Right by words; long lines scroll sideways;
- typing and deleting, Enter, Tab, Backspace and Delete (`^H` `^D`); UTF-8 with wide characters
  (CJK, emoji) and combining marks; tabs; DOS and Mac line ends, kept when saving;
- cutting and pasting lines (`^K` `^U`), a simple search (`^W` `^F` `M-W`), the position (`^C`);
- the title bar, the status bar and the help lines at any terminal size, the help screen (`^G`);
- writing the file (`^O` `^S`) and exiting (`^X`).

Saving is still the simple kind (the file is written in place); safe saving, emergency saves,
locking and backups come next.

## Build and test

Rust 1.85 or newer:

```sh
cargo build
cargo test
```

The comparison tests run every scenario on GNU nano 8.4 and on the editor, and compare the screens
and the saved files: see [tests/compare/README.md](tests/compare/README.md). To run them on the
editor:

```sh
cargo build
COMPARE_EDITOR=target/debug/nyra-editor cargo test --test compare -- --nocapture
```

## Security

See [SECURITY.md](SECURITY.md) to report a vulnerability.

## Licence

MIT, see [LICENSE](LICENSE).
