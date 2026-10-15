//! Read-only central document displayed in place of the terminal.
use crate::{app::TerminalWindow, theme::Roles};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::Sizable;
use gpui_kit::{
    div, AnyElement, App, AppContext, Context, Entity, Focusable, InteractiveElement, IntoElement,
    ParentElement, Styled, Window,
};

/// The document's editor, kept between renders.
///
/// The state is one entity created when the document is opened and read from
/// every frame after: a text selection, a scroll offset and a parse are then
/// things the reader did rather than things the last frame threw away. Nothing
/// here is written back — the document is a view of a file, and this slice owns
/// no editing and no persistence.
pub(crate) struct DocumentView {
    pub(crate) state: Entity<EditorState>,
    /// The document this state was built for. A different path replaces it: a
    /// selection or an offset carried over from another file would describe a
    /// position in a file that is not on screen.
    path: String,
}

impl DocumentView {
    /// The editor for one document, already holding its whole text.
    ///
    /// The text is set once, whole, from what `read_preview` returned: a
    /// preview that stops mid-file would look like a file that ends there.
    fn new(identity: &str, path: &str, text: &str, window: &mut Window, cx: &mut App) -> Self {
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(syntax_language(path))
                .line_number(true)
                // No reflow: a document is shown as it is written, and the
                // editor scrolls both ways instead of rewrapping long lines.
                .soft_wrap(false)
                .default_value(text)
        });
        Self {
            state,
            path: identity.to_owned(),
        }
    }
}

/// The editor for `path`: the retained one, or a new one for a new document.
fn document_state(
    retained: &mut Option<DocumentView>,
    identity: &str,
    path: &str,
    text: &str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<EditorState> {
    if retained.as_ref().is_none_or(|view| view.path != identity) {
        *retained = Some(DocumentView::new(identity, path, text, window, cx));
    }
    retained
        .as_ref()
        .map(|view| view.state.clone())
        .expect("just installed")
}

/// The toolkit language name a path asks for.
///
/// The same list Legacy chose its mode by, reduced to the languages the
/// toolkit compiles in. A name it has no grammar for is plain text rather than
/// nothing: an unknown file still opens, it just opens uncoloured.
pub(crate) fn syntax_language(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.').map_or("", |(_, extension)| extension) {
        "json" | "jsonc" => "json",
        "rs" => "rust",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "yaml" | "yml" => "yaml",
        "html" | "htm" => "html",
        "css" | "scss" => "css",
        "md" | "markdown" => "markdown",
        _ => "plaintext",
    }
}

impl TerminalWindow {
    pub(crate) fn clear_document(&mut self) {
        if let Some(key) = self.document_target.take() {
            self.file_loading.remove(&key);
            self.document_errors.remove(&key);
        }
        self.open_document = None;
        self.document_view = None;
        self.document_request = self.document_request.wrapping_add(1);
    }

    pub(crate) fn document_element(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let target = self.document_target.clone().or_else(|| {
            self.open_document
                .as_ref()
                .map(|(id, preview)| (id.clone(), preview.path.clone()))
        });
        let Some((checkout_id, path)) = target else {
            self.document_view = None;
            return None;
        };
        if self.navigation.active_checkout.as_ref() != Some(&checkout_id) {
            return None;
        }
        let roles = Roles::of(cx);
        let key = (checkout_id.clone(), path.clone());
        let body = if let Some((_, preview)) = self
            .open_document
            .as_ref()
            .filter(|(id, preview)| id == &checkout_id && preview.path == path)
        {
            let identity = format!("{checkout_id}\0{path}\0{}", self.document_request);
            let new_view = self
                .document_view
                .as_ref()
                .is_none_or(|view| view.path != identity);
            let state = document_state(
                &mut self.document_view,
                &identity,
                &path,
                &preview.text,
                window,
                cx,
            );
            if new_view {
                state.read(cx).focus_handle(cx).focus(window, cx);
            }
            Editor::new(&state)
                .readonly(true)
                .bordered(false)
                .size_full()
                .into_any_element()
        } else if let Some(error) = self.document_errors.get(&key) {
            let retry_path = path.clone();
            let retry_checkout = checkout_id.clone();
            div()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_color(roles.status.warning).child(error.clone()))
                .child(
                    Button::new(format!("retry-read-{path}"))
                        .ghost()
                        .small()
                        .label("Retry")
                        .accessibility_label(format!("Retry reading {path}"))
                        .on_click(cx.listener(move |app, _, window, cx| {
                            let checkout = app.navigation.checkout(&retry_checkout).cloned();
                            let repo = app
                                .navigation
                                .repos
                                .iter()
                                .find(|repo| repo.checkouts.iter().any(|c| c.id == retry_checkout))
                                .cloned();
                            if let (Some(repo), Some(checkout)) = (repo, checkout) {
                                app.request_document(
                                    repo,
                                    checkout,
                                    retry_path.clone(),
                                    window,
                                    cx,
                                );
                            }
                        })),
                )
                .into_any_element()
        } else {
            div()
                .id("document-loading")
                .p_3()
                .text_color(roles.text_muted)
                .child("Loading file…")
                .into_any_element()
        };
        Some(
            div()
                .id("document-pane")
                .size_full()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .bg(roles.surface_base)
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_3()
                        .px_3()
                        .py_1()
                        .border_b_1()
                        .border_color(roles.border)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(roles.text_muted)
                                .child(path),
                        )
                        .child(
                            Button::new("close-document")
                                .custom(crate::app::sidebar_action_style(cx))
                                .small()
                                .compact()
                                .icon(IconName::X)
                                .accessibility_label("Close preview")
                                .tooltip("Close preview")
                                .on_click(cx.listener(|app, _, window, cx| {
                                    app.clear_document();
                                    app.focus_terminal(window, cx);
                                    cx.notify();
                                })),
                        ),
                )
                .child(div().flex_1().min_w_0().min_h_0().child(body))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{Render, TestAppContext};

    /// The production body: one retained view, drawn as the window draws it.
    struct Harness {
        view: Option<DocumentView>,
        document: Option<(String, String)>,
    }

    impl Render for Harness {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let Some((path, text)) = self.document.clone() else {
                return div().size_full().into_any_element();
            };
            let state = document_state(
                &mut self.view,
                &format!("{path}\0{text}"),
                &path,
                &text,
                window,
                cx,
            );
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(
                    div().flex_1().min_w_0().min_h_0().child(
                        Editor::new(&state)
                            .readonly(true)
                            .bordered(false)
                            .size_full(),
                    ),
                )
                .into_any_element()
        }
    }

    fn open(cx: &mut TestAppContext) -> (gpui_kit::AnyWindowHandle, Entity<Harness>) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::install(cx);
        });
        cx.update(|cx| {
            let bounds = gpui_kit::Bounds {
                origin: gpui_kit::Point::default(),
                size: gpui_kit::size(gpui_kit::px(800.), gpui_kit::px(480.)),
            };
            let options = gpui_kit::WindowOptions {
                window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)),
                ..Default::default()
            };
            let (window, view) = gpui_kit::open_window(options, cx, |_, cx| {
                cx.new(|_| Harness {
                    view: None,
                    document: None,
                })
            })
            .expect("open test window");
            (window, view)
        })
    }

    /// The editor the harness is currently drawing.
    fn editor(cx: &mut TestAppContext, harness: &Entity<Harness>) -> Entity<EditorState> {
        harness.read_with(cx, |harness, _| {
            harness
                .view
                .as_ref()
                .expect("a document is open")
                .state
                .clone()
        })
    }

    /// The editor's own element id, which is what a pointer event addresses.
    fn at(state: &Entity<EditorState>) -> (&'static str, gpui_kit::EntityId) {
        ("input", state.entity_id())
    }

    #[test]
    fn the_language_comes_from_the_path_and_falls_back_to_plain_text() {
        for (path, language) in [
            ("package.json", "json"),
            ("src/main.rs", "rust"),
            ("app.js", "javascript"),
            ("app.mjs", "javascript"),
            ("types.d.ts", "typescript"),
            ("view.tsx", "tsx"),
            ("ci.yaml", "yaml"),
            ("ci.yml", "yaml"),
            ("index.html", "html"),
            ("site.css", "css"),
            ("README.md", "markdown"),
            ("LICENSE", "plaintext"),
            ("Makefile", "plaintext"),
            ("notes.rst", "plaintext"),
            ("src/main.rs.bak", "plaintext"),
        ] {
            assert_eq!(syntax_language(path), language, "{path}");
        }
    }

    #[gpui_kit::test]
    fn one_editor_serves_the_document_and_a_new_file_replaces_it(cx: &mut TestAppContext) {
        let (window, harness) = open(cx);
        harness.update(cx, |harness, cx| {
            harness.document = Some(("src/main.rs".into(), "fn main() {}\n".into()));
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();
        let first = editor(cx, &harness);
        let first_id = first.entity_id();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert_eq!(
            editor(cx, &harness).entity_id(),
            first_id,
            "a redraw keeps the editor"
        );

        harness.update(cx, |harness, cx| {
            harness.document = Some(("Cargo.toml".into(), "[package]\n".into()));
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();
        assert_ne!(
            editor(cx, &harness).entity_id(),
            first_id,
            "another file is another editor"
        );
    }

    #[gpui_kit::test]
    fn the_document_is_readable_whole_and_cannot_be_edited(cx: &mut TestAppContext) {
        let (window, harness) = open(cx);
        let text = "{\n  \"name\": \"muster\",\n  \"empty\": \"\"\n}\n";
        harness.update(cx, |harness, cx| {
            harness.document = Some(("Cargo.toml".into(), text.into()));
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();
        let state = editor(cx, &harness);
        let id = at(&state);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find(id).value(), Some(text));
            // Read-only: the text goes nowhere, and the editor still took it.
            window.click(id, cx);
            window.input("!", cx);
        })
        .unwrap();
        cx.read(|cx| {
            let state = state.read(cx);
            // A trailing newline is a line of the file, not a wrapping detail.
            assert_eq!(state.text().to_string(), text);
            assert!(state.presentation().is_readonly());
            assert!(state.presentation().is_code_editor());
        });

        // Selectable and copyable: selection is how a reader takes a name out.
        let (selected, copied) = cx
            .update_window(window, |_, window, cx| {
                window.click(id, cx);
                window.press("secondary-a", cx);
                let selected = state.read(cx).selected_text().to_string();
                window.press("secondary-c", cx);
                let copied = cx
                    .read_from_clipboard()
                    .expect("copy wrote the clipboard")
                    .text();
                (selected, copied)
            })
            .unwrap();
        assert_eq!(selected, text);
        assert_eq!(copied.as_deref(), Some(text));
    }

    #[gpui_kit::test]
    fn an_empty_file_is_an_empty_editor_and_not_a_missing_one(cx: &mut TestAppContext) {
        let (window, harness) = open(cx);
        harness.update(cx, |harness, cx| {
            harness.document = Some(("LICENSE".into(), String::new()));
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();
        let state = editor(cx, &harness);
        let id = at(&state);
        cx.update_window(window, |_, window, _cx| {
            assert_eq!(window.find(id).value(), Some(""));
        })
        .unwrap();
        cx.read(|cx| assert_eq!(state.read(cx).text().to_string(), ""));
    }
    #[gpui_kit::test]
    fn json_grammar_really_produces_different_token_styles(cx: &mut TestAppContext) {
        let (window, harness) = open(cx);
        let source = "{\"name\": \"muster\", \"count\": 42, \"ready\": true}\n";
        harness.update(cx, |h, cx| {
            h.document = Some(("package.json".into(), source.into()));
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .unwrap();
        let state = editor(cx, &harness);
        cx.update(|cx| {
            let mut highlighter = gpui_kit::component::highlighter::SyntaxHighlighter::new("json");
            let text = state.read(cx).text().clone();
            assert!(highlighter.update(None, &text, None));
            let theme = gpui_kit::component::Theme::global(cx);
            let styles = highlighter.styles(&(0..source.len()), theme.highlight_theme.as_ref());
            let colors = styles
                .iter()
                .filter_map(|(_, style)| style.color)
                .collect::<Vec<_>>();
            assert!(colors.len() > 2);
            assert!(
                colors.iter().any(|color| color != &colors[0]),
                "JSON is not monochrome"
            );
        });
        let old = state.entity_id();
        harness.update(cx, |h, cx| {
            h.document = Some(("package.json".into(), "{}\n".into()));
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .unwrap();
        let updated = editor(cx, &harness);
        assert_ne!(
            updated.entity_id(),
            old,
            "same path with refreshed content is not stale"
        );
        updated.read_with(cx, |state, _| assert_eq!(state.text().to_string(), "{}\n"));
    }
}
