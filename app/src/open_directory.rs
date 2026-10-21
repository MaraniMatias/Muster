//! The directory a person typed, for the machines whose picker cannot open.
//!
//! The native picker is the first answer and stays it: on macOS it is the
//! system dialog, and on Linux the desktop portal. A machine may have
//! neither — a portal is a service the desktop session may not run — and then
//! "open a directory" would be a button that does nothing at all. This is the
//! second answer: the path, typed.

use gpui_kit::component::{
    button::{Button, ButtonVariants},
    dialog::{Dialog, DialogContent, DialogFooter},
    input::{Input, InputState},
    Disableable, Sizable,
};
use gpui_kit::{div, px, AnyElement, Context, Entity, IntoElement, ParentElement, Styled};

use crate::app::TerminalWindow;
use crate::theme::Roles;

/// The path being typed, what the last attempt to open it said, and whether an
/// attempt is running.
pub(crate) struct OpenDirectoryDraft {
    pub(crate) path: Entity<InputState>,
    pub(crate) error: Option<String>,
    pub(crate) busy: bool,
}

impl TerminalWindow {
    /// The dialog that asks for a directory by path.
    ///
    /// It is the second answer, not the first: the native picker opens first
    /// and this dialog opens only when that picker could not.
    pub(crate) fn open_directory_dialog(
        &self,
        draft: &OpenDirectoryDraft,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let roles = Roles::of(cx);
        // The content builder runs again on every repaint, so it holds the
        // input entity and builds the field itself rather than closing over an
        // element it would have to hand out twice.
        let path = draft.path.clone();
        let error = draft.error.clone();
        let busy = draft.busy;
        let cancel = cx.weak_entity();
        let cancel_dialog = cancel.clone();
        let submit = cx.weak_entity();
        Dialog::new(cx)
            .bg(roles.surface_raised)
            .border_color(roles.border_strong)
            .width(px(440.))
            .title("Open directory")
            .content(move |content: DialogContent, _, _| {
                let mut content = content
                    .child(div().text_size(px(12.)).text_color(roles.text_muted).child(
                        "The system picker could not open, so type the path of the directory.",
                    ))
                    .child(div().mt_3().text_size(px(12.)).child("Path"))
                    .child(
                        Input::new(&path)
                            .small()
                            .bg(roles.surface_base)
                            .aria_label("Directory path")
                            .w_full(),
                    );
                if let Some(error) = error.clone() {
                    content = content.child(
                        div()
                            .mt_2()
                            .text_size(px(12.))
                            .text_color(roles.status.danger_text)
                            .child(error),
                    );
                }
                content
            })
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new("open-directory-cancel")
                            .label("Cancel")
                            .disabled(busy)
                            .on_click(move |_, _, cx| {
                                let _ = cancel.update(cx, |app, cx| {
                                    app.open_directory = None;
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        Button::new("open-directory-submit")
                            .primary()
                            .label(if busy { "Opening…" } else { "Open directory" })
                            .disabled(busy)
                            .on_click(move |_, window, cx| {
                                let _ = submit.update(cx, |app, cx| {
                                    app.submit_open_directory(window, cx);
                                });
                            }),
                    ),
            )
            .on_cancel(move |_, _, cx| {
                let _ = cancel_dialog.update(cx, |app, cx| {
                    app.open_directory = None;
                    cx.notify();
                });
                false
            })
            .into_any_element()
    }
}
