# Vendored `alacritty_terminal` 0.26.0

## Upstream

- Project: **Alacritty** — https://github.com/alacritty/alacritty
- Vendored crate: `alacritty_terminal`, the terminal-parsing library extracted out of Alacritty
  into its own crate.
- Exact version vendored: **0.26.0**, taken verbatim from the published crates.io archive
  (`alacritty_terminal-0.26.0.crate`).

## License

Apache-2.0. The upstream `LICENSE-APACHE` is retained verbatim alongside the sources in this
directory; `Cargo.toml` keeps its `license = "Apache-2.0"` field unchanged.

## Why this crate is vendored

The `alacritty_terminal` 0.26.0 release on crates.io is the latest published version, and it
carries a remotely triggerable panic in its terminal parser. There is no corrected published
release to depend on: upstream has not shipped a fix, so depending on the registry version
would keep shipping a crash that any program running inside a terminal can trigger.

Waiting for an upstream release is not an option either — this workspace needs a terminal
that a hostile process cannot take down. The only safe move is to vendor the exact published
sources and carry the fix ourselves, so the patch is explicit, reviewable, and limited to the
one defect.

## The defect

`push_keyboard_mode` in `src/term/mod.rs` guards the kitty keyboard mode stack against
unbounded growth: when the stack already holds `KEYBOARD_MODE_STACK_MAX_DEPTH` (4096) entries,
it drops the oldest entry before pushing a new one.

The length check consults `keyboard_mode_stack`, but the removal pops from the wrong vector.
It removes from `title_stack` instead. `title_stack` is empty unless the terminal is in
window-title mode, and in the common case nothing has ever pushed a title, so `Vec::remove(0)`
is called on an empty vector, which panics.

The panic happens on the reader/parser thread, so it is not recoverable from the application:
the parser thread dies and the terminal session stops parsing the stream it is reading.

Any program running inside the terminal can trigger it remotely, from data the application
never asked for. Kitty keyboard negotiation is enabled, so the terminal accepts the sequence,
and emitting 4096 or more `CSI > Ps u` sequences (`CSI > 1 u`, `CSI > 1 u`, …) walks
`keyboard_mode_stack` up to the limit and then takes the branch that panics. No user interaction
is required — only output from something the terminal is displaying.

## The fix

One line, in `Term::push_keyboard_mode` in `src/term/mod.rs`:

Before:

```rust
let removed = self.title_stack.remove(0);
```

After:

```rust
let removed = self.keyboard_mode_stack.remove(0);
```

The removal now targets the stack whose depth was just checked, so the evict-from-bottom
behaviour the code was already reaching for works as intended and the panic is gone. Nothing
else in the file, or anywhere else in the crate, is modified.

Verify the patch against the published sources with:

```
git diff --no-index <extracted-crate>/src/term/mod.rs native/vendor/alacritty_terminal-0.26.0/src/term/mod.rs
```

which must show exactly one changed line.

## Files removed relative to the published archive

- The `tests/` directory, in full. It is 46 MB of ref-test fixtures and is not needed to build
  the crate as a dependency.
- The `[[test]]` stanza in `Cargo.toml` (`name = "ref"`, `path = "tests/ref.rs"`), which exists
  only to point at that directory. Cargo would error on the missing path when it builds all
  targets of a path dependency, and the test target is never built as a dependency anyway.

Everything else — `src/`, `Cargo.toml`, `LICENSE-APACHE`, `README.md`, `CHANGELOG.md` — is the
published archive verbatim, apart from the two changes described above.

## Do not "fix" this by disabling kitty keyboard negotiation

Turning off kitty keyboard support would also remove the panic, because `push_keyboard_mode`
returns early when `config.kitty_keyboard` is false. **Do not do that.** It is not a fix, it is
a regression traded for one: modified-key reporting — Shift, Alt and Ctrl arriving as distinct
keys rather than as escape sequences — depends on kitty keyboard negotiation and is part of the
terminal's behaviour. Silencing the terminal to dodge a one-line bug breaks the feature the
bug lives next to, and hides the defect from every future audit instead of recording it here.