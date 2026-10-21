//! Short-lived messages raised by the window and drawn above its content.
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::Sizable;
use gpui_kit::{
    div, px, AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Role,
    StatefulInteractiveElement, Styled, TestSupportExt,
};

use crate::{app::TerminalWindow, theme::Roles};

const MAX_TOASTS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Level {
    Error,
    Info,
}

impl Level {
    fn lifetime(self) -> Duration {
        match self {
            Self::Error => Duration::from_secs(6),
            Self::Info => Duration::from_secs(3),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Error => "Error",
            Self::Info => "Info",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Toast {
    id: u64,
    generation: u64,
    level: Level,
    message: String,
}

#[derive(Default)]
pub(crate) struct ToastStack {
    items: Vec<Toast>,
    next_id: u64,
}

impl ToastStack {
    fn push(&mut self, message: String, level: Level) -> (u64, u64) {
        if let Some(newest) = self.items.last_mut() {
            if newest.message == message && newest.level == level {
                newest.generation += 1;
                return (newest.id, newest.generation);
            }
        }

        self.next_id += 1;
        let toast = Toast {
            id: self.next_id,
            generation: 0,
            level,
            message,
        };
        let id = toast.id;
        self.items.push(toast);
        if self.items.len() > MAX_TOASTS {
            self.items.remove(0);
        }
        (id, 0)
    }

    #[cfg(test)]
    pub(crate) fn newest_id(&self) -> Option<u64> {
        self.items.last().map(|toast| toast.id)
    }

    #[cfg(test)]
    pub(crate) fn latest_error(&self) -> Option<&str> {
        self.items
            .iter()
            .rev()
            .find(|toast| toast.level == Level::Error)
            .map(|toast| toast.message.as_str())
    }

    #[cfg(test)]
    pub(crate) fn latest_info(&self) -> Option<&str> {
        self.items
            .iter()
            .rev()
            .find(|toast| toast.level == Level::Info)
            .map(|toast| toast.message.as_str())
    }

    fn dismiss(&mut self, id: u64) -> bool {
        let before = self.items.len();
        self.items.retain(|toast| toast.id != id);
        before != self.items.len()
    }

    fn expire(&mut self, id: u64, generation: u64) -> bool {
        if self
            .items
            .iter()
            .any(|toast| toast.id == id && toast.generation == generation)
        {
            self.dismiss(id)
        } else {
            false
        }
    }
}

impl TerminalWindow {
    pub(crate) fn push_error(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.push_toast(message.into(), Level::Error, cx);
    }

    pub(crate) fn push_info(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.push_toast(message.into(), Level::Info, cx);
    }

    fn push_toast(&mut self, message: String, level: Level, cx: &mut Context<Self>) {
        let (id, generation) = self.toasts.push(message, level);
        let lifetime = level.lifetime();
        let weak = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(lifetime).await;
            let _ = weak.update(cx, |app, cx| {
                if app.toasts.expire(id, generation) {
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn dismiss_toast(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.toasts.dismiss(id) {
            cx.notify();
        }
    }

    pub(crate) fn toast_element(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.toasts.items.is_empty() {
            return None;
        }
        let roles = Roles::of(cx);
        let weak = cx.weak_entity();
        Some(
            div()
                .id("toast-stack")
                .test_support()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(16.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(6.))
                .children(self.toasts.items.iter().map(|toast| {
                    let id = toast.id;
                    let message = toast.message.clone();
                    let label = toast.level.label();
                    let ink = match toast.level {
                        Level::Error => roles.status.danger,
                        Level::Info => roles.text_muted,
                    };
                    let dismiss = weak.clone();
                    div()
                        .id(format!("toast-{id}"))
                        .role(match toast.level {
                            Level::Error => Role::Alert,
                            Level::Info => Role::Status,
                        })
                        .test_support()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .w_full()
                        .max_w(px(420.))
                        .rounded(px(8.))
                        .px(px(10.))
                        .py(px(8.))
                        .relative()
                        .border_1()
                        .border_color(roles.border_strong)
                        .bg(roles.surface_raised)
                        .text_color(roles.text)
                        .text_size(px(13.))
                        .shadow_md()
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .top_0()
                                .bottom_0()
                                .w(px(3.))
                                .bg(ink),
                        )
                        .child(div().flex_1().min_w_0().child(message.clone()))
                        .child(
                            Button::new(format!("dismiss-toast-{id}"))
                                .ghost()
                                .small()
                                .compact()
                                .icon(IconName::X)
                                .accessibility_label(format!("Dismiss {label}: {message}"))
                                .on_click(move |_, _, cx| {
                                    let _ = dismiss.update(cx, |app, cx| app.dismiss_toast(id, cx));
                                }),
                        )
                }))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stack_caps_at_three_and_only_restarts_the_newest_duplicate() {
        let mut stack = ToastStack::default();
        let first = stack.push("one".into(), Level::Error);
        stack.push("two".into(), Level::Info);
        let third = stack.push("three".into(), Level::Info);
        let restarted = stack.push("three".into(), Level::Info);
        assert_eq!(restarted, (third.0, third.1 + 1));
        assert_eq!(stack.items.len(), 3);
        stack.push("four".into(), Level::Error);
        assert_eq!(
            stack
                .items
                .iter()
                .map(|toast| toast.message.as_str())
                .collect::<Vec<_>>(),
            ["two", "three", "four"]
        );
        assert!(!stack.expire(third.0, third.1));
        assert!(stack.expire(third.0, restarted.1));
        assert!(!stack.dismiss(first.0));
    }

    #[test]
    fn toast_levels_keep_legacy_lifetimes() {
        assert_eq!(Level::Error.lifetime(), Duration::from_secs(6));
        assert_eq!(Level::Info.lifetime(), Duration::from_secs(3));
    }
}
