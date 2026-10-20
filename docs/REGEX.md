# Regular expressions

`M-R` in the search prompt (and `^\` or `M-R` in the main menu, for replacing) makes the text
typed a regular expression. nano uses the POSIX extended syntax of the C library; the editor
translates it (`translate` in `src/search.rs`) to the `regex` crate, whose version is pinned. The
translated constructs behave like nano's, and each has a scenario under
`tests/compare/scenarios/replace/` that runs against GNU nano 8.4:

| Construct | Scenario |
|-----------|----------|
| classes, `[[:alpha:]]`, `[[:digit:]]`, ... | `regexp-class` |
| `^` and `$`, also matching nothing | `regexp-anchor-start`, `regexp-anchor-end`, `regexp-empty-matches` |
| alternation `a\|b` and groups | `regexp-alternation` |
| intervals `{n}`, `{n,m}`, `{,m}` | `regexp-interval` |
| `\<` and `\>` word boundaries (also `\b` `\B`) | `regexp-word-boundaries` |
| `\1` to `\9` in the replacement | `backreferences`, `backreference-missing`, `backreference-literal` |
| errors, shown as `Bad regex "...": reason`, with the text of the C library | `regexp-bad-paren`, `regexp-bad-interval` |

In a replacement, `\1` to `\9` are the groups of the match (nothing for a group that did not
match); `\0`, `&`, and a group the pattern does not have stay as typed, as in nano. Without
regular expressions the replacement is always literal.

## Differences that cannot be translated

- **Back-references in the pattern** (`(a)\1`) are not supported: the `regex` crate leaves them
  out, because they cannot be matched in linear time. The editor says `Bad regex "(a)\1":
  Back-references in the pattern are not supported`, in a plain search at once and in a replace
  once the replacement has been typed; nano matches them. A `\1` with no group before it is
  `Invalid back reference` in both. Scenario: `regexp-pattern-backreference` (pending: the cursor
  is shown in the editor at that point and hidden in nano).
- **Leftmost-longest.** POSIX takes the longest match of an alternation (`a|ab` on `ab` matches
  `ab`); the `regex` crate takes the first alternative that matches (`a`). Put the longer
  alternative first.
- **Unicode.** Classes such as `[[:alpha:]]` and the case folding of a case-blind search follow
  Unicode, not the locale of the C library.
- **Error texts** are those of the GNU C library for the mistakes `translate` finds itself (see
  `bad_expressions_give_the_message_of_nano` in `src/search.rs`). A mistake only the `regex`
  crate finds is `Invalid regular expression`, or `Regular expression too big`.
- **Lazy repetition.** In POSIX `a*?` is `(a*)?`; the editor translates it that way, so it is
  never lazy.
