//! The icon a file or a directory wears, and the element that draws it.
//!
//! These are Catppuccin's own drawings, the ones the app this one replaces drew in its file tree
//! and on the last step of a file crumb, so a row here wears the icon that app gave it. The
//! drawings and the rules that choose between them are in [`data`], written by
//! `scripts/generate-native-file-icons.mjs` out of the vendored Catppuccin module.
//!
//! # Why one icon is several layers
//!
//! GPUI rasterizes an SVG to a single-colour alpha mask: `SvgRenderer::render_alpha_mask` keeps
//! `pixel.alpha()` and `Svg` paints the result with one `text_color`. A stroke's own paint is read
//! nowhere, and that is what makes the generated table a cut rather than a copy. An icon that
//! paints with three inks is three layers, each a complete SVG, and [`element`] stacks them in one
//! box and tints each with the role its ink names: a drawing that is two colours stays two colours,
//! and one that is eight stays eight.
//!
//! Layers stack rather than compose, so two strokes that cross are painted in the order their inks
//! are first met instead of the order the file wrote them. Over one-pixel strokes on a sixteen-unit
//! grid that is not visible, and it is the only way a toolkit that tints with one colour at a time
//! draws a multicoloured icon at all.
use gpui_kit::component::Icon;
use gpui_kit::{
    div, px, InteractiveElement, IntoElement, ParentElement, Pixels, SharedString, Styled,
    TestSupportExt,
};

use crate::theme::{Ink, Roles};

mod data;

/// One ink's half of a drawing, as a complete SVG.
///
/// The paint in the markup is `#000` and means nothing: only the alpha is ever read, and
/// [`Roles::ink`] is what this layer is actually painted with. The ink is a role name rather than
/// a value because the values are this app's, in [`crate::theme`].
pub(crate) struct Layer {
    pub(crate) ink: Ink,
    pub(crate) markup: &'static str,
}

/// One drawing, cut into the inks it paints with.
///
/// In the order the drawings first ask for them, so a layer laid over another is the one Legacy
/// lays over it. Never empty: every id in the table paints something.
pub(crate) struct FileIcon {
    pub(crate) layers: &'static [Layer],
}

/// The icon `name` wears, by the rules Legacy resolves a name with.
///
/// A directory answers with the icon its own name has, or with the plain folder. A file answers
/// with the icon the name before its first dot has, then with the icon its extension has, and
/// with the plain file when neither rule reaches.
pub(crate) fn icon_for(name: &str, directory: bool) -> &'static FileIcon {
    let id = if directory {
        rule(data::NAMED_DIRECTORY, name).unwrap_or(data::DIRECTORY)
    } else {
        file_icon_id(name)
    };
    let icon = data::ICONS
        .binary_search_by(|(name, _)| name.cmp(&id))
        .ok()
        .map(|at| &data::ICONS[at].1)
        .expect("every id the rules name is in the table the rules were read with");
    debug_assert!(
        !icon.layers.is_empty(),
        "{id} is in the table but paints nothing"
    );
    icon
}

/// The file icon a name resolves to, in the order a name is asked.
fn file_icon_id(name: &str) -> &'static str {
    if let Some(stem) = rule(data::FILE_STEM, stem_of(name)) {
        return stem;
    }
    match name.rfind('.') {
        // `dot > 0` and not `>= 0`: a dotfile's only dot is its first character, so a name that
        // starts with one has no extension to ask about. `.gitignore` is a plain file in Legacy
        // and is a plain file here.
        Some(dot) if dot > 0 => rule(data::FILE_SUFFIX, &name[dot + 1..]).unwrap_or(data::FILE),
        _ => data::FILE,
    }
}

/// The name before its first dot, as Legacy asks for it.
///
/// `slice(0, -1)` on a name with no dot is not a no-op in JavaScript: it drops the last character,
/// so `LICENSEX` finds the `LICENSE` rule and wears the licence icon, while `LICENSE` itself
/// becomes `LICENS` and finds nothing. Ported as it is rather than tidied up, because a row that
/// draws a different icon from the app it replaces is the drift this port exists to close.
fn stem_of(name: &str) -> &str {
    match name.find('.') {
        Some(dot) => &name[..dot],
        // `next_back` is the index of the last character, so slicing there drops
        // exactly that one, which is what a slice to `-1` does.
        None => name
            .char_indices()
            .next_back()
            .map_or("", |(at, _)| &name[..at]),
    }
}

fn rule<'a>(table: &'a [(&'static str, &'static str)], key: &str) -> Option<&'a str> {
    table
        .binary_search_by(|(name, _)| (*name).cmp(key))
        .ok()
        .map(|at| table[at].1)
}

/// The icon for `name`, in a box of `size`, as `id` so a headless test can find it.
///
/// The layers lie on top of one another inside the box rather than beside each other, so an icon
/// keeps its shape and its alignment in the row it sits in however many inks it paints with. It
/// carries an id but no handler: the row that owns it is what answers the pointer.
pub(crate) fn element(
    id: impl Into<SharedString>,
    name: &str,
    directory: bool,
    size: Pixels,
    roles: &Roles,
) -> impl IntoElement {
    div()
        .id(id.into())
        .test_support()
        .relative()
        .flex_shrink_0()
        .size(size)
        .children(icon_for(name, directory).layers.iter().map(|layer| {
            Icon::default()
                .data(layer.markup.as_bytes())
                .size(size)
                .absolute()
                .left(px(0.))
                .top(px(0.))
                .text_color(roles.ink(layer.ink))
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The inks `name` is drawn in, which is how a test says what icon it wears without holding
    /// on to an id the table happens to use today.
    fn inks(name: &str, directory: bool) -> Vec<Ink> {
        icon_for(name, directory)
            .layers
            .iter()
            .map(|layer| layer.ink)
            .collect()
    }

    #[test]
    fn a_named_directory_wears_the_icon_its_name_names() {
        assert_eq!(inks("src", true), vec![Ink::Text, Ink::Green]);
        assert_eq!(inks("srcs", true), inks("src", true));
        assert_eq!(inks("node_modules", true), inks("src", true));
        assert_ne!(inks("docs", true), inks("src", true));
    }

    #[test]
    fn a_directory_name_is_matched_whole() {
        assert_eq!(inks(".github", true), vec![Ink::Text, Ink::Overlay1]);
        assert_eq!(inks("github", true), inks(".github", true));
        assert_ne!(inks("github", true), inks("docs", true));
    }

    #[test]
    fn a_directory_no_rule_names_wears_the_plain_folder() {
        assert_eq!(data::DIRECTORY, "_folder");
        assert_eq!(inks("a-name-nothing-claims", true), vec![Ink::Text]);
    }

    #[test]
    fn a_file_wears_the_icon_its_extension_names() {
        assert_eq!(inks("main.rs", false), vec![Ink::Peach]);
        assert_ne!(inks("main.rs", false), inks("main.ts", false));
        assert_eq!(inks("notes.md", false), vec![Ink::Sapphire]);
    }

    #[test]
    fn the_name_before_the_first_dot_is_asked_before_the_extension() {
        assert_eq!(inks("README.md", false), vec![Ink::Teal]);
        assert_ne!(inks("README.md", false), inks("guide.md", false));
    }

    /// Neither thing a file name is asked about can hold a dot: what is before its first dot, and
    /// what is after its last one. A key with a dot in it is therefore a key no file name can be
    /// asked about, which is why the generator drops it. Directories are exempt, because a
    /// directory is asked about by its whole name and `.github` is a real one. If the resolver
    /// ever starts asking something else, this fails before a dropped key is silently missed.
    #[test]
    fn no_file_rule_key_carries_a_dot() {
        for (name, table) in [
            ("FILE_STEM", data::FILE_STEM),
            ("FILE_SUFFIX", data::FILE_SUFFIX),
        ] {
            for (key, _) in table {
                assert!(
                    !key.contains('.'),
                    "{name} asks about {key}, which no name can be"
                );
            }
        }
        // The directories that do carry one are kept, and reachable.
        assert!(data::NAMED_DIRECTORY
            .iter()
            .any(|(name, _)| *name == ".github"));
        assert_eq!(inks(".github", true), vec![Ink::Text, Ink::Overlay1]);
    }

    /// What is left of the rules is still what a name asks. `Cargo.toml` reaches the toml icon by
    /// its extension, and `Cargo.lock` reaches nothing: the theme's own `Cargo.lock` rule is a stem
    /// key with a dot in it, so no name ever asked about it, and there is no `lock` extension.
    #[test]
    fn a_name_resolves_by_the_rules_that_survive() {
        assert_eq!(inks("Cargo.toml", false), vec![Ink::Maroon]);
        assert_eq!(inks("Cargo.lock", false), vec![Ink::Text]);
        assert_ne!(inks("Cargo.toml", false), inks("Cargo.lock", false));
    }

    #[test]
    fn a_name_no_rule_reaches_is_a_plain_file() {
        assert_eq!(data::FILE, "_file");
        assert_eq!(inks("a.qqq", false), vec![Ink::Text]);
        assert_eq!(inks("LICENSE", false), vec![Ink::Text]);
    }

    /// Legacy slices to `-1` when a name has no dot, which drops its last character: `LICENSEX`
    /// asks about `LICENSE` and wears the licence icon, and `LICENSE` asks about `LICENS` and
    /// wears the plain one. The asymmetry is the port's, not a tidy-up.
    #[test]
    fn a_dotless_name_asks_about_itself_without_its_last_character() {
        assert_eq!(inks("LICENSEX", false), vec![Ink::Peach]);
        assert_ne!(inks("LICENSEX", false), inks("LICENSE", false));
        assert_eq!(stem_of("LICENSEX"), "LICENSE");
        assert_eq!(stem_of("a"), "");
        assert_eq!(stem_of(""), "");
    }

    /// A dotfile's only dot is its first character, so it has no extension to ask about and wears
    /// the plain file. This is the one place the port could have improved on Legacy without being
    /// asked, so it is pinned.
    #[test]
    fn a_dotfile_has_no_extension_to_ask_about() {
        assert_eq!(inks(".md", false), vec![Ink::Text]);
        assert_ne!(inks(".md", false), inks("notes.md", false));
        assert_eq!(inks(".gitignore", false), vec![Ink::Text]);
    }

    /// A shape has to carry a paint the rasterizer can parse, or the layer draws nothing: the
    /// token Legacy wrote is a browser custom property and usvg has no custom properties at all.
    /// The token is written as `#000`, which is why there is nothing to resolve here at all — the
    /// ink is the role on the same line, and only this shape's alpha ever reaches the screen.
    #[test]
    fn every_shape_in_every_layer_carries_a_paint() {
        for (id, icon) in data::ICONS {
            assert!(!icon.layers.is_empty(), "{id} paints nothing");
            for layer in icon.layers {
                assert!(
                    !layer.markup.contains("var(--muster-icon"),
                    "{id} still carries a custom property usvg cannot resolve"
                );
                assert!(
                    layer.markup.contains("stroke:#000") || layer.markup.contains("fill:#000"),
                    "{id} has a layer with nothing painted in it"
                );
            }
        }
    }

    /// The cut is the whole point: a drawing that paints two inks is two elements in the window,
    /// not one tinted with a colour picked out of them.
    #[test]
    fn a_drawing_that_paints_two_inks_is_two_layers() {
        assert_eq!(inks("site.astro", false), vec![Ink::Text, Ink::Mauve]);
        assert_eq!(inks("app.py", false), vec![Ink::Blue, Ink::Yellow]);
        assert_eq!(inks("main.rs", false).len(), 1);
    }
}
