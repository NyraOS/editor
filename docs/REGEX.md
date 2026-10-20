# Regular expressions

`M-R` in the search prompt (and `^\` or `M-R` in the main menu, for replacing) makes the text
typed a regular expression. nano uses the POSIX extended syntax of the C library; the editor
translates it (`translate` in `src/search.rs`) to the `regex` crate, whose version is pinned. The
translated constructs behave like nano's, and each has a scenario under
`tests/compare/scenarios/replace/` that runs against GNU nano 8.4:

| Construct | Scenario |
|-----------|----------|
| classes, `[[:alpha:]]`, `[[:digit:]]`, ... | `regexp-class`, `regexp-class-alpha`, `regexp-class-alnum`, `regexp-class-space` |
| `.` and `[^...]` never match an invalid byte of the file | `regexp-invalid-bytes` |
| `^` and `$`, also matching nothing | `regexp-anchor-start`, `regexp-anchor-end`, `regexp-empty-matches`, `regexp-empty-skips` |
| alternation `a\|b` and groups | `regexp-alternation` |
| intervals `{n}`, `{n,m}`, `{,m}` | `regexp-interval`, `regexp-interval-empty` |
| `\<` and `\>` word boundaries (also `\b` `\B`) | `regexp-word-boundaries` |
| `\1` to `\9` in the replacement | `backreferences`, `backreference-missing`, `backreference-literal` |
| errors, shown as `Bad regex "...": reason`, with the text of the C library | `regexp-bad-paren`, `regexp-bad-interval` |

In a replacement, `\1` to `\9` are the groups of the match (nothing for a group that did not
match); `\0`, `&`, and a group the pattern does not have stay as typed, as in nano. Without
regular expressions the replacement is always literal.

Replacing all with an expression that can match nothing follows nano's loop: after such a match
(or with a pattern that starts with `^`) every later search starts one character further, so
`x*` replaced by `-` turns `abxxc` into `-a-b-c-`.

The classes are those of the C library in a UTF-8 locale: `[[:alpha:]]` is the Unicode
Alphabetic property and the decimal digits other than 0-9, `[[:alnum:]]` the Alphabetic property
and all decimal digits, `[[:space:]]` the White_Space property without the no-break spaces
U+00A0, U+2007 and U+202F. `\w` and `\s` are `[_[:alnum:]]` and `[[:space:]]`. A byte that is not
valid UTF-8 is one unit for the matching, and is matched by no class and by no negated one.

## Differences that cannot be translated

- **Back-references in the pattern** (`(a)\1`) are not supported: the `regex` crate leaves them
  out, because they cannot be matched in linear time. The editor says `Bad regex "(a)\1":
  Back-references in the pattern are not supported`, in a plain search at once and in a replace
  once the replacement has been typed; nano matches them. A `\1` with no group before it is
  `Invalid back reference` in both. Scenario: `regexp-pattern-backreference`; its `pending:` mark
  is permanent: the editor shows the cursor at that point and nano hides it, and this is a
  deliberate difference that will not be fixed.
- **Leftmost-longest.** POSIX takes the longest match of an alternation (`a|ab` on `ab` matches
  `ab`); the `regex` crate takes the first alternative that matches (`a`). Put the longer
  alternative first.
- **Case-blind regular expressions** fold with the full Unicode rules of the `regex` crate, the C
  library does not: `k` also matches the Kelvin sign (U+212A), which nano
  does not match.
- **Other classes and word boundaries.** `[[:upper:]]`, `[[:lower:]]`, `[[:punct:]]`,
  `[[:blank:]]`, `[[:print:]]` and `[[:graph:]]` use Unicode properties that may differ from the
  tables of the C library for some rare characters, as do `\b`, `\B`, `\<` and `\>`, which take
  their idea of a word character from the `regex` crate.
- **Error texts** are those of the GNU C library for the mistakes `translate` finds itself (see
  `bad_expressions_give_the_message_of_nano` in `src/search.rs`). A mistake only the `regex`
  crate finds is `Invalid regular expression`, or `Regular expression too big`.
- **Lazy repetition.** In POSIX `a*?` is `(a*)?`; the editor translates it that way, so it is
  never lazy.
