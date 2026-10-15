# Legacy dark → native colour map

The reference is the fallback dark block in `legacy/src/muster.css`, not GPUI
Kit's default dark theme. `native/src/theme.rs::install` installs these colours
into GPUI Kit with `Theme::update`, keeping its solid colours, renderable tokens
and Base projection synchronized. `main.rs` calls it before opening a window.
No light palette or appearance preference is introduced.

## Application chrome

All Legacy token names below have the `--muster-` prefix.

| Legacy token | Dark value | Native equivalent |
| --- | --- | --- |
| `bg-0` | `#16181d` | `surface_base`; application background, worktree text fields |
| `bg-1` | `#1c1f26` | `surface`, `surface_inset`; sidebar, title bar, dialog preview wells |
| `bg-2`, `el`, `control-bg` | `#242830` | `surface_raised`, `disabled_surface`; menus, dialogs, controls |
| `border` | `#2c313b` | `border`; separators, input borders, sidebar edges |
| `border-strong` | `#3a404c` | `border_strong`; dialog and overlay edges this app draws |
| `text` | `#d7dae0` | `text`; toolkit foreground |
| `text-muted`, `text-secondary`, `text-dim`, `text-faint`, `placeholder-fg` | `#8b909c` | `text_muted`; secondary labels and placeholders |
| `text-disabled` | `#565b67` | `text_disabled`; disabled controls |
| `el-hover`, `control-hover` | `#2d323c` | `hover`; hovered controls and highlighted menu options |
| `el-active`, `control-pressed` | `#363c48` | `pressed` |
| `el-selected` | `#2c3b57` | `selected` **and** `selected_hover`; hover does not repaint selection |
| `accent`, `focus`, `control-focus` | `#5b97ff` | `accent`, `focus`, `drag`; primary fill and focus ring |
| `accent-fg` | `#0d1626` | Primary button foreground |
| `danger`, `control-danger` | `#f07178` | `status.danger`; danger border |
| `danger-fg` | `#f7adb1` | `status.danger_text`; error text and failed-session glyph |
| `success`, `success-fg` | `#5fc98a` | `status.success` |
| `warning`, `warning-fg` | `#e3b341` | `status.warning` |
| `content-added` | `#a1c181` | `status.added`; Git additions, not generic success |
| `content-removed` | `#d97f84` | `status.removed`; Git removals, not generic errors |
| `scrollbar` | `#c8ccd44c` | Toolkit scrollbar thumb; also the terminal's own overlay thumb |

Legacy has no separate information token: `status.info` uses its accent.
Filled button hover/press follow Legacy's sRGB brightness factors, 1.08/0.94.
Danger buttons have a transparent rest fill, with danger at 12%/20% on hover/press.
The modal scrim is `bg-0` at 80% alpha.

Legacy's secondary text does not reach 4.5:1 on all row fills (3.51:1 on the
selected fill). Matching Legacy is intentional; tests pin the original tokens
rather than silently substituting a lighter palette. Primary text and focus
contrast checks remain in place.

## Terminal

`native/src/terminal/view.rs` owns terminal defaults separately from application
chrome. Runtime VT/OSC colours and explicitly coloured cells still take priority.

| Legacy token | Dark value | Native equivalent |
| --- | --- | --- |
| `bg-0` | `#16181d` | `Palette::background`; also the default OSC 11 reply. Legacy used `content-bg-0` `#222436` here — a violet content well, kept for its editor and diff; the grid is the body of this window and wears the surface the chrome around it already has |
| `content-text` | `#dce0e5` | `Palette::foreground` |
| `cursor` | `#c2c9f1` | Default cursor colour; also toolkit text caret |
| `selection` | `#74ade83d` | Translucent selection band; glyph colours are preserved |

The block cursor retains its dynamic foreground/background inversion, focus and
visibility behavior, and application-supplied cursor colour. It is **not** replaced
with an unconditional lavender rectangle. The default colour remains available
for blank cells, non-block cursors and the unfocused outline.

| ANSI | Normal | Bright |
| --- | --- | --- |
| Black | `#282c34` | `#636d83` |
| Red | `#e06c75` | `#ea858b` |
| Green | `#98c379` | `#aad581` |
| Yellow | `#e5c07b` | `#ffd885` |
| Blue | `#61afef` | `#85c1ff` |
| Magenta | `#c678dd` | `#d398eb` |
| Cyan | `#56b6c2` | `#6ed5de` |
| White | `#abb2bf` | `#fafafa` |

ANSI 16–255 retain the standard xterm colour cube and greyscale ramp.

## Reference colours without a corresponding native surface yet

Do not substitute these for the application's chrome or add unused native
constants. The native app now has a read-only syntax preview, a unified diff
and Legacy's own file icons. Content background/foreground, gutter ink and syntax
colors are installed in `theme.rs::content_highlight_theme`; the file icon inks
are `theme::Ink`, answered by `Roles::ink`, one per ink a drawing names rather
than one per extension a name has. The syntax values and the icon values below
are therefore implemented, not pending. Full editor behavior remains open.

| Legacy group | Tokens and dark values |
| --- | --- |
| Tinted accent | `accent-tint #1f2f4d`, `accent-tint-fg #83b1ff` |
| Content surfaces | `content-bg-1 #2f343e`, `content-bg-2 #363c46`, `content-border #464b57` |
| Content secondary ink | `content-text-muted #a9afbc`, `content-text-faint #767d8d`, `content-accent #74ade8` |
| File icons: neutral | `icon-text #cdd6f4`, `icon-overlay1 #7f849c` |
| File icons: warm | `icon-red #f38ba8`, `icon-maroon #eba0ac`, `icon-flamingo #f2cdcd`, `icon-rosewater #f5e0dc`, `icon-pink #f5c2e7`, `icon-peach #fab387` |
| File icons: cool | `icon-mauve #cba6f7`, `icon-lavender #b4befe`, `icon-blue #89b4fa`, `icon-sky #89dceb`, `icon-sapphire #74c7ec`, `icon-teal #94e2d5` |
| File icons: other | `icon-green #a6e3a1`, `icon-yellow #f9e2af` |
| Syntax base | `syntax-foreground #acb2be`, `syntax-token-comment #878e98`, `syntax-token-parameter #acb2be`, `syntax-token-punctuation #b2b9c6` |
| Syntax colour | `syntax-token-constant #dfc184`, `syntax-token-keyword #bd81d6`, `syntax-token-string #a1c181`, `syntax-token-function #73ade9`, `syntax-token-string-expression #dfc184`, `syntax-token-link #74ade8` |
| Syntax changes | `syntax-token-inserted #98c379`, `syntax-token-deleted #e06c75`, `syntax-token-changed #dec184` |

## Remaining differences

GPUI Kit draws every overlay edge, including a menu's, with `theme.border`,
which is Legacy's `--muster-border`, the same hairline a field and the sidebar
take. Legacy gives its menus the stronger `--muster-border-strong`; the toolkit
has no separate token for it, so that one difference is accepted rather than
paid for with a different border everywhere else.

The two menu rows that can be out of range are drawn by this app with the
disabled token, because the toolkit's own disabled row keeps the secondary
foreground.

Two behavioural differences are not colours and are left in place:

- The terminal's scrollbar thumb is painted and tracks the viewport with
  Legacy's arithmetic, but it is not a hit target: Legacy's thumb can be
  dragged, and the native one cannot yet. It also does not widen on hover.
- Legacy's keyboard-highlighted menu row adds a focus ring around the wash.
  Native keeps the wash and the toolkit's own focus handling, without that ring.

The toolkit diff still paints change washes from `theme.success`/`theme.danger`
at 12% rather than Legacy's content-added/content-removed mixtures, and uses
muted chrome ink for line numbers rather than the content gutter token.
See `interface-parity.md` for typography, geometry and grammar limitations.

No interactive screenshot comparison has been performed. This map and the tests
verify colour values and state rules, not pixel-for-pixel geometry or shadows.
