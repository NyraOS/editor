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
- the mark (`M-A` or `^^`) and selecting with Shift and the cursor keys; cutting (`^K`), copying
  (`M-6`) and pasting (`^U`) the selection or lines, `M-T` to cut to the end; a bracketed paste
  goes in as text, in one step of undo; unlike nano, nothing between its start and its end is ever
  run as a key (control characters and escape sequences are dropped, a CR LF is one line break,
  and a paste that gets no end for 2 s is ended);
- the position (`^C`), undo and redo (`M-U` `M-E`) with nano's steps (typing in a row, deleting
  in a row, cutting in a row; a new line, a paste and a cut selection are steps of their own);
- searching (`^W` `^F` `F6`, backwards with `^Q` `^B`, again with `M-W` `M-Q`; `M-C`, `M-R` (regular
  expressions) and `M-B` in the prompt, a history with `^P` `^N`) and going to a line and column
  (`^_` `M-G`); replacing (`^\` `M-R`: Yes, No, All or `^C` for each match, `\1` to `\9` in the
  replacement, in the selection when there is one). Each replacement is a step of undo, as in
  nano. The regular expressions are nano's (POSIX extended) translated to the `regex` crate; the
  differences are in [docs/REGEX.md](docs/REGEX.md);
- completing a word (`^]`): the fragment before the cursor takes the ending of a word of the
  text, and the next one on each `^]` in a row; one step of undo;
- the title bar, the status bar and the help lines at any terminal size, the help screen (`^G`);
- writing the file (`^O` `^S`) and exiting (`^X`).

Saving is safe (a new file renamed over the old one, or a copy first when the file must be
written in place), and a file changed on disk since it was read is noticed, as in nano. Files are
always locked (nano's `--locking`, vim-style `.NAME.swp`); SIGHUP, SIGTERM or a closed terminal
write the changes to `NAME.save`.

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
