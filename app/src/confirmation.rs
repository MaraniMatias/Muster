//! Cross-platform confirmations, hosted by the toolkit's modal and focus layers.

use std::{cell::RefCell, rc::Rc};

use gpui_kit::component::{
    button::{Button, ButtonVariant, ButtonVariants},
    dialog::{Confirm, DialogEntrance, DialogFooter},
    WindowExt,
};
use gpui_kit::{App, InteractiveElement, ParentElement, SharedString, Window};

/// Choice zero is always cancellation, including Escape and an unqualified Enter.
/// Close before answering so a continuation can safely open its own dialog.
pub(crate) fn ask(
    window: &mut Window,
    cx: &mut App,
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
    choices: &'static [(&'static str, ButtonVariant)],
    answer: impl FnOnce(usize, &mut Window, &mut App) + 'static,
) {
    let title = title.into();
    let detail = detail.into();
    let answer = Rc::new(RefCell::new(Some(answer)));
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let cancel = answer.clone();
        dialog
            .title(title.clone())
            .description(detail.clone())
            .entrance(DialogEntrance::None)
            .footer(
                DialogFooter::new().children(choices.iter().enumerate().map(
                    |(choice, (label, variant))| {
                        let answer = answer.clone();
                        let choose = Rc::new(move |window: &mut Window, cx: &mut App| {
                            let answer = answer.borrow_mut().take();
                            if let Some(answer) = answer {
                                window.close_dialog(cx);
                                answer(choice, window, cx);
                            }
                        });
                        let click = choose.clone();
                        Button::new(("confirmation-choice", choice))
                            .label(*label)
                            .with_variant(*variant)
                            .on_click(move |_, window, cx| click(window, cx))
                            // The dialog binds Enter to Confirm before GPUI's native
                            // keyboard click. Let the focused choice answer it.
                            .on_action(move |_: &Confirm, window, cx| choose(window, cx))
                    },
                )),
            )
            .on_close(move |_, window, cx| {
                let answer = cancel.borrow_mut().take();
                if let Some(answer) = answer {
                    answer(0, window, cx);
                }
            })
    });
}
