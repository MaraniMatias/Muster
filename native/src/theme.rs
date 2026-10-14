//! Legacy's dark colour vocabulary, installed into the toolkit's theme.
//!
//! Every surface, ink, hairline and state wash the window draws is named here
//! once and taken from [`Theme`], which is the same theme GPUI Kit's own
//! components paint with. A row drawn by hand and a menu drawn by the toolkit
//! are therefore the same colour rather than two constants that agree today.
//!
//! ## Dark only
//!
//! [`install`] applies the dark tokens from `legacy/src/muster.css` before the
//! first window opens. There is no preference, switch or system follow.
//!
//! ## Not the terminal's palette
//!
//! [`crate::terminal::Palette`] is a different contract and is untouched by
//! this module. That palette is what a VT sequence asked for — ANSI indices,
//! OSC colours, an explicit background — and the application must draw it
//! verbatim. These roles are the chrome *around* that grid. Nothing here reads
//! or writes terminal colours, and nothing in the terminal reads these roles.

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{rgb, rgba, App, Hsla};

const TEXT_DISABLED: u32 = 0x565b67;
const BORDER_STRONG: u32 = 0x3a404c;
const CONTENT_ADDED: u32 = 0xa1c181;
const CONTENT_REMOVED: u32 = 0xd97f84;

/// Install Legacy dark for both toolkit widgets and application-owned drawing.
/// `Theme::update` also synchronizes renderable tokens and the Base layer.
pub fn install(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    Theme::update(cx, |theme| {
        let base: Hsla = rgb(0x16181d).into();
        let panel = rgb(0x1c1f26).into();
        let control = rgb(0x242830).into();
        let hover = rgb(0x2d323c).into();
        let pressed = rgb(0x363c48).into();
        let selected = rgb(0x2c3b57).into();
        let text = rgb(0xd7dae0).into();
        let muted = rgb(0x8b909c).into();
        let border = rgb(0x2c313b).into();
        let accent: Hsla = rgb(0x5b97ff).into();
        let accent_text = rgb(0x0d1626).into();
        let danger: Hsla = rgb(0xf07178).into();
        let danger_text = rgb(0xf7adb1).into();
        let success = rgb(0x5fc98a).into();
        let warning = rgb(0xe3b341).into();
        let transparent = gpui_kit::transparent_black();

        theme.background = base;
        theme.foreground = text;
        // One hairline everywhere the toolkit draws an edge: `border` for the
        // overlays it paints itself, `input` for a field and `sidebar_border`
        // for the panel, all three Legacy's `--muster-border`.
        theme.border = border;
        theme.input = border;
        theme.muted = control;
        theme.muted_foreground = muted;
        theme.popover = control;
        theme.popover_foreground = text;
        theme.sidebar = panel;
        theme.sidebar_foreground = text;
        theme.sidebar_border = border;
        theme.sidebar_accent = selected;
        theme.sidebar_accent_foreground = text;
        theme.sidebar_primary = accent;
        theme.sidebar_primary_foreground = accent_text;
        theme.title_bar = panel;
        theme.title_bar_border = border;
        theme.accent = hover;
        theme.accent_foreground = text;
        theme.primary = accent;
        // Legacy's filled buttons use CSS brightness, not an HSL lightening.
        let brightness = |color: Hsla, factor: f32| {
            let mut color = color.to_rgb();
            color.r = (color.r * factor).min(1.);
            color.g = (color.g * factor).min(1.);
            color.b = (color.b * factor).min(1.);
            Hsla::from(color)
        };
        theme.primary_hover = brightness(accent, 1.08);
        theme.primary_active = brightness(accent, 0.94);
        theme.primary_foreground = accent_text;
        theme.button_primary = accent;
        theme.button_primary_hover = theme.primary_hover;
        theme.button_primary_active = theme.primary_active;
        theme.button_primary_foreground = accent_text;
        theme.secondary = control;
        theme.secondary_hover = hover;
        theme.secondary_active = pressed;
        theme.secondary_foreground = text;
        theme.button = control;
        theme.button_hover = hover;
        theme.button_active = pressed;
        theme.button_foreground = text;
        theme.button_secondary = control;
        theme.button_secondary_hover = hover;
        theme.button_secondary_active = pressed;
        theme.button_secondary_foreground = text;
        // Legacy danger actions are outlined, not a solid red fill.
        theme.danger = danger;
        theme.danger_foreground = danger_text;
        theme.danger_hover = danger.alpha(0.12);
        theme.danger_active = danger.alpha(0.20);
        theme.button_danger = transparent;
        theme.button_danger_hover = theme.danger_hover;
        theme.button_danger_active = theme.danger_active;
        theme.button_danger_foreground = danger_text;
        theme.success = success;
        theme.success_foreground = success;
        theme.warning = warning;
        theme.warning_foreground = warning;
        // Legacy has no separate info token; informational chrome uses accent.
        theme.info = accent;
        theme.info_foreground = accent;
        theme.ring = accent;
        theme.caret = rgb(0xc2c9f1).into();
        theme.selection = rgba(0x74ade83d).into();
        theme.colors.list = control;
        theme.list_hover = hover;
        theme.list_active = selected;
        theme.list_active_border = accent;
        theme.list_head = control;
        theme.list_even = control;
        theme.drag_border = accent;
        theme.drop_target = selected;
        theme.scrollbar = transparent;
        theme.scrollbar_thumb = rgba(0xc8ccd44c).into();
        theme.scrollbar_thumb_hover = theme.scrollbar_thumb;
        theme.overlay = base.alpha(0.8);
        theme.link = accent;
        theme.link_hover = accent;
        theme.link_active = accent;
        theme.window_border = border;
    });
}

/// Ink the toolkit paints a state with, where a state has one meaning only.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Status {
    /// A line this workspace added.
    pub added: Hsla,
    /// A line this workspace removed.
    pub removed: Hsla,
    /// Something is waiting on the person rather than failing.
    pub warning: Hsla,
    /// Something failed and nothing recovers on its own.
    pub danger: Hsla,
    /// Readable error ink, distinct from the danger border.
    pub danger_text: Hsla,
    /// Something the workspace just finished.
    pub success: Hsla,
    /// Something worth reading that is neither of the three above.
    pub info: Hsla,
}

/// One colour per meaning the window can draw.
///
/// Named for what the colour is *for*, so a call site reads as a decision
/// rather than as a value: `roles.selected_hover` is a state, and the only
/// way to break it is to paint the wrong state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Roles {
    // ---- surfaces -------------------------------------------------------
    /// The application background and the fill of a text field.
    pub surface_base: Hsla,
    /// The window's own base: the sidebar panel and the bar across the top.
    pub surface: Hsla,
    /// A surface lifted off the base: a menu, a popover, a row being dragged.
    pub surface_raised: Hsla,
    /// A well inset into a raised surface: a preview block, a dialog body.
    pub surface_inset: Hsla,
    // ---- ink ------------------------------------------------------------
    /// Primary ink: a name, a label, a title.
    pub text: Hsla,
    /// Secondary ink: a path, a hint, a control at rest.
    pub text_muted: Hsla,
    /// Ink for something that cannot be acted on.
    pub text_disabled: Hsla,
    // ---- lines ----------------------------------------------------------
    /// A hairline: a separator, a panel edge, a row's guide.
    pub border: Hsla,
    /// The stronger hairline that closes an overlay.
    pub border_strong: Hsla,
    // ---- pointer and keyboard states ------------------------------------
    /// The pointer is on something actionable and nothing is selected.
    pub hover: Hsla,
    /// A control is being held down.
    ///
    /// A press belongs to a control, not to a row: a row's press is a selection
    /// gesture that finishes in one click, while a control stays held for as
    /// long as the pointer is on it. This is the colour the toolkit paints for
    /// every custom button variant this window installs — the row action, the
    /// row close, the dismiss control and a crumb menu's own action.
    pub pressed: Hsla,
    /// This window's open row, at rest.
    pub selected: Hsla,
    /// This window's open row, with the pointer on it.
    ///
    /// Its own role rather than [`hover`](Self::hover): GPUI refines a hover
    /// style after the rest style, so a selected row that also answered the
    /// plain hover would stop looking selected for exactly as long as the
    /// pointer was on it.
    pub selected_hover: Hsla,
    /// The ring a keyboard focus paints.
    ///
    /// Not drawn by hand anywhere in this window: every control here is a
    /// toolkit control, and the toolkit paints its focus ring with this colour.
    /// The role exists so the window has one name for it, and a test below
    /// stops the name and the ring from drifting apart.
    pub focus: Hsla,
    /// The accent bar on the selected row, and the tick on a chosen crumb.
    pub accent: Hsla,
    /// The wash a control that cannot be used wears instead of its own.
    pub disabled_surface: Hsla,
    // ---- meaning --------------------------------------------------------
    pub status: Status,
    /// The marker a row drag draws: the slot's outline and its label.
    pub drag: Hsla,
}

impl Roles {
    /// Read the dark theme's roles once, at the top of a frame.
    pub fn of(cx: &App) -> Self {
        Self::from(Theme::global(cx))
    }

    /// Read installed toolkit colours without inventing extra tints.
    pub fn from(theme: &Theme) -> Self {
        let hover = theme.secondary_hover;
        Self {
            surface_base: theme.background,
            surface: theme.sidebar,
            surface_raised: theme.popover,
            // Legacy's preview wells inside dialogs use bg-1.
            surface_inset: theme.sidebar,
            text: theme.sidebar_foreground,
            text_muted: theme.muted_foreground,
            text_disabled: rgb(TEXT_DISABLED).into(),
            border: theme.sidebar_border,
            border_strong: rgb(BORDER_STRONG).into(),
            hover,
            pressed: theme.secondary_active,
            selected: theme.list_active,
            // Legacy's selected rule wins over hover, with the same fill.
            selected_hover: theme.list_active,
            focus: theme.ring,
            // The theme's own accent, which is what its primary controls wear.
            accent: theme.primary,
            disabled_surface: theme.muted,
            status: Status {
                added: rgb(CONTENT_ADDED).into(),
                removed: rgb(CONTENT_REMOVED).into(),
                warning: theme.warning,
                danger: theme.danger,
                danger_text: theme.danger_foreground,
                success: theme.success,
                info: theme.info,
            },
            drag: theme.drag_border,
        }
    }
}

impl Roles {
    /// The background a row wears at rest.
    ///
    /// A row that is not open has no surface of its own: it inherits the
    /// panel, so a panel of unopened rows reads as one surface.
    pub fn row_rest(&self, selected: bool) -> Hsla {
        if selected {
            self.selected
        } else {
            self.surface
        }
    }

    /// The background a row wears with the pointer on it.
    ///
    /// GPUI refines a hover style after the rest style, so one hover closure
    /// has to answer both cases. Answering the open row with the plain hover is
    /// what makes a selected row stop looking selected for exactly as long as
    /// the pointer is resting on it.
    pub fn row_hover(&self, selected: bool) -> Hsla {
        if selected {
            self.selected_hover
        } else {
            self.hover
        }
    }
}

/// `top` painted over `base`, as one opaque colour.
///
/// The toolkit's alpha is authored against a surface this module may not be
/// painting on, so a role that has to answer a contrast question is resolved
/// once, here, against the surface that role is actually drawn over.
pub fn over(base: Hsla, top: Hsla) -> Hsla {
    base.blend(top)
}

/// WCAG relative luminance, for asking whether two roles can be read.
pub fn luminance(color: Hsla) -> f32 {
    let rgba = color.to_rgb();
    0.2126 * channel(rgba.r) + 0.7152 * channel(rgba.g) + 0.0722 * channel(rgba.b)
}

/// WCAG contrast ratio between two roles, from 1 to 21.
///
/// Takes opaque colours: a translucent one has no single luminance until it is
/// composited, and [`over`] is the composite.
pub fn contrast(first: Hsla, second: Hsla) -> f32 {
    let (first, second) = (luminance(first), luminance(second));
    let (high, low) = if first > second {
        (first, second)
    } else {
        (second, first)
    };
    (high + 0.05) / (low + 0.05)
}

fn channel(value: f32) -> f32 {
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;

    fn dark(cx: &mut TestAppContext) -> Roles {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install(cx);
            Roles::of(cx)
        })
    }

    // Only the fallback dark block is read, never the light overrides.
    fn legacy(token: &str) -> Hsla {
        let css = include_str!("../../legacy/src/muster.css");
        let dark = css.split(":root[data-theme=\"light\"]").next().unwrap();
        let prefix = format!("--muster-{token}: #");
        let value = dark
            .lines()
            .find_map(|line| line.trim().strip_prefix(&prefix))
            .unwrap_or_else(|| panic!("missing dark token: {token}"))
            .trim_end_matches(';');
        let value_number = u32::from_str_radix(value, 16).unwrap();
        if value.len() == 8 {
            rgba(value_number).into()
        } else {
            rgb(value_number).into()
        }
    }

    #[gpui_kit::test]
    fn every_role_matches_legacy_dark(cx: &mut TestAppContext) {
        let roles = dark(cx);
        for (role, token) in [
            (roles.surface_base, "bg-0"),
            (roles.surface, "bg-1"),
            (roles.surface_raised, "bg-2"),
            (roles.surface_inset, "bg-1"),
            (roles.text, "text"),
            (roles.text_muted, "text-muted"),
            (roles.text_disabled, "text-disabled"),
            (roles.border, "border"),
            (roles.border_strong, "border-strong"),
            (roles.hover, "el-hover"),
            (roles.pressed, "el-active"),
            (roles.selected, "el-selected"),
            (roles.selected_hover, "el-selected"),
            (roles.focus, "focus"),
            (roles.accent, "accent"),
            (roles.disabled_surface, "el"),
            (roles.drag, "accent"),
            (roles.status.added, "content-added"),
            (roles.status.removed, "content-removed"),
            (roles.status.warning, "warning"),
            (roles.status.danger, "danger"),
            (roles.status.danger_text, "danger-fg"),
            (roles.status.success, "success"),
            (roles.status.info, "accent"),
        ] {
            assert_eq!(role, legacy(token), "role differs from --muster-{token}");
            assert!(role.is_opaque(), "{token} must be opaque");
        }
    }

    #[gpui_kit::test]
    fn toolkit_widgets_use_the_same_palette_and_renderable_tokens(cx: &mut TestAppContext) {
        dark(cx);
        cx.update(|cx| {
            let theme = Theme::global(cx);
            assert_eq!(theme.mode, ThemeMode::Dark);
            for (color, token, reference) in [
                (theme.background, theme.tokens.background, "bg-0"),
                (theme.popover, theme.tokens.popover, "bg-2"),
                (theme.sidebar, theme.tokens.sidebar, "bg-1"),
                (theme.foreground, theme.tokens.foreground, "text"),
                (theme.border, theme.tokens.border, "border"),
                (theme.input, theme.tokens.input, "border"),
                (theme.button, theme.tokens.button, "el"),
                (theme.button_hover, theme.tokens.button_hover, "el-hover"),
                (theme.button_active, theme.tokens.button_active, "el-active"),
                (theme.button_primary, theme.tokens.button_primary, "accent"),
                (
                    theme.button_primary_foreground,
                    theme.tokens.button_primary_foreground,
                    "accent-fg",
                ),
                (
                    theme.button_danger_foreground,
                    theme.tokens.button_danger_foreground,
                    "danger-fg",
                ),
                (theme.list_active, theme.tokens.list_active, "el-selected"),
                (theme.caret, theme.tokens.caret, "cursor"),
                (theme.selection, theme.tokens.selection, "selection"),
                (
                    theme.scrollbar_thumb,
                    theme.tokens.scrollbar_thumb,
                    "scrollbar",
                ),
                (theme.ring, theme.tokens.ring, "focus"),
            ] {
                assert_eq!(color, legacy(reference), "{reference}");
                assert_eq!(token.color, color, "stale toolkit token: {reference}");
                assert_eq!(
                    token.background,
                    color.into(),
                    "stale background: {reference}"
                );
            }
            assert_eq!(theme.overlay, legacy("bg-0").alpha(0.8));
            assert_eq!(theme.button_danger_hover, legacy("danger").alpha(0.12));
            assert_eq!(theme.button_danger_active, legacy("danger").alpha(0.20));
        });
    }

    #[gpui_kit::test]
    fn selection_wins_over_hover_as_in_legacy(cx: &mut TestAppContext) {
        let roles = dark(cx);
        assert_eq!(roles.row_rest(false), roles.surface);
        assert_eq!(roles.row_hover(false), roles.hover);
        assert_eq!(roles.row_rest(true), roles.selected);
        assert_eq!(roles.row_hover(true), roles.selected);
        assert_ne!(roles.hover, roles.selected);
        assert_ne!(roles.pressed, roles.hover);
        assert_ne!(roles.pressed, roles.selected);
    }

    #[gpui_kit::test]
    fn primary_ink_and_focus_remain_legible(cx: &mut TestAppContext) {
        let roles = dark(cx);
        for surface in [
            roles.surface,
            roles.surface_raised,
            roles.surface_inset,
            roles.hover,
            roles.pressed,
            roles.selected,
        ] {
            assert!(contrast(roles.text, surface) >= 4.5);
            assert!(contrast(roles.focus, surface) >= 3.);
        }
        // Legacy secondary text does not meet 4.5:1 on every state. Pin its
        // exact token above instead of silently replacing it with lighter ink.
        assert!(contrast(roles.text_muted, roles.surface) >= 4.5);
    }
}
