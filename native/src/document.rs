//! Read-only central document displayed in place of the terminal.
use crate::{app::TerminalWindow, theme::Roles};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::text::{FrontmatterPlugin, MarkdownExtensions, TextView, TextViewState};
use gpui_kit::component::Sizable;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    div, px, AnyElement, App, AppContext, Context, Entity, Focusable, InteractiveElement,
    IntoElement, ParentElement, Styled, TestSupportExt, Window,
};
use std::sync::Arc;

mod media;

/// The document's editor, kept between renders.
///
/// The state is one entity created when the document is opened and read from
/// every frame after: a text selection, a scroll offset and a parse are then
/// things the reader did rather than things the last frame threw away. Nothing
/// here is written back — the document is a view of a file, and this slice owns
/// no editing and no persistence.
pub(crate) struct DocumentView {
    pub(crate) state: Entity<EditorState>,
    /// The reading view of a Markdown file, built when the document is opened and
    /// kept beside the editor so the two views are one toggle rather than a second
    /// read of the file.
    pub(crate) preview: Option<Entity<TextViewState>>,
    /// Whether this document is being read as a document or as its source. True
    /// for a Markdown file that opens rendered, false for everything else.
    rendered: bool,
    /// The document this state was built for. A different path replaces it: a
    /// selection or an offset carried over from another file would describe a
    /// position in a file that is not on screen.
    path: String,
    /// The pictures this document's reading has named, and the bytes that came
    /// back for them. Owned by the document rather than by the window, so a
    /// different document — or closing this one — drops them with the view, and
    /// a read still in flight has nothing to publish into afterwards.
    pub(crate) media: Arc<media::MediaStore>,
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
        let markdown = is_markdown(path);
        let preview = markdown.then(|| cx.new(|cx| TextViewState::markdown(text, cx)));
        Self {
            state,
            preview,
            rendered: markdown,
            path: identity.to_owned(),
            media: media::MediaStore::new(),
        }
    }

    /// Whether this document is being read as a document rather than as source.
    ///
    /// A file that is not Markdown has no second reading, so the question only has
    /// two answers where there are two views to choose between.
    fn is_rendered(&self) -> bool {
        self.rendered && self.preview.is_some()
    }

    /// The reading that is on screen, and the keyboard that goes with it.
    ///
    /// A document has two readings and mounts one of them, so the focus belongs to
    /// the one mounted: on the editor while the source is what the pane draws, and
    /// on the text view while the reading is. Focusing the editor behind a rendered
    /// document is what made a Markdown file look like the keyboard had stopped, and
    /// leaving focus on the reading that just went away is the other half of it. Both
    /// handles are the toolkit's own — `EditorState` and `TextViewState` each carry
    /// one, and the toolkit tracks them on the elements it draws.
    fn focus_visible(&self, window: &mut Window, cx: &mut App) {
        if self.is_rendered() {
            if let Some(preview) = self.preview.as_ref() {
                preview.read(cx).focus_handle().clone().focus(window, cx);
                return;
            }
        }
        self.state.read(cx).focus_handle(cx).focus(window, cx);
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

/// Whether a path names a Markdown file.
///
/// Legacy's own set (`legacy/src/lib/markdown-preview.ts:20`), which is the same
/// four names `syntax_language` answers `markdown` for: a file whose whole point
/// is to be read, rather than one whose point is that something runs it.
pub(crate) fn is_markdown(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        matches!(
            extension.to_ascii_lowercase().as_str(),
            "md" | "markdown" | "mdown" | "mkd"
        )
    })
}

/// The toolkit language name a path asks for.
///
/// The same list Legacy chose its mode by, reduced to the languages the
/// toolkit compiles in. A name it has no grammar for is plain text rather than
/// nothing: an unknown file still opens, it just opens uncoloured.
pub(crate) fn syntax_language(path: &str) -> &'static str {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase();
    match name.as_str() {
        "makefile" | "gnumakefile" => return "make",
        "cmakelists.txt" => return "cmake",
        ".bash_login" | ".bash_logout" | ".bash_profile" | ".bashrc" | ".profile" | ".shrc"
        | ".cshrc" | ".kshrc" | ".login" | ".tcshrc" | ".zlogin" | ".zlogout" | ".zprofile"
        | ".zshenv" | ".zshrc" => return "bash",
        ".babelrc" | ".eslintrc" | ".jshintrc" | ".prettierrc" | ".swcrc" | ".watchmanconfig" => {
            return "json"
        }
        ".psqlrc" => return "sql",
        _ => {}
    }
    match name.rsplit_once('.').map_or("", |(_, extension)| extension) {
        "json" | "jsonc" => "json",
        "rs" => "rust",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "yaml" | "yml" => "yaml",
        "html" | "htm" => "html",
        "css" | "scss" => "css",
        "md" | "mdown" | "markdown" => "markdown",
        "bash" | "sh" | "zsh" => "bash",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => "cpp",
        "cs" => "csharp",
        "cmake" => "cmake",
        "diff" | "patch" => "diff",
        "ejs" => "ejs",
        "erb" => "erb",
        "ex" | "exs" => "elixir",
        "go" => "go",
        "gql" | "graphql" => "graphql",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "lua" => "lua",
        "make" | "makefile" | "mk" => "make",
        "php" => "php",
        "proto" => "proto",
        "py" | "pyw" | "pyi" => "python",
        "rb" => "ruby",
        "scala" => "scala",
        "sql" => "sql",
        "svelte" => "svelte",
        "swift" => "swift",
        "toml" => "toml",
        "vue" => "vue",
        "zig" => "zig",
        _ => "plaintext",
    }
}

/// The document's body: the reading, or the source.
///
/// Both fill the pane and scroll themselves, because a document taller than the
/// pane has to be readable from the middle of it. A scrollable text view keeps
/// its own scrollbar; the editor keeps the one it draws over its own viewport.
/// Neither scrolls inside a container here: a container whose only child fills
/// it has nothing to scroll, which is what a Markdown preview without a
/// `scrollable` view is.
///
/// The margin is the pane's, not either reading's, so a document is read the
/// same way whichever of the two is on screen.
fn document_body(
    state: &Entity<EditorState>,
    rendered: Option<Entity<TextViewState>>,
    media: Option<media::DocumentMedia>,
    link_handler: impl Fn(&gpui_kit::SharedString, &gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App)
        + Send
        + Sync
        + 'static,
) -> AnyElement {
    let reading = match rendered {
        Some(preview) => {
            let mut extensions = MarkdownExtensions::default()
                .frontmatter()
                .plugin(FrontmatterPlugin::new());
            // A document's own pictures are answered through the checkout it was
            // read from, and everything it names that is not a picture is said in
            // words instead: see `document::media`.
            if let Some(media) = media {
                extensions = extensions.plugin(media);
            }
            div()
                .id("document-markdown")
                .test_support()
                .size_full()
                .child(
                    TextView::new(&preview)
                        // Scrolling its own content is the text view's, and the
                        // only thing that has the height of the document to
                        // measure it against.
                        .scrollable(true)
                        .size_full()
                        // Legacy's front matter was a collapsible tree above
                        // the document, drawn from the YAML rather than shown
                        // as a wall of keys. This is the toolkit's rendering
                        // of the same node.
                        .markdown_extensions(extensions)
                        .on_link_click(link_handler),
                )
                .into_any_element()
        }
        None => Editor::new(state)
            .readonly(true)
            .bordered(false)
            .size_full()
            .into_any_element(),
    };
    div()
        .size_full()
        .min_w_0()
        .min_h_0()
        .px_6()
        .py_5()
        .child(reading)
        .into_any_element()
}

impl TerminalWindow {
    pub(crate) fn clear_document(&mut self) {
        // Every gesture that replaces what this window shows comes through here
        // first, so this is where a newer view selection is noted. A decision
        // waiting on Git may only act while nothing newer has been asked for.
        self.view_intent = self.view_intent.wrapping_add(1);
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
        // Opened where it was clicked: the handler carries this window weakly
        // into the toolkit's text view and dispatches from the click itself.
        let link_handler = link_handler(cx.entity().downgrade());
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
                // Only the reading that is on screen takes the keyboard: a
                // Markdown document opens as its reading, so the editor behind
                // it stays unfocused until the header asks for the source.
                self.document_view
                    .as_ref()
                    .expect("just installed")
                    .focus_visible(window, cx);
            }
            // A Markdown file is read as a document and can be read as its source;
            // everything else is its source and only its source.
            let rendered = self
                .document_view
                .as_ref()
                .filter(|view| view.is_rendered())
                .and_then(|view| view.preview.clone());
            // The pictures a rendered document names are reached through the same
            // checkout the document itself was read from, so a `../` that would
            // leave it is refused where every other path out of it is refused.
            let media = rendered.as_ref().and_then(|_| {
                let repo = self
                    .navigation
                    .repos
                    .iter()
                    .find(|repo| repo.checkouts.iter().any(|one| one.id == checkout_id))?;
                let checkout = self.navigation.checkout(&checkout_id)?;
                Some(media::DocumentMedia::new(
                    repo.clone(),
                    checkout.clone(),
                    &path,
                    Arc::downgrade(&self.document_view.as_ref()?.media),
                ))
            });
            if let Some(media) = media.as_ref() {
                // This frame says what the document already holds and starts the
                // reads its last parse asked for; the bytes arrive later, off the
                // UI thread, and ask for a frame of their own when they do.
                media.prepare(cx.entity().downgrade(), cx);
            }
            document_body(&state, rendered, media, link_handler)
        } else if let Some(error) = self.document_errors.get(&key) {
            let retry_path = path.clone();
            let retry_checkout = checkout_id.clone();
            // A file the pane cannot draw is not a file nobody can open: this is the one
            // place a refusal has something to offer instead. What it cannot be
            // is a file outside the checkout — the opener resolves the path
            // through the checkout again, and refuses it there if it is not there.
            let open_repo = self
                .navigation
                .repos
                .iter()
                .find(|repo| repo.checkouts.iter().any(|c| c.id == checkout_id))
                .cloned();
            let open_checkout = self.navigation.checkout(&checkout_id).cloned();
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
                .when(open_repo.is_some() && open_checkout.is_some(), |pane| {
                    let repo = open_repo.clone().expect("asked for");
                    let checkout = open_checkout.clone().expect("asked for");
                    let opened = path.clone();
                    pane.child(
                        Button::new(format!("open-externally-{path}"))
                            .ghost()
                            .small()
                            .label("Open externally")
                            .accessibility_label(format!("Open {path} in another application"))
                            .on_click(cx.listener(move |app, _, window, cx| {
                                app.open_externally(
                                    repo.clone(),
                                    checkout.clone(),
                                    opened.clone(),
                                    window,
                                    cx,
                                );
                            })),
                    )
                })
                .into_any_element()
        } else {
            div()
                .id("document-loading")
                .p_3()
                .text_color(roles.text_muted)
                .child("Loading file…")
                .into_any_element()
        };
        // A Markdown document is rendered by the toolkit's own text view and not
        // by the editor: the toolkit's view draws headings, lists, tables and
        // fences the way Legacy's preview did, and its fences are coloured by the
        // `muster-content` theme this window already installed. The header says
        // which reading is on the screen and offers the other one.
        let markdown = self
            .document_view
            .as_ref()
            .is_some_and(|view| view.preview.is_some());
        let rendered = self
            .document_view
            .as_ref()
            .is_some_and(DocumentView::is_rendered);
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
                                .child(path.clone()),
                        )
                        .when(markdown, |header| {
                            header.child(
                                Button::new("document-toggle-reading")
                                    .text()
                                    .small()
                                    .compact()
                                    .rounded(px(0.))
                                    .label(if rendered { "Source" } else { "Rendered" })
                                    .accessibility_label(if rendered {
                                        "Read this document as its source"
                                    } else {
                                        "Read this document rendered"
                                    })
                                    .tooltip(if rendered {
                                        "Read the Markdown source"
                                    } else {
                                        "Read the rendered document"
                                    })
                                    .on_click(cx.listener(|app, _, window, cx| {
                                        let Some(view) = app.document_view.as_mut() else {
                                            return;
                                        };
                                        if view.preview.is_some() {
                                            view.rendered = !view.rendered;
                                            // The reading that just went away takes
                                            // the focus with it, and the one now on
                                            // screen has to have it: focus left on
                                            // nothing reads as a window that stopped
                                            // listening, and focus left on the hidden
                                            // editor is what the rendered reading
                                            // used to do.
                                            view.focus_visible(window, cx);
                                            cx.notify();
                                        }
                                    })),
                            )
                        })
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

/// A link clicked in a rendered document, opened where it was clicked.
///
/// The toolkit's text view hands this the `Window` and `App` of the click, and
/// its handler is `Send + Sync`; a weak handle to this window is both of those
/// things. So the click is dispatched from the click rather than left for a
/// later frame to notice, and a window that has been released answers `Err` —
/// answering nothing is what a click on a closed window should do.
pub(crate) fn link_handler(
    view: gpui_kit::WeakEntity<TerminalWindow>,
) -> impl Fn(&gpui_kit::SharedString, &gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App)
       + Send
       + Sync
       + 'static {
    move |url, _, window, cx| {
        let _ = view.update(cx, |app, cx| {
            app.open_link(url.to_string(), window, cx);
        });
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
            let rendered = self
                .view
                .as_ref()
                .filter(|view| view.is_rendered())
                .and_then(|view| view.preview.clone());
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(div().flex_1().min_w_0().min_h_0().child(document_body(
                    &state,
                    rendered,
                    None,
                    |_, _, _, _| {},
                )))
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

    /// Frames until a Markdown parse has landed and the view measured itself.
    fn settle(cx: &mut TestAppContext, window: gpui_kit::AnyWindowHandle) {
        for _ in 0..8 {
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| window.render_frame(cx))
                .unwrap();
        }
    }

    /// A document taller than the pane is read from the middle of it, whichever
    /// reading is on screen.
    #[gpui_kit::test]
    fn a_document_taller_than_the_pane_scrolls_in_both_readings(cx: &mut TestAppContext) {
        let (window, harness) = open(cx);
        // Its source: the editor keeps a viewport onto a file taller than the pane.
        let source = (0..400)
            .map(|line| format!("let line_{line} = {line};\n"))
            .collect::<String>();
        harness.update(cx, |harness, cx| {
            harness.document = Some(("src/main.rs".into(), source));
            cx.notify();
        });
        settle(cx, window);
        let state = editor(cx, &harness);
        let id = at(&state);
        cx.update_window(window, |_, window, cx| {
            window.scroll(
                id,
                // A negative delta is down: the offset is the content's own
                // displacement, so scrolling down moves it away from zero.
                gpui_kit::ScrollDelta::Lines(gpui_kit::point(0., -5.)),
                cx,
            );
        })
        .unwrap();
        assert!(
            state.read_with(cx, |state, _| state.scroll_offset().y) < gpui_kit::px(0.),
            "the source did not scroll"
        );

        // Its reading: the same content as a document, which scrolls itself.
        let rendered = (0..60)
            .map(|block| format!("Block {block}: {}\n\n", "lorem ipsum dolor ".repeat(20)))
            .collect::<String>();
        harness.update(cx, |harness, cx| {
            harness.document = Some(("README.md".into(), rendered));
            cx.notify();
        });
        settle(cx, window);
        let preview = harness.read_with(cx, |harness, _| {
            harness
                .view
                .as_ref()
                .expect("a document is open")
                .preview
                .clone()
                .expect("a Markdown file has a reading")
        });
        assert!(
            preview.read_with(cx, |preview, _| {
                preview.list_state().max_offset_for_scrollbar().y
            }) > gpui_kit::px(0.),
            "the reading has nothing to scroll"
        );
        cx.update_window(window, |_, window, cx| {
            window.scroll(
                "document-markdown",
                gpui_kit::ScrollDelta::Lines(gpui_kit::point(0., -5.)),
                cx,
            );
        })
        .unwrap();
        assert!(
            preview.read_with(cx, |preview, _| {
                preview.list_state().scroll_px_offset_for_scrollbar().y
            }) < gpui_kit::px(0.),
            "the reading did not scroll"
        );
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
            ("Makefile", "make"),
            ("Cargo.toml", "toml"),
            ("src/MAIN.PY", "python"),
            (r"src\main.go", "go"),
            ("CMakeLists.txt", "cmake"),
            ("scripts/build.sh", "bash"),
            ("home/.bashrc", "bash"),
            ("home/.kshrc", "bash"),
            ("home/.login", "bash"),
            ("src/main.cpp", "cpp"),
            ("query.sql", "sql"),
            ("api.proto", "proto"),
            ("schema.graphql", "graphql"),
            ("types.gql", "graphql"),
            ("component.vue", "vue"),
            ("diagram.mmd", "plaintext"),
            ("document.xml", "plaintext"),
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
    fn additional_grammars_produce_legacy_token_colors(cx: &mut TestAppContext) {
        let (window, harness) = open(cx);
        for (path, source) in [
            ("Cargo.toml", "[package]\nname = \"muster\"\nversion = 42\n"),
            ("Makefile", "# comment\nall:\n\techo hello\n"),
            (
                "change.patch",
                "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n",
            ),
            (
                "view.svelte",
                "<script>const name = 'hello';</script><p>{name}</p>",
            ),
            ("view.ejs", "<p><%= name %></p>"),
            ("view.erb", "<p><%= name %></p>"),
            (
                "component.vue",
                "<template><p>{{ name }}</p></template>\n<script>const n = 1;</script>\n",
            ),
            (
                "api.proto",
                "// a service\nsyntax = \"proto3\";\n\nmessage User {\n  string name = 1;\n}\n",
            ),
            (
                "schema.graphql",
                "# a query\nquery Hello($id: ID!) {\n  user(id: $id) { name }\n}\n",
            ),
            ("main.py", "# comment\ndef run():\n    return \"hello\"\n"),
            ("run.sh", "# comment\nif true; then echo \"hello\"; fi\n"),
            (
                "main.go",
                "package main\nfunc main() { println(\"hello\") }\n",
            ),
            ("main.c", "// comment\nint main() { return 42; }\n"),
            ("main.cpp", "// comment\nint main() { return 42; }\n"),
            (
                "Main.java",
                "// comment\nclass Main { String name = \"hello\"; }\n",
            ),
            (
                "main.cs",
                "// comment\nclass Main { string name = \"hello\"; }\n",
            ),
            (
                "query.sql",
                "-- comment\nSELECT 'hello' FROM users WHERE id = 42;\n",
            ),
            ("main.rb", "# comment\ndef run\n  puts \"hello\"\nend\n"),
            ("main.lua", "-- comment\nlocal name = \"hello\"\n"),
            ("main.swift", "// comment\nlet name = \"hello\"\n"),
            ("main.kt", "// comment\nval name = \"hello\"\n"),
            ("main.scala", "// comment\nval name = \"hello\"\n"),
            ("main.php", "<?php // comment\n$name = \"hello\";\n"),
            (
                "main.ex",
                "# comment\ndefmodule Main do\n def name, do: \"hello\"\nend\n",
            ),
            ("main.zig", "// comment\nconst name = \"hello\";\n"),
            ("CMakeLists.txt", "# comment\nset(NAME \"hello\")\n"),
        ] {
            harness.update(cx, |h, cx| {
                h.document = Some((path.into(), source.into()));
                cx.notify();
            });
            cx.update_window(window, |_, window, cx| window.render_frame(cx))
                .unwrap();
            let state = editor(cx, &harness);
            cx.update(|cx| {
                let mut highlighter =
                    gpui_kit::component::highlighter::SyntaxHighlighter::new(syntax_language(path));
                let text = state.read(cx).text().clone();
                assert!(highlighter.update(None, &text, None), "{path}");
                let theme = gpui_kit::component::Theme::global(cx);
                let styles = highlighter.styles(&(0..source.len()), theme.highlight_theme.as_ref());
                let colors: Vec<_> = styles.iter().filter_map(|(_, style)| style.color).collect();
                assert!(
                    colors.iter().any(|color| Some(color) != colors.first()),
                    "{path} is monochrome"
                );
            });
        }
    }

    /// A single-file component is coloured in all three of its blocks.
    ///
    /// The Vue grammar's own query is the markup one — a tag is a tag and an
    /// attribute is an attribute — so `<template>` reads as what it is, and the
    /// two blocks that are *not* markup are reached through the grammar's
    /// injections. Each is asked for separately, because a component whose
    /// `<style>` opens in the editor's own ink still passes a check that only
    /// asks whether the file is coloured somewhere.
    #[gpui_kit::test]
    fn a_vue_component_is_coloured_in_its_script_its_template_and_its_style(
        cx: &mut TestAppContext,
    ) {
        let (window, harness) = open(cx);
        let source = "<script setup lang=\"ts\">\nconst name = 'muster';\n</script>\n\n\
             <template>\n  <p :title=\"name\">{{ name }}</p>\n</template>\n\n\
             <style scoped>\n.name {\n  color: red;\n}\n</style>\n";
        harness.update(cx, |h, cx| {
            h.document = Some(("component.vue".into(), source.into()));
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .unwrap();
        let state = editor(cx, &harness);
        cx.update(|cx| {
            let mut highlighter = gpui_kit::component::highlighter::SyntaxHighlighter::new("vue");
            let text = state.read(cx).text().clone();
            assert!(highlighter.update(None, &text, None), "vue");
            let theme = gpui_kit::component::Theme::global(cx);
            let styles = highlighter.styles(&(0..source.len()), theme.highlight_theme.as_ref());
            let coloured = |block: &str| {
                let range = source
                    .find(block)
                    .map(|at| at..at + block.len())
                    .unwrap_or_else(|| panic!("the component has a {block} block"));
                let mut runs = styles
                    .iter()
                    .filter(|(span, _)| span.start < range.end && span.end > range.start)
                    .filter_map(|(_, style)| style.color)
                    .collect::<Vec<_>>();
                runs.sort_by_key(|color| format!("{color:?}"));
                runs.dedup();
                assert!(runs.len() > 1, "the {block} block is one colour: {runs:?}");
            };
            coloured("const name = 'muster';");
            coloured("<p :title=\"name\">");
            coloured("color: red;");
        });
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
