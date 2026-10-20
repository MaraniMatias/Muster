//! The native app's dark design system, installed into the toolkit's theme.
//!
//! Every surface, ink, hairline and state wash the window draws is named here
//! once and taken from [`Theme`], which is the same theme GPUI Kit's own
//! components paint with. A row drawn by hand and a menu drawn by the toolkit
//! therefore share one palette and state language.
//!
//! ## Shape: Zed's radii, the toolkit's sizes
//!
//! GPUI Kit is not the widget library Zed draws with — Zed's buttons and
//! overlays live in its own unpublished crate, and the toolkit's widgets are
//! a different design system on the same `gpui` core. So the shape of a
//! control is set here too: [`install`] gives the theme Zed's two corner
//! radii (4px for a control, 8px for an overlay) and [`zed_button`] gives every
//! button Zed's borderless fill. What the toolkit still decides on its own is
//! the *size* ladder — 20/24/32px where Zed's is 22/28/32 — because it hardcodes
//! those in each widget's render and no theme token reaches them.
//!
//! ## Dark only
//!
//! [`install`] applies the app's dark tokens before the first window opens.
//! There is no preference, switch or system-appearance follow.
//!
//! ## Not the terminal's palette
//!
//! [`crate::terminal::Palette`] is a different contract and is untouched by
//! this module. That palette is what a VT sequence asked for — ANSI indices,
//! OSC colours, an explicit background — and the application must draw it
//! verbatim. These roles are the chrome *around* that grid. Nothing here reads
//! or writes terminal colours, and nothing in the terminal reads these roles.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::highlighter::{HighlightTheme, HighlightThemeStyle, LanguageConfig};
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{px, rgb, rgba, App, ElementId, Hsla, Styled};

const TEXT_DISABLED: u32 = 0x565b67;
const BORDER_STRONG: u32 = 0x3a404c;
const CONTENT_ADDED: u32 = 0xa1c181;
const CONTENT_REMOVED: u32 = 0xd97f84;

// The content ink and the syntax tokens, `--muster-content-*` and
// `--muster-syntax-*`, named as the old theme named them. The tokens are named
// per meaning, not per colour, and a grammar's capture is one of those
// meanings spelled its own way.
//
// The content *surface* is deliberately not one of them. Legacy's
// `--muster-content-bg-0`, `#222436`, is a violet-tinted well for a document
// drawn inside a page; this window's body is the terminal's grid, which Legacy
// already paints in `--muster-bg-0`. The document and the diff are that body
// too, so all three share the base the chrome is painted on rather than
// carrying a second background of their own.
const CONTENT_TEXT: u32 = 0xdce0e5;
const CONTENT_TEXT_FAINT: u32 = 0x767d8d;
const CONTENT_ACCENT: u32 = 0x74ade8;
const SYNTAX_FOREGROUND: u32 = 0xacb2be;
const SYNTAX_COMMENT: u32 = 0x878e98;
const SYNTAX_CONSTANT: u32 = 0xdfc184;
const SYNTAX_KEYWORD: u32 = 0xbd81d6;
const SYNTAX_STRING: u32 = 0xa1c181;
const SYNTAX_FUNCTION: u32 = 0x73ade9;
const SYNTAX_PUNCTUATION: u32 = 0xb2b9c6;
const SYNTAX_LINK: u32 = 0x74ade8;
const SYNTAX_STRING_EXPRESSION: u32 = 0xdfc184;

/// The highlight query for GraphQL, and the one for Protobuf.
///
/// The toolkit registers both grammars with empty queries and neither grammar
/// crate exports its query as a constant, so there is nothing to lift: these
/// are written here, against the node names each grammar declares, in the
/// capture vocabulary [`SYNTAX_TOKENS`] answers. That is the point of writing
/// them here rather than copying a query that would carry capture names this
/// theme cannot paint.
const GRAPHQL_HIGHLIGHTS: &str = r#"
; A keyword is an anonymous node named after itself in this grammar.
[
  "query"
  "mutation"
  "subscription"
  "fragment"
  "on"
  "schema"
  "scalar"
  "type"
  "interface"
  "union"
  "enum"
  "input"
  "implements"
  "extend"
  "directive"
  "repeatable"
] @keyword

; What a definition is called is the name the thing it defines is known by.
[
  (object_type_definition (name) @type)
  (interface_type_definition (name) @type)
  (union_type_definition (name) @type)
  (enum_type_definition (name) @type)
  (input_object_type_definition (name) @type)
  (scalar_type_definition (name) @type)
  (named_type (name) @type)
]

[
  (fragment_definition (fragment_name) @function)
  (directive_definition (name) @function)
]

; What the file names a thing: the field it selects, the argument it passes.
[
  (field (name) @property)
  (argument (name) @property)
  (object_field (name) @property)
]

(variable (name) @variable)

(directive (name) @keyword.directive)

(string_value) @string

[
  (int_value)
  (float_value)
] @number

[
  (boolean_value)
  (null_value)
] @constant.builtin

(enum_value) @constant

(comment) @comment
"#;

const PROTO_HIGHLIGHTS: &str = r#"
; Keywords are named nodes in this grammar rather than anonymous tokens.
[
  (syntax)
  (edition)
  (package)
  (option)
  (import)
  (service)
  (rpc)
  (message)
  (enum)
  (oneof)
  (extend)
  (reserved)
] @keyword

[
  (type)
  (key_type)
  (message_name)
  (enum_name)
  (service_name)
  (rpc_name)
  (message_or_enum_type)
] @type

(string) @string

[
  (int_lit)
  (float_lit)
] @number

[
  (true)
  (false)
] @constant.builtin

(comment) @comment
"#;

/// The injections for Vue, which are the crate's own plus the one it is missing.
///
/// `tree_sitter_vue_updated` injects a language into `<script>` and into a
/// `<style>` that names one — `lang="ts"`, `lang="tsx"`, `lang="css"`,
/// `lang="scss"` — and a bare `<style>`, which is what most single-file
/// components write, gets nothing at all: the whole stylesheet opens in the
/// editor's own ink. CSS is what that block is without a `lang`, so this rule
/// says so. It also matches the two blocks that do name one, and that is a
/// harmless overlap rather than a precedence question: `scss` and `css` resolve
/// to the same registered grammar, so either rule colours the block as CSS.
const VUE_INJECTIONS: &str = r#"
(component
  (style_element
    (start_tag
      (tag_name))
    (raw_text) @injection.content
    (end_tag
      (tag_name))
  )
  (#set! injection.language "css")
)
"#;

/// The two places this grammar reads a Vue file differently from the one
/// Legacy read it with.
///
/// Legacy rendered a `.vue` with Shiki's TextMate grammar, where an attribute is
/// an `entity.other.attribute-name` and a directive is a `meta.attribute.directive`:
/// both reach one token, the function one, and neither of them reaches the tag.
/// The grammar this app uses sends `(attribute_name)` to `attribute`, which is
/// Legacy's string-expression — so `class` wore exactly the colour of `<p>`,
/// and only an attribute with a `:` or an `@` in it read as something else.
/// One capture says it the way Legacy painted it.
///
/// The grammar also captures a whole container as `@tag` — `(template_element)`
/// is the entire `<template>` block, `(element)` a whole tag's body. A span that
/// wide is wider than any token Legacy had: it reaches every byte inside that
/// the rest of the query does not name, which is the whitespace, the
/// indentation, and the prose between the tags. `(text) @none` wears no colour,
/// so the container's gold showed through and the text in a template came out
/// the colour of a tag. Legacy left template text as plain text, so a container
/// wears nothing and what is inside it falls to the editor's ink.
///
/// All three come before the grammar's own query on purpose: the highlighter
/// takes the first pattern that matches a node, and the grammar's
/// `(attribute_name)`, `(template_element)` and `(element)` are all further down
/// its file.
const VUE_HIGHLIGHTS: &str = r#"
(attribute_name) @function
(template_element) @none
(element) @none
"#;

const DOTENV_HIGHLIGHTS: &str = r#"
(comment) @comment
(variable_assignment name: (variable_name) @property value: (_) @string)
(string) @string
(raw_string) @string
"=" @operator
"$" @operator
"#;

/// Install the native dark palette for toolkit widgets and application-owned drawing.
/// `Theme::update` also synchronizes renderable tokens and the Base layer.
pub fn install(cx: &mut App) {
    // C++'s upstream query contains only its additions to C. The toolkit
    // registers it alone; include the base query so ordinary C++ is colored.
    static LANGUAGES: std::sync::Once = std::sync::Once::new();
    LANGUAGES.call_once(|| {
        let registry = gpui_kit::component::highlighter::LanguageRegistry::singleton();
        let c = registry.language("c").expect("C grammar enabled");
        let mut cpp = registry.language("cpp").expect("C++ grammar enabled");
        cpp.highlights = format!("{}\n{}", c.highlights, cpp.highlights).into();
        registry.register("cpp", &cpp);
        for (name, query) in [
            ("csharp", tree_sitter_c_sharp::HIGHLIGHTS_QUERY),
            ("cmake", tree_sitter_cmake::HIGHLIGHTS_QUERY),
            ("swift", tree_sitter_swift::HIGHLIGHTS_QUERY),
            // Two more the toolkit registers empty, and here the query is ours.
            ("graphql", GRAPHQL_HIGHLIGHTS),
            ("proto", PROTO_HIGHLIGHTS),
        ] {
            let mut config = registry.language(name).expect("grammar enabled");
            config.highlights = query.to_owned().into();
            registry.register(name, &config);
        }
        // Legacy highlighted Vue and the toolkit ships a grammar for it with
        // no feature to turn one on, so it carries its grammar and its queries
        // in from the crate that owns them. It is the toolkit's own registry
        // either way, so `syntax_language` reaches it by the name it already
        // answers and the rename pass below covers it like any other.
        // The Vue highlights are the grammar's own plus the one capture Legacy
        // read differently, and the grammar's has to be asked for a `String`
        // because a query that is longer than its own is built rather than
        // borrowed.
        let vue_highlights = format!(
            "{VUE_HIGHLIGHTS}\n{}",
            tree_sitter_vue_updated::HIGHLIGHTS_QUERY
        );
        registry.register(
            "vue",
            &LanguageConfig::new(
                "vue",
                tree_sitter_vue_updated::language(),
                Vec::new(),
                &vue_highlights,
                &format!(
                    "{}\n{VUE_INJECTIONS}",
                    tree_sitter_vue_updated::INJECTIONS_QUERY
                ),
                "",
            ),
        );
        registry.register(
            "gitignore",
            &LanguageConfig::new(
                "gitignore",
                tree_sitter_gitignore::LANGUAGE.into(),
                Vec::new(),
                tree_sitter_gitignore::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        );
        registry.register(
            "ini",
            &LanguageConfig::new(
                "ini",
                tree_sitter_ini::LANGUAGE.into(),
                Vec::new(),
                tree_sitter_ini::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        );
        // Dotenv is shell-style assignments; reuse the installed Bash parser
        // and query under Dotenv's own selectable name.
        let mut dotenv = registry.language("bash").expect("Bash grammar enabled");
        dotenv.name = "dotenv".into();
        dotenv.highlights = DOTENV_HIGHLIGHTS.into();
        registry.register("dotenv", &dotenv);
        // And then one rule for every grammar there is, so a name none of them
        // can be painted in is never emitted in the first place.
        for name in registry.languages() {
            let mut config = registry.language(&name).expect("a registered grammar");
            let renamed = rename_captures(&config.highlights);
            if renamed != config.highlights {
                config.highlights = renamed.into();
                registry.register(&name, &config);
            }
        }
    });
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
        // for the panel.
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
        // Keep the filled-button hover ramp subtle without changing its hue.
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
        // Danger actions are outlined rather than solid red.
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
        // Informational chrome uses the accent because it has no separate token.
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
        // Interface chrome uses the platform UI face; code panes use Fira Code.
        // Keeping these roles separate makes labels, menus and controls feel
        // native without changing the terminal, document or diff typography.
        theme.font_family = crate::app::UI_FONT_FAMILY.into();
        theme.mono_font_family = crate::app::MONO_FONT_FAMILY.into();
        // The grid, document and diff share one monospace size so adjacent code
        // surfaces remain aligned when the terminal font size changes.
        // `font_size` remains the toolkit's base UI scale.
        theme.mono_font_size = crate::terminal::FONT_SIZE;
        // Zed's two corner radii, in the places the toolkit keeps them. A
        // control in Zed is rounded 4px (`rounded_sm`) and an overlay — a
        // dialog, a palette, a dropdown panel — is rounded 8px
        // (`rounded_lg`).
        //
        // The toolkit keeps one `radius` for both roles and a `radius_lg` for
        // the panels it knows are large, so `radius` is given Zed's *overlay*
        // number: it is the one its dropdown menus and popovers read, and a
        // menu that rounds 4px where every Zed menu rounds 8px is the one
        // shape difference a menu cannot hide. The controls that share that
        // token — buttons through [`zed_button`], the app's own fields —
        // name Zed's 4px where they are built, because a control is not an
        // overlay and a 8px corner on a 24px control is a pill.
        theme.radius = px(8.);
        theme.radius_lg = px(8.);
        theme.highlight_theme = Arc::new(content_highlight_theme(theme));
    });
}

/// A button wearing the shape Zed draws one in, in this app's colours.
///
/// Zed fills a control and rounds it 4px, with no outline at all: the only edge
/// a button has is the contrast between its fill and the surface behind it. The
/// toolkit's default variant is the opposite — the same fill inside a 1px
/// hairline box — so a button drawn that way reads as a text field rather than
/// as something to press, which is the shape a user asked this app to stop
/// drawing.
///
/// `Secondary` is that same default without its border: [`install`] gives
/// `button`, `button_hover`, `button_active` and their foregrounds to both
/// variants, so the fill, the pointer ramp and the ink are unchanged and only
/// the outline goes.
///
/// A control that means to be one of Zed's *other* buttons says so after this
/// — `.ghost()`, `.text()` and `.custom(..)` each replace the variant — so this
/// is the shape a button wears unless the call site names another.
///
/// The 4px corner is named here rather than left to [`Theme::radius`], which
/// carries the *overlay's* radius: a button is a control, and Zed rounds a
/// control `rounded_sm`.
///
/// One state does move: a disabled control keeps its fill and dims its ink,
/// where the default variant dimmed the fill as well. That is what every
/// borderless variant in the toolkit does, and a dimmed label on an unchanged
/// fill still says "not now" next to a pointer that no longer answers.
///
/// The native arrow cursor is explicit because the toolkit's text variant
/// otherwise opts into a pointing hand intended for link-like content.
pub fn zed_button(id: impl Into<ElementId>) -> Button {
    Button::new(id).secondary().rounded(px(4.)).cursor_default()
}

/// Every capture name a grammar this app compiles in can emit, and the Legacy
/// token it wears.
///
/// A name the toolkit's theme holds no key for is not here: those are answered
/// by renaming the capture itself, which [`CAPTURE_ALIASES`] does.
///
/// The rule is Legacy's: a capture naming a meaning Legacy's tables name takes
/// that token, and one Legacy leaves as plain text takes the foreground. The
/// names absent from here are the ones that carry no colour by design —
/// `none`, `spell`, `nospell` and `text.*`, which are markdown prose and a
/// Makefile's own diagnostics.
const SYNTAX_TOKENS: &[(&str, u32)] = &[
    ("primary", SYNTAX_FOREGROUND),
    ("comment", SYNTAX_COMMENT),
    ("comment.doc", SYNTAX_COMMENT),
    ("keyword", SYNTAX_KEYWORD),
    // Legacy sends `operatorKeyword` with the keywords.
    ("operator", SYNTAX_KEYWORD),
    ("string", SYNTAX_STRING),
    ("string.escape", SYNTAX_STRING),
    ("string.regex", SYNTAX_STRING),
    ("string.special", SYNTAX_STRING),
    ("string.special.symbol", SYNTAX_STRING),
    ("constant", SYNTAX_CONSTANT),
    ("number", SYNTAX_CONSTANT),
    ("boolean", SYNTAX_CONSTANT),
    ("property", SYNTAX_CONSTANT),
    ("function", SYNTAX_FUNCTION),
    ("constructor", SYNTAX_FUNCTION),
    // Legacy's `string-expression`: a name the file gives something.
    ("tag", SYNTAX_STRING_EXPRESSION),
    ("attribute", SYNTAX_STRING_EXPRESSION),
    ("type", SYNTAX_STRING_EXPRESSION),
    ("punctuation", SYNTAX_PUNCTUATION),
    ("punctuation.bracket", SYNTAX_PUNCTUATION),
    ("punctuation.delimiter", SYNTAX_PUNCTUATION),
    ("punctuation.list_marker", SYNTAX_PUNCTUATION),
    ("punctuation.special", SYNTAX_PUNCTUATION),
    ("link_text", SYNTAX_LINK),
    ("link_uri", SYNTAX_LINK),
    // Legacy sends an identifier and a label to the plain foreground, and an
    // interpolated expression along with them.
    ("variable", SYNTAX_FOREGROUND),
    ("variable.special", SYNTAX_FOREGROUND),
    ("embedded", SYNTAX_FOREGROUND),
    ("label", SYNTAX_FOREGROUND),
];

/// Every capture name a grammar here emits that the toolkit's highlight theme
/// holds no key for, and the name that carries the same meaning in it.
///
/// The toolkit answers a name it does not hold by falling back to the segment
/// before the first dot, so renaming that segment is enough: Go's
/// `@namespace` becomes `@punctuation` and Scala's `@method.call` becomes
/// `@function.call`, each painted by [`SYNTAX_TOKENS`] as Legacy paints it.
/// The right-hand column names the same meaning as the left one and nothing
/// else: an escape sequence is the string it is written in, a parameter is an
/// identifier, and a directive is a keyword where the grammar calls one.
const CAPTURE_ALIASES: &[(&str, &str)] = &[
    // Three grammars name an escape sequence this way, so a `\n` in the middle
    // of a green string was painted in the editor's own ink.
    ("escape", "string.escape"),
    // A character literal is a string literal one character long. Swift spells
    // its wildcard this way too, and a wildcard is punctuation, which is the
    // one place a whole name is answered before the segment before the dot.
    ("character", "string"),
    ("character.special", "punctuation.special"),
    ("float", "number"),
    ("field", "property"),
    ("method", "function"),
    ("parameter", "variable"),
    // Legacy sends a namespace, a module path and C's own `.` and `;` with the
    // punctuation.
    ("namespace", "punctuation"),
    ("module", "punctuation"),
    ("delimiter", "punctuation.delimiter"),
    // Make's and SQL's directives are keywords where they are written as words.
    ("conditional", "keyword.conditional"),
    ("include", "keyword.include"),
    ("storageclass", "keyword.storageclass"),
    ("exception", "keyword.exception"),
    ("repeat", "keyword.repeat"),
    ("clean", "keyword.clean"),
    ("error", "keyword.error"),
    // Vue names the attribute inside a directive — the `if` in `v-if`, the
    // `click` in `@click` — as the directive's own name, and it is an
    // attribute the file gives a thing, which is Legacy's `string-expression`.
    ("props", "attribute"),
];

/// The name [`CAPTURE_ALIASES`] answers `name` with, if it answers it at all.
///
/// A whole name is asked before the segment before its dot, because a grammar
/// may use one spelling for one thing and another for a different one.
fn capture_alias(name: &str) -> Option<&'static str> {
    CAPTURE_ALIASES
        .iter()
        .find(|(from, _)| *from == name)
        .or_else(|| {
            name.split_once('.')
                .and_then(|(base, _)| CAPTURE_ALIASES.iter().find(|(from, _)| *from == base))
        })
        .map(|(_, to)| *to)
}

/// One grammar's highlight query with every capture in [`CAPTURE_ALIASES`]
/// renamed to the one the toolkit can paint.
///
/// A capture is an `@` and the name after it; the name is the whole run of
/// letters, digits, `_`, `.` and `-`, so `@module.builtin` is answered in one
/// piece. A `"` in front of the `@` means the text is a literal inside a
/// predicate — CSS writes its at-rules as `"@namespace"` — and is left as it is.
fn rename_captures(query: &str) -> String {
    let mut renamed = String::with_capacity(query.len());
    let mut rest = query;
    while let Some(at) = rest.find('@') {
        let (before, after) = rest.split_at(at);
        let name: String = after[1..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-'))
            .collect();
        let literal = before.ends_with('"') && !before.ends_with("\\\"");
        renamed.push_str(before);
        renamed.push('@');
        match capture_alias(&name).filter(|_| !literal) {
            Some(to) => renamed.push_str(to),
            None => renamed.push_str(&name),
        }
        rest = &after[1 + name.len()..];
    }
    renamed.push_str(rest);
    renamed
}

/// Legacy's content ink and syntax tokens as the toolkit's highlight theme.
///
/// The toolkit paints both the read-only editor and the diff from this one
/// theme, so a file keeps its colours when it stops being a document. A token
/// name comes from a grammar query; Legacy names the same meaning with a
/// `--muster-syntax-token-*`, and the two vocabularies are joined by
/// [`SYNTAX_TOKENS`] rather than anywhere that draws.
///
/// The colours are reached through serde because a `ThemeStyle`'s fields are
/// private to the toolkit: this is the toolkit's own route in, not a
/// reimplementation of it, and it takes the toolkit's own `#rrggbb` spelling
/// rather than a float crossing, so a token arrives as the colour Legacy wrote.
fn content_highlight_theme(theme: &Theme) -> HighlightTheme {
    let token = |value: u32| format!("#{value:06x}");
    let ink = |value: u32| serde_json::Value::String(token(value));
    let installed =
        |color: Hsla| serde_json::to_value(color).expect("the toolkit spells a colour as a string");
    let mut syntax = serde_json::Map::new();
    for (name, value) in SYNTAX_TOKENS {
        syntax.insert(
            (*name).to_owned(),
            serde_json::json!({ "color": ink(*value) }),
        );
    }
    // The three Legacy names a weight or a slant rather than a colour of its
    // own: a Markdown heading is its `function` ink in bold, and emphasis is
    // the font alone, which is what Legacy's editor asks for.
    for (name, value, weight, slant) in [
        ("title", SYNTAX_FUNCTION, Some(700), None),
        ("emphasis", SYNTAX_FOREGROUND, None, Some("italic")),
        ("emphasis.strong", SYNTAX_FOREGROUND, Some(700), None),
    ] {
        let mut style = serde_json::Map::new();
        style.insert("color".into(), ink(value));
        if let Some(weight) = weight {
            style.insert("font_weight".into(), weight.into());
        }
        if let Some(slant) = slant {
            style.insert("font_style".into(), slant.into());
        }
        syntax.insert(name.into(), style.into());
    }
    let style = serde_json::from_value::<HighlightThemeStyle>(serde_json::json!({
        // The document, the diff and the terminal grid are the window's body,
        // so they are one surface: the base the chrome is painted on.
        "editor.background": installed(theme.background),
        "editor.foreground": token(CONTENT_TEXT),
        // Legacy paints the gutter as the document it numbers rather than as
        // a well of its own, which is what leaving the gutter unset asks for.
        "editor.line_number": token(CONTENT_TEXT_FAINT),
        "editor.active_line_number": token(CONTENT_TEXT),
        "editor.invisible": token(CONTENT_TEXT_FAINT),
        "error": installed(theme.danger),
        "warning": installed(theme.warning),
        "info": installed(theme.info),
        "hint": token(CONTENT_ACCENT),
        "success": installed(theme.success),
        "syntax": syntax,
    }))
    .expect("the content highlight theme is this crate's own literal");
    HighlightTheme {
        name: "muster-content".into(),
        appearance: ThemeMode::Dark,
        style,
    }
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

/// The inks Catppuccin's file icons paint their layers with.
///
/// A role name rather than a value: the drawing names the ink it wants and the window answers with
/// [`Roles::ink`], which is what lets one set of drawings serve whatever palette the app installs.
/// The set is exhaustive on purpose — a drawing reaching an ink that is not here is a generator
/// failure, not a blank stroke in the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ink {
    Text,
    Overlay1,
    Red,
    Maroon,
    Flamingo,
    Rosewater,
    Pink,
    Mauve,
    Lavender,
    Blue,
    Sky,
    Sapphire,
    Teal,
    Green,
    Yellow,
    Peach,
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
    pub content: Hsla,
    pub content_text: Hsla,
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
    /// The primary action and small affirmative marks such as selected checkmarks.
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
            content: theme
                .highlight_theme
                .style
                .editor_background
                .unwrap_or(theme.background),
            content_text: theme
                .highlight_theme
                .style
                .editor_foreground
                .unwrap_or(theme.foreground),
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
    /// The ink one of Catppuccin's file icons paints its layer with.
    ///
    /// These are not chrome: a file icon is its own drawing wearing its own colours, the way the
    /// terminal grid wears what a VT sequence asked for. They live here anyway because this is the
    /// one place the app names a colour, and because a second table anywhere else is the drift
    /// this module exists to prevent.
    ///
    /// The values are Catppuccin's Mocha palette, which is what Legacy draws file icons in under
    /// its dark theme. There is no light palette to swap: the app has one appearance, chosen in
    /// `main.rs` before the first window opens.
    pub fn ink(&self, ink: Ink) -> Hsla {
        rgb(match ink {
            Ink::Text => 0xcdd6f4,
            Ink::Overlay1 => 0x7f849c,
            Ink::Red => 0xf38ba8,
            Ink::Maroon => 0xeba0ac,
            Ink::Flamingo => 0xf2cdcd,
            Ink::Rosewater => 0xf5e0dc,
            Ink::Pink => 0xf5c2e7,
            Ink::Mauve => 0xcba6f7,
            Ink::Lavender => 0xb4befe,
            Ink::Blue => 0x89b4fa,
            Ink::Sky => 0x89dceb,
            Ink::Sapphire => 0x74c7ec,
            Ink::Teal => 0x94e2d5,
            Ink::Green => 0xa6e3a1,
            Ink::Yellow => 0xf9e2af,
            Ink::Peach => 0xfab387,
        })
        .into()
    }

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

/// A colour the service gave, as ink, when it is a colour this app will paint.
///
/// The colour belongs to the service: it is a string on a wire that becomes a tint in a
/// window, so it is checked for being a colour before it is used and a string that is
/// not one is not painted at all. What a row wears instead is the theme's decision, not
/// the service's.
///
/// `#RGB`, `#RRGGBB` and `#RRGGBBAA` are the shapes OpenCode writes.
pub fn service_tint(color: &str) -> Option<Hsla> {
    let digits = color.strip_prefix('#')?;
    let value = match digits.len() {
        3 => {
            let mut expanded = String::with_capacity(6);
            for digit in digits.chars() {
                expanded.push(digit);
                expanded.push(digit);
            }
            u32::from_str_radix(&expanded, 16).ok()?
        }
        6 => u32::from_str_radix(digits, 16).ok()?,
        8 => u32::from_str_radix(digits, 16).ok()?,
        _ => return None,
    };
    let (red, green, blue, alpha) = match digits.len() {
        8 => (
            (value >> 24) & 0xff,
            (value >> 16) & 0xff,
            (value >> 8) & 0xff,
            value & 0xff,
        ),
        _ => (
            (value >> 16) & 0xff,
            (value >> 8) & 0xff,
            value & 0xff,
            0xff,
        ),
    };
    Some(Hsla::from(gpui_kit::rgba(
        (red << 24) | (green << 16) | (blue << 8) | alpha,
    )))
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
    use gpui_kit::component::highlighter::{FontStyle, FontWeightContent};
    use gpui_kit::{px, TestAppContext};

    /// The capture names a grammar emits on purpose for the theme to leave
    /// alone: markdown's own prose, a spell hint that rides beside a real
    /// token, and Makefile's own diagnostics.
    const UNCOLOURED: &[&str] = &[
        "none",
        "spell",
        "nospell",
        "text.code.span",
        "text.literal",
        "text.reference",
        "text.uri",
        "text.danger",
        "text.note",
        "text.warning",
    ];

    /// The three names Legacy paints with a weight or a slant rather than with
    /// a colour of their own, so they are answered outside [`SYNTAX_TOKENS`].
    const STYLED: &[&str] = &["title", "emphasis", "emphasis.strong"];

    /// Every capture name in a highlight query, under the same rule
    /// [`rename_captures`] reads them by: the whole run of letters, digits,
    /// `_`, `.` and `-` after an `@`, and not a literal inside a predicate.
    fn captures(query: &str) -> impl Iterator<Item = &str> {
        query.lines().flat_map(|line| {
            // `;` opens a comment in a query, and a credit line is nothing but
            // an `@handle` that no grammar emits.
            let line = line.split_once(';').map_or(line, |(before, _)| before);
            line.match_indices('@').filter_map(|(at, _)| {
                let before = &line[..at];
                if before.ends_with('"') && !before.ends_with("\\\"") {
                    return None;
                }
                let after = &line[at + 1..];
                let end = after
                    .find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '.' | '-')))
                    .unwrap_or(after.len());
                (end > 0).then(|| &after[..end])
            })
        })
    }

    fn dark(cx: &mut TestAppContext) -> Roles {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install(cx);
            Roles::of(cx)
        })
    }

    // Syntax captures remain pinned to the project's previous token contract; UI chrome does not.
    //
    // The contract is written down here rather than read from the stylesheet it
    // came from: that stylesheet is not in this repository any more, and these
    // are the values its dark theme carried. The constants above are the
    // production copy of the same numbers and the theme literal is the third;
    // this table is what says all three must agree.
    const LEGACY_TOKENS: &[(&str, u32)] = &[
        ("content-text", 0xdce0e5),
        ("content-text-faint", 0x767d8d),
        ("syntax-foreground", 0xacb2be),
        ("syntax-token-comment", 0x878e98),
        ("syntax-token-constant", 0xdfc184),
        ("syntax-token-keyword", 0xbd81d6),
        ("syntax-token-string", 0xa1c181),
        ("syntax-token-function", 0x73ade9),
        ("syntax-token-string-expression", 0xdfc184),
        ("syntax-token-punctuation", 0xb2b9c6),
        ("syntax-token-link", 0x74ade8),
    ];

    fn legacy(token: &str) -> Hsla {
        let value = LEGACY_TOKENS
            .iter()
            .find(|(name, _)| *name == token)
            .unwrap_or_else(|| panic!("missing syntax token: {token}"))
            .1;
        rgb(value).into()
    }

    #[gpui_kit::test]
    fn roles_follow_the_installed_native_theme(cx: &mut TestAppContext) {
        let roles = dark(cx);
        cx.read(|cx| {
            let theme = Theme::global(cx);
            for (role, theme_color, name) in [
                (roles.surface_base, theme.background, "surface_base"),
                (roles.surface, theme.sidebar, "surface"),
                (roles.surface_raised, theme.popover, "surface_raised"),
                (roles.text, theme.sidebar_foreground, "text"),
                (roles.text_muted, theme.muted_foreground, "text_muted"),
                (roles.border, theme.sidebar_border, "border"),
                (roles.hover, theme.secondary_hover, "hover"),
                (roles.pressed, theme.secondary_active, "pressed"),
                (roles.selected, theme.list_active, "selected"),
                (roles.focus, theme.ring, "focus"),
                (roles.accent, theme.primary, "accent"),
                (roles.status.warning, theme.warning, "warning"),
                (roles.status.danger, theme.danger, "danger"),
                (roles.status.success, theme.success, "success"),
                (roles.status.info, theme.info, "info"),
            ] {
                assert_eq!(role, theme_color, "role diverged from theme: {name}");
                assert!(role.is_opaque(), "{name} must be opaque");
            }
            assert_eq!(roles.row_hover(true), roles.selected_hover);
            assert_eq!(roles.selected_hover, roles.selected);
            assert_eq!(roles.disabled_surface, theme.muted);
        });
    }

    #[gpui_kit::test]
    fn interface_and_code_use_distinct_font_families(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install(cx);
        });
        cx.read(|cx| {
            let theme = Theme::global(cx);
            assert_eq!(theme.font_family, crate::app::UI_FONT_FAMILY);
            assert_eq!(theme.mono_font_family, crate::app::MONO_FONT_FAMILY);
            assert_ne!(theme.font_family, theme.mono_font_family);
            assert_eq!(theme.mono_font_size, crate::terminal::FONT_SIZE);
            assert_eq!(theme.font_size, px(16.));
        });
    }

    /// Every overlay is rounded the way Zed rounds one.
    ///
    /// The toolkit's own default is 6px, which is its design system's radius
    /// rather than Zed's, and it is what a dropdown panel, a popover and a
    /// dialog would all inherit. Zed rounds every one of those `rounded_lg`.
    ///
    /// A *control* is not on this test: the toolkit keeps one token for both
    /// roles, so `radius` holds the overlay's 8px and the controls name the
    /// 4px they wear where they are built — `zed_button` for every button,
    /// and the field that takes a search query.
    #[gpui_kit::test]
    fn overlays_are_rounded_lg(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install(cx);
        });
        cx.read(|cx| {
            let theme = Theme::global(cx);
            assert_eq!(
                theme.radius,
                px(8.),
                "a dropdown, a popover and a dialog follow Zed's rounded_lg"
            );
            assert_eq!(
                theme.radius_lg,
                px(8.),
                "and so does the panel the toolkit knows is large"
            );
            assert_eq!(theme.radius_tokens().md, px(8.));
        });
    }

    /// The document and the diff are drawn at the grid's own size.
    ///
    /// Both are the app's code, and both are read next to a terminal in the
    /// same window. Legacy drew its editor at 13px against a 16px grid, which
    /// is why the file pane looked like it was set in a smaller type than the
    /// terminal it sits beside: the value here is the terminal's, and the
    /// editor and the toolkit's diff both read it from this one place.
    #[gpui_kit::test]
    fn the_document_and_the_diff_read_at_the_terminals_own_size(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install(cx);
        });
        cx.read(|cx| {
            let theme = Theme::global(cx);
            // The grid's own size, and not a number that happens to match it.
            assert_eq!(crate::terminal::FONT_SIZE, px(16.));
            // What the toolkit's editor and diff paint with: the same value.
            assert_eq!(theme.mono_font_size, crate::terminal::FONT_SIZE);
            // The chrome around the code stays where it was: a header is a
            // header, and the code is what is being read.
            assert!(theme.mono_font_size > px(13.));
        });
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
                assert_eq!(token.color, color, "stale toolkit token: {reference}");
                assert_eq!(
                    token.background,
                    color.into(),
                    "stale background: {reference}"
                );
            }
            assert_eq!(theme.overlay, theme.background.alpha(0.8));
            assert_eq!(theme.button_danger_hover, theme.danger.alpha(0.12));
            assert_eq!(theme.button_danger_active, theme.danger.alpha(0.20));
        });
    }

    #[gpui_kit::test]
    fn selection_wins_over_hover(cx: &mut TestAppContext) {
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
        // Secondary text is less prominent but remains readable on the panel.
        assert!(contrast(roles.text_muted, roles.surface) >= 4.5);
    }

    /// The colour the installed highlight theme paints a grammar token with.
    fn syntax(cx: &mut TestAppContext, name: &str) -> Hsla {
        let color = cx.update(|cx| {
            Theme::global(cx)
                .highlight_theme
                .style(name)
                .and_then(|style| style.color)
        });
        color.unwrap_or_else(|| panic!("no highlight style for {name}"))
    }

    /// Whether the installed highlight theme paints a capture at all.
    fn paints(cx: &mut TestAppContext, name: &str) -> Option<gpui_kit::HighlightStyle> {
        cx.update(|cx| Theme::global(cx).highlight_theme.style(name))
    }

    /// The colour a grammar's capture named `name` is painted with, once the
    /// renames `install` applies to the queries have been accounted for.
    ///
    /// A name the toolkit's theme holds a key for answers itself; one it does
    /// not is only ever emitted under its alias, which is what this resolves.
    fn painted(cx: &mut TestAppContext, name: &str) -> Option<Hsla> {
        let renamed = capture_alias(name).unwrap_or(name);
        paints(cx, renamed).and_then(|style| style.color)
    }

    /// Assert that a syntax token resolves to the expected content color.
    ///
    /// A highlight theme is installed by deserializing the toolkit's own
    /// `#rrggbb` spelling, so the token arrives as the authored value rather
    /// than as a float that went somewhere and came back.
    fn assert_token(actual: Option<Hsla>, expected: Hsla) {
        assert_eq!(actual, Some(expected));
    }

    #[gpui_kit::test]
    fn content_surface_uses_grid_background_and_syntax_tokens_are_stable(cx: &mut TestAppContext) {
        let roles = dark(cx);
        cx.update(|cx| {
            let theme = Theme::global(cx);
            assert_eq!(theme.mode, ThemeMode::Dark);
            let ink = theme.highlight_theme.style.editor_foreground;
            let numbers = theme.highlight_theme.style.editor_line_number;
            for (color, token) in [(ink, "content-text"), (numbers, "content-text-faint")] {
                assert_token(color, legacy(token));
            }
        });
        // The document, diff and terminal grid share the window's body surface.
        let grid = crate::terminal::Palette::default().background;
        let grid = Hsla::from(rgb((grid.0 as u32) << 16
            | (grid.1 as u32) << 8
            | grid.2 as u32));
        assert_eq!(roles.content, roles.surface_base);
        assert_eq!(
            roles.content, grid,
            "the grid and the document share a background"
        );
        for (name, token) in [
            ("primary", "syntax-foreground"),
            ("comment", "syntax-token-comment"),
            ("keyword", "syntax-token-keyword"),
            ("operator", "syntax-token-keyword"),
            ("string", "syntax-token-string"),
            ("string.escape", "syntax-token-string"),
            ("number", "syntax-token-constant"),
            ("boolean", "syntax-token-constant"),
            ("property", "syntax-token-constant"),
            ("function", "syntax-token-function"),
            ("tag", "syntax-token-string-expression"),
            ("type", "syntax-token-string-expression"),
            ("punctuation", "syntax-token-punctuation"),
            ("link_text", "syntax-token-link"),
            ("variable", "syntax-foreground"),
        ] {
            assert_token(Some(syntax(cx, name)), legacy(token));
        }
    }

    /// Every capture name a grammar this app compiles in can emit, and the
    /// Legacy token it wears.
    ///
    /// A colour the service gave is painted only when it is a colour.
    ///
    /// The service owns the colour, and it is a string on a wire that becomes a tint in
    /// a window: a string that is not a colour is not painted, and the theme's own ink
    /// is what a row wears instead.
    #[test]
    fn a_service_colour_is_painted_only_when_it_is_a_colour() {
        for color in ["#5c7cfa", "#abc", "#5c7cfa80", "#ABCDEF"] {
            assert!(service_tint(color).is_some(), "{color} is a colour");
        }
        for color in ["rebeccapurple", "#12345", "#1234567", "5c7cfa", ""] {
            assert!(service_tint(color).is_none(), "{color} is not a colour");
        }
    }

    /// This is the compatibility contract, stated once and read off Legacy's own
    /// table: a capture naming a meaning Legacy names takes that token, and the
    /// whole of them stays readable on the surface they are drawn on. Every row
    /// is a name a grammar here emits, so a capture missing from this list is
    /// either one no grammar emits or one the two tables still fail to answer.
    #[gpui_kit::test]
    fn every_capture_a_grammar_can_emit_wears_a_legacy_token(cx: &mut TestAppContext) {
        let roles = dark(cx);
        for (capture, token) in [
            // An identifier, a label and an interpolated expression are all
            // plain text in Legacy.
            ("primary", "syntax-foreground"),
            ("variable", "syntax-foreground"),
            ("variable.builtin", "syntax-foreground"),
            ("variable.member", "syntax-foreground"),
            ("variable.parameter", "syntax-foreground"),
            ("variable.special", "syntax-foreground"),
            ("embedded", "syntax-foreground"),
            ("label", "syntax-foreground"),
            // Legacy names a parameter beside the foreground it equals, so the
            // capture is renamed to an identifier rather than given a colour of
            // its own.
            ("parameter", "syntax-foreground"),
            ("emphasis", "syntax-foreground"),
            ("emphasis.strong", "syntax-foreground"),
            // Comments, including the doc and the unused spellings.
            ("comment", "syntax-token-comment"),
            ("comment.doc", "syntax-token-comment"),
            ("comment.documentation", "syntax-token-comment"),
            ("comment.unused", "syntax-token-comment"),
            // Keywords, the operators Legacy sends with them, and the
            // directives Make, SQL and Zig spell as words of their own.
            ("keyword", "syntax-token-keyword"),
            ("keyword.conditional", "syntax-token-keyword"),
            ("keyword.coroutine", "syntax-token-keyword"),
            ("keyword.directive", "syntax-token-keyword"),
            ("keyword.function", "syntax-token-keyword"),
            ("keyword.import", "syntax-token-keyword"),
            ("keyword.modifier", "syntax-token-keyword"),
            ("keyword.operator", "syntax-token-keyword"),
            ("keyword.repeat", "syntax-token-keyword"),
            ("keyword.return", "syntax-token-keyword"),
            ("keyword.type", "syntax-token-keyword"),
            ("operator", "syntax-token-keyword"),
            ("conditional", "syntax-token-keyword"),
            ("include", "syntax-token-keyword"),
            ("storageclass", "syntax-token-keyword"),
            ("exception", "syntax-token-keyword"),
            ("repeat", "syntax-token-keyword"),
            ("clean", "syntax-token-keyword"),
            ("error", "syntax-token-keyword"),
            // A string and everything the grammars capture inside one.
            ("string", "syntax-token-string"),
            ("string.escape", "syntax-token-string"),
            ("string.regex", "syntax-token-string"),
            ("string.regexp", "syntax-token-string"),
            ("string.special", "syntax-token-string"),
            ("string.special.regex", "syntax-token-string"),
            ("string.special.symbol", "syntax-token-string"),
            ("character", "syntax-token-string"),
            // Swift's wildcard is the one name that means the opposite.
            ("character.special", "syntax-token-punctuation"),
            // An escape sequence is the string it is written in.
            ("escape", "syntax-token-string"),
            // Literals and the names a file gives a thing.
            ("constant", "syntax-token-constant"),
            ("constant.builtin", "syntax-token-constant"),
            ("constant.macro", "syntax-token-constant"),
            ("number", "syntax-token-constant"),
            ("number.float", "syntax-token-constant"),
            ("boolean", "syntax-token-constant"),
            ("float", "syntax-token-constant"),
            ("field", "syntax-token-constant"),
            ("property", "syntax-token-constant"),
            ("property.definition", "syntax-token-constant"),
            // Calls, definitions and a heading.
            ("function", "syntax-token-function"),
            ("function.builtin", "syntax-token-function"),
            ("function.call", "syntax-token-function"),
            ("function.macro", "syntax-token-function"),
            ("function.method", "syntax-token-function"),
            ("function.method.builtin", "syntax-token-function"),
            ("function.special", "syntax-token-function"),
            ("constructor", "syntax-token-function"),
            ("method", "syntax-token-function"),
            ("method.call", "syntax-token-function"),
            ("title", "syntax-token-function"),
            // Tags, attributes and types.
            ("attribute", "syntax-token-string-expression"),
            ("tag", "syntax-token-string-expression"),
            ("tag.attribute", "syntax-token-string-expression"),
            ("tag.delimiter", "syntax-token-string-expression"),
            ("tag.doctype", "syntax-token-string-expression"),
            ("tag.error", "syntax-token-string-expression"),
            ("type", "syntax-token-string-expression"),
            ("type.builtin", "syntax-token-string-expression"),
            ("type.definition", "syntax-token-string-expression"),
            ("type.qualifier", "syntax-token-string-expression"),
            // Punctuation, C's own delimiter and the namespaces.
            ("punctuation", "syntax-token-punctuation"),
            ("punctuation.bracket", "syntax-token-punctuation"),
            ("punctuation.delimiter", "syntax-token-punctuation"),
            ("punctuation.list_marker", "syntax-token-punctuation"),
            ("punctuation.special", "syntax-token-punctuation"),
            ("delimiter", "syntax-token-punctuation"),
            ("namespace", "syntax-token-punctuation"),
            ("module", "syntax-token-punctuation"),
            ("module.builtin", "syntax-token-punctuation"),
            // Links.
            ("link_text", "syntax-token-link"),
            ("link_uri", "syntax-token-link"),
        ] {
            let color = painted(cx, capture).unwrap_or_else(|| panic!("{capture} is uncoloured"));
            assert_eq!(color, legacy(token), "{capture}");
            assert!(
                contrast(color, roles.content) >= 4.5,
                "{capture} is unreadable on the content surface"
            );
        }
        // Legacy names a weight or a slant rather than a colour for these three,
        // and the toolkit can say both, so it does.
        for (capture, weight, slant) in [
            ("title", Some(FontWeightContent::Bold), None),
            ("emphasis", None, Some(FontStyle::Italic)),
            ("emphasis.strong", Some(FontWeightContent::Bold), None),
        ] {
            let style = paints(cx, capture).unwrap_or_else(|| panic!("{capture} is unstyled"));
            assert_eq!(style.font_weight, weight.map(Into::into), "{capture}");
            assert_eq!(style.font_style, slant.map(Into::into), "{capture}");
        }
        // The names that carry no colour by design, in Legacy and here: a
        // "leave this alone" capture, a spelling hint that rides beside a real
        // token, and markdown's own prose.
        for capture in UNCOLOURED {
            assert_eq!(paints(cx, capture), None, "{capture} is coloured");
        }
    }

    /// A capture the theme cannot hold is renamed before it is ever emitted.
    ///
    /// Rust, Python and Ruby capture an escape sequence as `@escape` and Go
    /// captures a package qualifier as `@namespace`; neither is a name the
    /// toolkit's theme has a key for, so without the rename `install` does the
    /// token falls back to the editor's own ink. The range is compared exactly,
    /// because a style that merely covers the capture is the surrounding token
    /// and would pass either way.
    #[gpui_kit::test]
    fn a_renamed_capture_is_painted_as_legacy_paints_its_meaning(cx: &mut TestAppContext) {
        dark(cx);
        for (language, source, needle, name) in [
            (
                "rust",
                "fn main() {\n    let s = \"a\\nb\";\n}\n",
                "\\n",
                "string",
            ),
            ("python", "s = \"a\\nb\"\n", "\\n", "string"),
            ("ruby", "s = \"a\\nb\"\n", "\\n", "string"),
            (
                "go",
                "package main\n\nvar r io.Reader\n\nfunc main() {}\n",
                "io",
                "punctuation",
            ),
            ("c", "int main(void) { return 0; }\n", ";", "punctuation"),
        ] {
            cx.update(|cx| {
                let mut highlighter =
                    gpui_kit::component::highlighter::SyntaxHighlighter::new(language);
                let text = gpui_kit::component::Rope::from_str(source);
                assert!(highlighter.update(None, &text, None), "{language}");
                let theme = Theme::global(cx);
                let expected = theme
                    .highlight_theme
                    .style(name)
                    .and_then(|style| style.color);
                let at = source.find(needle).expect("the source has the capture");
                let span = at..at + needle.len();
                let painted =
                    highlighter.styles(&(0..source.len()), theme.highlight_theme.as_ref());
                // Every run the token sits in, not just the run that starts on
                // it: a capture the theme cannot answer becomes a run of its
                // own with no colour in it, which is what an escape inside a
                // green string used to look like.
                let runs = painted
                    .iter()
                    .filter(|(range, _)| range.start < span.end && range.end > span.start)
                    .collect::<Vec<_>>();
                assert!(!runs.is_empty(), "{language} leaves {needle} uncoloured");
                for (range, style) in runs {
                    assert_eq!(
                        style.color, expected,
                        "{language} paints {}..{} as {name}",
                        range.start, range.end
                    );
                }
            });
        }
    }

    /// Every grammar carries the toolkit's own capture names.
    ///
    /// The renames are what let the theme answer a capture at all, so a grammar
    /// the pass never reached is a token silently back to the editor's ink.
    /// Running the rename over an already-renamed query changes nothing, and
    /// that is what says it reached all of them. What it leaves behind is then
    /// required to be paintable, which is what says a grammar is *done*: a name
    /// neither table answers would be a file opening with half its tokens in
    /// the editor's own ink. Vue's own `@props` is the one this caught.
    #[gpui_kit::test]
    fn every_grammar_has_had_its_captures_renamed(cx: &mut TestAppContext) {
        dark(cx);
        cx.read(|_cx| {
            let registry = gpui_kit::component::highlighter::LanguageRegistry::singleton();
            for name in registry.languages() {
                let query = registry
                    .language(&name)
                    .expect("a registered grammar")
                    .highlights;
                assert_eq!(
                    rename_captures(&query),
                    query,
                    "{name} still captures a name the theme cannot paint"
                );
                for capture in captures(&query) {
                    let themed = |token: &&str| SYNTAX_TOKENS.iter().any(|(name, _)| name == token);
                    assert!(
                        // A leading `_` is the grammar's own scratch capture:
                        // CMake holds `set` in `@_function` to match it in a
                        // predicate and never paints it.
                        capture.starts_with('_')
                            || themed(&capture)
                            || capture
                                .split_once('.')
                                .is_some_and(|(base, _)| themed(&base))
                            || STYLED.contains(&capture)
                            || UNCOLOURED.contains(&capture),
                        "{name} emits @{capture}, which neither table answers"
                    );
                }
            }
        });
    }

    /// A Vue attribute wears the colour Legacy painted one in, and not the
    /// colour it painted the tag beside it in.
    ///
    /// Legacy read a `.vue` with a TextMate grammar where an attribute and a
    /// directive are two spellings of one thing — both reach its function token —
    /// and neither reaches the tag's. The grammar this app uses sends the plain
    /// attribute to `attribute`, which is the tag's colour, so `class` and `<p>`
    /// came out identical and only an attribute with a `:` in it looked like
    /// anything. What is compared here is what a document actually paints, not a
    /// capture name: a capture can be answered by the wrong token, which is
    /// exactly what happened, and a test over capture names would not have said
    /// so.
    #[gpui_kit::test]
    fn a_vue_attribute_is_not_painted_like_a_tag(cx: &mut TestAppContext) {
        use gpui_kit::component::input::Rope;

        let source = "<template>\n  <p class=\"plain\" :title=\"name\">text</p>\n</template>\n";
        dark(cx);
        cx.update(|cx| {
            let mut highlighter = gpui_kit::component::highlighter::SyntaxHighlighter::new("vue");
            assert!(
                highlighter.update(None, &Rope::from(source), None),
                "the Vue grammar accepted its own file"
            );
            let theme = gpui_kit::component::Theme::global(cx);
            let styles = highlighter.styles(&(0..source.len()), theme.highlight_theme.as_ref());
            let painted = |needle: &str| {
                styles
                    .iter()
                    .find(|(span, style)| {
                        style.color.is_some()
                            && span.start < source.len()
                            && source.get(span.start..span.end) == Some(needle)
                    })
                    .map(|(_, style)| style.color)
                    .unwrap_or_else(|| panic!("{needle} was not painted"))
            };
            let function = legacy("syntax-token-function");
            let tag = legacy("syntax-token-string-expression");
            // The three ways a template names something, and Legacy's answer:
            // two of them are the function, one is the tag.
            assert_eq!(painted("class"), Some(function), "a plain attribute");
            assert_eq!(painted("title"), Some(function), "a bound attribute");
            assert_eq!(painted("p"), Some(tag), "the tag beside them");
        });
    }

    /// JSON's own grammar, so the tokens compared are the ones a `.json`
    /// document actually reaches rather than names this crate chose.
    #[gpui_kit::test]
    fn json_tokens_are_not_all_one_colour(cx: &mut TestAppContext) {
        dark(cx);
        let string = syntax(cx, "string");
        let property = syntax(cx, "property");
        let comment = syntax(cx, "comment");
        let number = syntax(cx, "number");
        let boolean = syntax(cx, "boolean");
        let null = syntax(cx, "constant.builtin");
        let punctuation = syntax(cx, "punctuation");
        assert_ne!(string, property, "a key and its value must not read alike");
        assert_ne!(string, comment);
        assert_ne!(string, punctuation);
        assert_ne!(comment, punctuation);
        // Legacy gives a property, a number and a boolean one constant colour.
        assert_eq!(property, number);
        assert_eq!(property, boolean);
        assert_eq!(property, null);
    }
}
