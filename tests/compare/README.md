# Comparison tests against GNU nano

The editor must behave exactly like GNU nano 8.4 (the version in Debian 13). These tests check it:
each scenario runs GNU nano and the editor under test side by side, each in its own
pseudo-terminal with [`vt100`](https://crates.io/crates/vt100) as the terminal emulator, with the
same files, the same screen size and the same keys. After every step the two **screens** must
match (text, colours and attributes, cursor), and at the end the **exit status** and the **files**
left in the working directory (names, contents, permissions) must match.

By default the editor under test is GNU nano itself, which checks the scenarios and the harness.
`COMPARE_EDITOR=target/debug/nyra-editor` (built with `--features test-sysconfdir`, and tested with the same feature, as in CI) compares the editor. CI does both,
in this order.

## Running

```sh
cargo test --test compare -- --nocapture
```

| Variable | Meaning |
|---|---|
| `NANO` | Path of GNU nano. Must be version 8.4, or the tests fail. When it is not set, `nano` from `PATH` is used if it is 8.4; otherwise the tests are skipped with a message. |
| `COMPARE_EDITOR` | The editor under test: a command with its arguments, split on spaces. Default: GNU nano. When it is set, scenarios with a `pending:` line are skipped. |
| `COMPARE_ONLY` | Run only the scenarios whose name contains this text, for example `search/`. |
| `COMPARE_SHOW` | When set, print nano's screen after every step and its files at the end: use it to check that a new scenario does what you meant. |
| `NANO_SYSCONFDIR` | The directory of the system nanorc that `$NANO` reads, when it is not `etc` next to the `bin` of a nano from `tools/build-nano.sh`. Its `nanorc` must have no settings, and the name must be compiled into nano (checked). |

Debian 13 has GNU nano 8.4 (`apt install nano`). Elsewhere, build it from the GNU release, checked
against a pinned SHA-256:

```sh
sudo apt-get install libncurses-dev     # or your system's ncurses headers
tools/build-nano.sh ~/nano-8.4
NANO=~/nano-8.4/bin/nano cargo test --test compare -- --nocapture
```

### The system nanorc

`tools/build-nano.sh` builds nano with its system nanorc at `PREFIX/etc/nanorc`, empty. The tests
point the editor under test there with `NYRA_EDITOR_SYSCONFDIR`, which replaces its system files
(`/usr/share/nyra-editor/nanorc` and `/etc/nanorc`). Only a build with the `test-sysconfdir`
feature reads that variable; release builds never do. Before the scenarios, the harness checks
that the editor under test reads it (with a probe nanorc), and fails otherwise. The editor ignores that variable for root and
when its real and effective users differ, so an inherited environment (`sudo`, `su -m`) cannot
choose the administrator's settings of a privileged editor. With a nano from a package, whose
`/etc/nanorc` cannot be replaced, the scenarios with nanorc files are skipped (they fail when `NANO` is set),
unless `NANO_SYSCONFDIR=/etc` says to use the host's file (which must then have no settings, only
comments, as the stock one).

As root, those scenarios are skipped too: nano takes root's home from the password database, so
the test directory is not its `HOME`. Run the tests as a normal user.

CI does the same on every push (the `compare` job), with the build cached, and fails if `$NANO` is
not 8.4 or if, with `$NANO` set, its system nanorc cannot be used (the nanorc scenarios are never
skipped there; the log says how many there are).

## The scenario format

A scenario is a text file `scenarios/<area>/<name>.scn`; its name is the path without `.scn`, for
example `search/where-is`. One line, one setting or step:

```
# Type a line into an existing file, save it with ^O and exit.
args: --linenumbers notes.txt
size: 80x24
file notes.txt:
| first line
| \tindented with a tab
keys: {End}{Enter}Hello, world
keys: {^O}
keys: {Enter}
keys: {^X}
```

| Line | Meaning |
|---|---|
| `# ...` | A comment (only at the start of a line). Start every scenario with one saying what it checks. |
| `args: ...` | Command-line arguments for both editors, split on spaces (no quoting). Both always get `--ignorercfiles` first, unless the scenario has a `nanorc:` or `xdgrc:` line. Default: none. |
| `nanorc: LINE` | One line (escapes as in keys: `\t`, `\\`, `\xHH`; may be empty) of `~/.nanorc`, written into each side's own `HOME` before it starts. Repeat for more lines. With it, `--ignorercfiles` is not passed, and both editors read the one system nanorc that the tests control (see "The system nanorc"). The scenarios are skipped, with a message, when that cannot be arranged or when the tests run as root: nano then takes the home directory from the password database, not from `HOME`. `HOME` is then the relative `../home`, so an error message naming the file reads the same on both sides. |
| `xdgrc: LINE` | The same for `~/.config/nano/nanorc`. |
| `env: NAME=value` | An environment variable for both editors, on top of the fixed environment below (`NO_COLOR`, for example). The name is upper-case letters and underscores, and not one the harness sets itself. Repeat for more. |
| `size: COLSxROWS` | The terminal size. Default: `80x24`. |
| `file NAME:` | A file in the working directory, created before the editors start; its lines follow, each starting with `\| ` (or just `\|` for an empty line). No lines: an empty file. `NAME` is relative and may have directories. After the first step, it is a step: the file is (re)written in both working directories while the editors run; the files are compared first, so that the rewrite cannot hide what was saved. |
| `keys: ...` | A step: keys sent at once (see below). The screens are compared after each step. |
| `resize: COLSxROWS` | A step: the terminal changes size. |
| `pause: MS` | A step: both editors are left alone for MS milliseconds (1 to 10000), for something they show for a while, then compared. |
| `differ: rows A-B[ cols C-D]: reason` | A deliberate difference from GNU nano: these screen cells are not compared, from the next step on (before the first step: also at the start). Also `row N`, `col N`. The reason is required. The cursor position is still compared, also inside the region. |
| `differ: none` | The deliberate differences declared so far end: the next steps are compared whole. |
| `pending: reason` | The editor does not do this yet: the scenario runs only when GNU nano is the editor under test, and is skipped (and counted as skipped) with `COMPARE_EDITOR`. Remove the line in the change that makes the editor pass it. |

`args`, `size` and `pending` may appear once; the other lines as often as needed. Steps run in
file order.

**File lines** are UTF-8 text, written to the file with a newline after each. Escapes: `\t` (tab),
`\r` (carriage return, for DOS line endings), `\e` (escape), `\\` (a backslash), `\xHH` (any byte,
also invalid UTF-8). A single `\` at the end of a line leaves out its newline (a file without a
final newline).

**Keys** are text, sent as typed, with special keys in braces. A literal `{` is `{{`. Leading and
trailing spaces are trimmed: write them as `{Space}`.

| Key | Written |
|---|---|
| Ctrl + key | `{^X}`, `{^]}`, `{^_}`, `{^@}`, `{^?}` (letters in either case) |
| Alt + key | `{M-u}`, `{M-U}`, `{M-^X}` (Alt and Ctrl) |
| Named keys | `{Enter}` `{Tab}` `{Esc}` `{Space}` `{Bsp}` `{Del}` `{Ins}` `{Up}` `{Down}` `{Left}` `{Right}` `{Home}` `{End}` `{PgUp}` `{PgDn}` `{F1}` to `{F12}` |
| Named keys with modifiers | `{^Left}`, `{S-Up}` (Shift), `{M-Home}`, `{^S-End}`: any of `^`, `M-`, `S-` |
| Repeated | `{Down*5}`, `{x*3}` (up to 200) |
| Mouse | `{Click 12 3}`: a press and a release of the left button at column 12, row 3; `{Mouse 65 5 4 M}`: one report of button code 65 (wheel down; 64 up, 0 left, 2 right; add 4, 8, 16 for Shift, Alt, Ctrl) at column 5, row 4, `M` a press and `m` a release. Columns and rows are counted from 1, as in the report. The editor needs `--mouse`. |

Send one mouse report to a step: ncurses does not handle several that arrive in one write the way
it handles them one by one, and nano's results then depend on its event queue, not on the mouse.

The keys are the bytes xterm sends: Backspace is `^?`, the arrows `ESC O A` and so on, with
modifiers `ESC [ 1 ; m A`. A lone `{Esc}` must be the last key of its step: followed by more keys in
the same step, it reads as Alt.

## Writing scenarios

- **One behaviour per scenario**, with a comment saying which. Short scenarios fail with a clear
  report; a scenario of 50 steps fails at the first difference and hides the rest.
- **One meaningful step per `keys:` line**, so a difference names the key that caused it. Typing a
  word, or moving several times, is one step.
- **End with the editor exited** (`{^X}`, answering any question), so the exit status and the saved
  files are compared too. A scenario that leaves the editor running still works: the test kills
  both editors and compares the files.
- **Check new scenarios with `COMPARE_SHOW=1 COMPARE_ONLY=<name>`**: GNU nano decides what is
  right, but only if the keys do what you meant (a question answered, a file actually saved).
- **Group by area** in directories (`basic/`, `move/`, `search/`, `text/`, ...), and name files by
  what they check, in lowercase words joined by hyphens.
- **`differ:` only for the differences the editor makes on purpose**, with the reason. A
  difference that is not deliberate is a bug in the editor, not a reason for `differ:`.
- Do not copy GNU nano's syntax files or source code into scenarios: nano is GPL; this repository
  is MIT. Scenarios describe behaviour; GNU nano is only run.

## What is fixed, so every run is the same

- The environment: `TERM=xterm-256color`, `LANG` and `LC_ALL` `C.UTF-8`, `TZ=UTC`, a fixed `PATH`,
  and an empty `HOME` per editor; nothing else is inherited.
- No configuration: `--ignorercfiles`, and the empty `HOME` has no nanorc (except in scenarios with
  `nanorc:` or `xdgrc:` lines, see above).
- Each editor runs in its own fresh working directory under `target/tmp/compare/<name>.scn/`, in
  its own session with the pseudo-terminal as the controlling terminal (so a resize sends
  `SIGWINCH`). A failed scenario's directories are kept there for a look.
- The title bar's program name and version are not compared: nano shows "GNU nano 8.4", the
  editor its own name. Only when nano's row 0 starts (after blanks) with `GNU nano 8.4`, the
  columns of that name and of the editor's first group of words on row 0 (up to two blanks) read
  as `*` on both screens, in the reports too. On a narrow screen nano leaves its name out, and
  nothing is masked.
- The welcome message is not compared: nano's, shown when it starts without a file, names nano
  ("Welcome to nano.  For basic help, type Ctrl+G."); the editor's leaves the name out ("For basic
  help, type Ctrl+G."). Only in the status bar (the third row from the bottom, or the last one
  with `-x`), and only when each screen shows its own message as nano shows messages (in brackets,
  centred; or, when that does not fit, from the left edge and cut at the right edge), with the
  look of nano's message and nothing else in the row, that row reads as `*` on both screens.
- The editor always locks its file (nano only with `-G`): its lock files (`.NAME.swp`, those nano
  does not have) are left out of the comparison of files before a rewrite, and at the end when
  it was still running and killed. When it has exited, they must be gone.
- The replacement character U+FFFD, which both draw for bytes that are not valid UTF-8, reaches
  the emulator as `¿`: vt100 drops U+FFFD, and the screens would not show it (a U+FFFD split
  between two reads is put together first).
- A reference nano that cannot run (exit status 126 or 127) fails the scenario, so a wrong
  `NANO` can never make the scenarios pass. A report also says when an editor has exited. `NANO`, and an editor command with a `/`, are made
  absolute paths, as the editors start in their own directories.

**Timing.** A step ends when the editor's output pauses for 40 ms. If the screens then differ, both
editors get a pause of 400 ms and are compared again, so a slow machine only slows the run down;
it does not fail it. Nothing waits forever: no first screen within 10 s, drawing for 10 s without
a pause, or not exiting within 3 s of closing the terminal fails the scenario. An editor still
running after the last step is killed.

## A failure

When a step's screens differ, the report shows nano's screen with the differing rows marked, each
differing row from both editors with `^` under the differing cells, differences in look by column
range, and the cursors:

```
FAIL basic/type-and-save: screens differ after line 5, keys: Hello, world{Enter}
  nano's screen (> marks the rows that differ):
     0 |  ************                       notes.txt                        Modified
     1 |Hello, world
     2 |first line
     ...
  > 21 |                                [ Read 1 line ]
    22 |^G Help      ^O Write Out ^F Where Is  ^K Cut       ^T Execute   ^C Location
    23 |^X Exit      ^R Read File ^\ Replace   ^U Paste     ^J Justify   ^/ Go To Line
  row 21:
    nano:   |                                [ Read 1 line ]
    editor: |            [ line  2/3 (66%), col  1/11 (  9%), char 13/24 (54%) ]
                         ^ ^^^^  ^^^ ^^^^^^ ^^^^^^^^  ^^^^^^^ ^^^^ ^^^^^ ^^^^^ ^
  row 21, columns 12-31: nano plain, editor inverse
  row 21, columns 47-66: nano plain, editor inverse
```

This one comes from the harness's own test, which runs a scenario with `nano --constantshow` as a
deliberately wrong editor and checks that it fails like this; `cargo test --test compare --
--nocapture` prints it. Files that differ are reported by name, permissions and the first
differing line.
