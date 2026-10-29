# Editor

A nano-compatible terminal editor for Nyra OS, installed as the `editor` command; not GNU nano.
Written from scratch in Rust, for [Nyra OS](https://nyraos.com).

## Status

Early development. What works, the same way as in GNU nano 8.4:

- opening a file (or a new buffer); the command line of nano 8.4 for what is implemented:
  `+LINE,COLUMN`, `+/TEXT` and `+?TEXT` (with the modifiers `c` `C` `r` `R`), `-A` `-B` `-C` `-E`
  `-F` `-G` `-I` `-J` `-L` `-N` `-T` `-W` `-X` `-Y` `-Z` `-c` `-i` `-k` `-l` `-m` `-q` `-t` `-u` `-x` `-z`,
  long forms (also shortened), joined short options, `--`, and nano's messages for a wrong option.
  `-M` `-O` `-a` `-r` `-s` `-w` are accepted and do nothing yet; every other option of nano
  (`--minibar`, `--stateflags`, ...) is refused with "not available yet", rather than ignored, as is
  the file browser (`^T` in the prompt for a file to read). `--help` lists them;
- several files: one buffer for each file on the command line (up to 256), with `+LINE,COLUMN`
  and `+/TEXT` applying to the file that follows; `M-<` and `M->` (or `M-,` and `M-.`) switch
  between them and the title bar shows `[n/m]`; `^X` closes the current buffer, asking to save
  it, and the editor ends after the last; `^R` reads a file into the text, and with `M-F` (or
  `-F`/`--multibuffer` for every time) into a new buffer. Undo, the mark, the syntax, the lock
  file and the disk-change check are each buffer's own. A file that is already open can be
  opened again after the lock question, as in nano; both buffers then edit the same file, and
  the disk-change check warns the one that saves second;
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
- writing the file (`^O` `^S`) and exiting (`^X`);
- syntax colouring from the `syntax`, `header`, `color`, `icolor` and `extendsyntax` lines of a
  nanorc (see below). `tabgives`, `linter` and `formatter` are refused with "not available yet",
  and `comment` too (it is for a key that comments lines); `magic` is read and does nothing, as it
  only works with `--magic`;
- the colours of the interface: `set titlecolor`, `statuscolor`, `errorcolor`, `selectedcolor`,
  `numbercolor`, `keycolor`, `functioncolor`, `promptcolor`, `spotlightcolor`, `stripecolor` and
  `scrollercolor` in a nanorc, in the syntax of the colours of a syntax (`bold,italic,white,red`,
  `lightblue`, `#f80`), with nano's defaults and its messages for a wrong colour. They colour the
  title bar, the status bar, the prompts, the line numbers, the keys and the names of the help
  lines, the selection, the search match, the guide stripe and the scroll bar. `minicolor` (the
  mini bar is not in the editor) is refused with "not available yet". With `NO_COLOR` in the
  environment, as in nano 8.4, the error messages and the search match are inverse instead of
  coloured, unless the nanorc sets a colour of the interface; the colours of a syntax stay;
- the guide stripe (`-J COLUMN`, `--guidestripe`, `set guidestripe`): the cell of that column is
  drawn in the colour of `stripecolor` (inverse by default) on every line that reaches it, also
  on a wrapped row; and the scroll bar (`-q`, `--indicator`, `set indicator`) in the last column
  of the text, which shows where the screen is in the text and takes that column from the text;
- the syntax of a file by name: `-Y NAME` (`--syntax`) uses the syntax NAME for every file, `none`
  for no syntax, and says "Unknown syntax name" when there is none of that name; `-z`
  (`--listsyntaxes`) prints the names of the syntaxes and exits;
- the mouse, with `-m` (`--mouse`) or `set mouse`, off unless asked: a click of the left button puts
  the cursor there, a second one on the same character sets the mark (or takes it away), a click
  on an item of the help lines or on Yes, No, All or Cancel of a question presses its key, and the
  wheel scrolls the text (or the help text) by two lines. The other buttons, a drag and a click on
  the bars do nothing, as in nano. The editor reads a mouse report strictly (it is dropped if it is
  not exactly `ESC [ < button ; column ; row M` or `m` with numbers of at most five digits), so the
  bytes of one are never typed as text. `M-M` switches the mouse on and off while editing. Mouse
  reporting, the bracketed paste and the alternate screen are switched off again whenever the
  editor ends, by a signal and by a panic too;
- key bindings: `bind KEY FUNCTION MENU` and `unbind KEY MENU` in a nanorc (key names, menus and
  messages as in `man nanorc`; a function the editor does not have yet is refused with a message,
  a string as the function types it in the main menu), and `--modernbindings`, applied first. The
  help lines show the keys as bound. Without a key for Exit (main) or Cancel (yes/no) the editor
  does not start, as nano.

Saving is safe (a new file renamed over the old one, or a copy first when the file must be
written in place), and a file changed on disk since it was read is noticed, as in nano. Files are
always locked (nano's `--locking`, vim-style `.NAME.swp`); SIGHUP, SIGTERM or a closed terminal
write the changes of every modified buffer to its `NAME.save`.

## Syntax colouring

No syntax is built in, and the package ships none. The editor colours a file only when a nanorc
defines a syntax for it, in the nanorc format of nano 8.4 (`man nanorc`). The files are read in
this order, a later one winning: `/usr/share/nyra-editor/nanorc`, `/etc/nanorc`, then the user's
`~/.nanorc` (else `$XDG_CONFIG_HOME/nano/nanorc`, else `~/.config/nano/nanorc`), and the files
that these name in an `include "GLOB"` line. A syntax is picked when a file is opened or saved
under another name: the one whose `syntax NAME "REGEX"...` matches the full name of the file, else
the one whose `header "REGEX"...` matches its first line, else `default`. A `color` or `icolor`
line gives a foreground and background (nano's names, `bright` and `light` before the basic ones, `#rgb`, `normal`, and `bold,` or `italic,` first)
and one or more expressions, or `start="..." end="..."` for text over several lines. Rules apply
in order, and a later rule paints over an earlier one. [docs/sh.nanorc](docs/sh.nanorc) is a small
example.

The text of a file never reaches the terminal as bytes, only as the printable characters and the
`^X` forms that the editor draws without colours too; the colours come from the nanorc only. A
file cannot make the screen hang either: the expressions are the same as in search (see
[docs/REGEX.md](docs/REGEX.md)) and are also limited in size, a syntax has at most 2000 rules,
only the first 2000 bytes of a line are coloured, a multi-line rule is looked for in the 1000
lines above the screen at most, and a screen takes 50 ms of matching at most (the rest of it is
drawn without colour).

Differences from nano: all syntaxes of the nanorc files are read at the start, and a mistake in
one is reported then, not when a file needs it; and the limits above. nano also colours nothing
that starts past 2000 bytes of a line, but lets a match that starts before it run on: here a
match is cut there (`x+` on a line of 3000 `x` colours 2000), and `$` matches at the cut.

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
