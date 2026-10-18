//! The window's tooltips: the wait a trigger pays, and the popup it wears.
//!
//! The toolkit's own tooltip is one overlay for the whole window, and it
//! switches: a trigger entered while the last tooltip is still on screen takes
//! its place at once, so the wait is paid once per session rather than once per
//! row. Browsing the file tree that way paints the path of every row the
//! pointer crosses without ever resting on it. Here every trigger pays the
//! wait: a pointer that moves from one row to the next dismisses what was
//! there and starts again.
//!
//! A trigger watches its own bounds rather than its hover, because the control
//! that wears a tooltip is often one that cannot be hovered: a disabled button
//! still has to say why it is disabled.

use std::time::Duration;

use gpui_kit::{
    base::{Root, TooltipDefaults, TooltipOverlay, TooltipRequest, TooltipTransition},
    canvas,
    component::{
        animation::{ease_out_cubic, EffectTransition},
        tooltip::Tooltip,
    },
    div, px, AnyElement, AnyView, App, AppContext, Bounds, Context, ElementId, Entity,
    InteractiveElement, IntoElement, MouseDownEvent, MouseExitEvent, MouseMoveEvent, ParentElement,
    Pixels, Render, SharedString, Styled, TestSupportExt, Window,
};

use crate::app::TerminalWindow;

/// The slide-down an arriving tooltip takes.
const ARRIVAL: Duration = Duration::from_millis(150);

/// The window's tooltips: the trigger the pointer is resting on, and the
/// overlay that draws it. One per window, because a trigger belongs to the
/// window it was painted in.
pub(crate) struct Tooltips {
    overlay: Entity<TooltipOverlay>,
    /// The trigger whose tooltip is up or on its way, by its bounds.
    resting: Option<Bounds<Pixels>>,
}

impl Tooltips {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            overlay: cx.new(|_| TooltipOverlay::new().render_with(render_tooltip)),
            resting: None,
        }
    }

    /// The pointer came to rest on a trigger.
    ///
    /// Whatever was up goes first: the overlay's grace period would otherwise
    /// put this one on screen without a wait, and clearing it is what makes the
    /// wait this trigger's own.
    fn rest(
        &mut self,
        bounds: Bounds<Pixels>,
        text: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.resting == Some(bounds) {
            return;
        }
        self.resting = Some(bounds);
        let delay = TooltipDefaults::global(cx).show_delay();
        self.overlay.update(cx, |overlay, cx| {
            overlay.hide(cx);
            overlay.request_show(
                TooltipRequest::new(bounds, move |window, cx| {
                    Tooltip::new(text.clone()).build(window, cx)
                })
                .with_show_delay(delay),
                window,
                cx,
            );
        });
    }

    /// The pointer left a trigger. Another one may already own the overlay, and
    /// this says nothing about that one.
    fn lift(&mut self, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        if self.resting == Some(bounds) {
            self.hide(cx);
        }
    }

    /// Nothing stays up: the pointer left the window, or pressed a button.
    fn hide(&mut self, cx: &mut Context<Self>) {
        self.resting = None;
        self.overlay.update(cx, |overlay, cx| overlay.hide(cx));
    }
}

impl Render for Tooltips {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let exited = cx.weak_entity();
        let pressed = exited.clone();
        div()
            .absolute()
            .inset_0()
            .child(self.overlay.clone())
            // The pointer can leave the window without a final move, and a
            // press is a decision: what was named is no longer wanted.
            .child(canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    window.on_mouse_event(move |_: &MouseExitEvent, phase, _, cx| {
                        if phase.bubble() {
                            let _ = exited.update(cx, |tooltips, cx| tooltips.hide(cx));
                        }
                    });
                    window.on_mouse_event(move |_: &MouseDownEvent, phase, _, cx| {
                        if phase.bubble() {
                            let _ = pressed.update(cx, |tooltips, cx| tooltips.hide(cx));
                        }
                    });
                },
            ))
    }
}

/// The arrival the toolkit gives its own tooltip, kept so this host is the same
/// popup it was. Nothing is ever replaced in place here — each trigger waits its
/// own turn — so `Switch` is a transition this host does not ask for.
fn render_tooltip(
    view: AnyView,
    transition: TooltipTransition,
    _: &mut Window,
    _: &mut App,
) -> AnyElement {
    let popup = div().id("tooltip").test_support().child(view);
    match transition {
        TooltipTransition::Enter { epoch } => EffectTransition::new(ARRIVAL)
            .ease(ease_out_cubic)
            .slide_y(px(4.), px(0.))
            .fade(0.0, 1.0)
            .apply(
                popup,
                ElementId::NamedInteger("tooltip-arrival".into(), epoch as u64),
            )
            .into_any_element(),
        TooltipTransition::Switch { .. } => popup.into_any_element(),
    }
}

/// A tooltip that appears once the pointer has rested on its trigger.
pub(crate) trait RestingTooltip: ParentElement + Sized {
    /// The tooltip this trigger wears, waited for afresh on every trigger.
    ///
    /// The trigger is a canvas laid over the control and a window listener it
    /// registers as it is painted: a hitbox would be a hover, and hover is what
    /// a disabled button does not have.
    fn resting_tooltip(self, text: impl Into<SharedString>) -> Self {
        let text = text.into();
        self.child(
            canvas(
                |bounds, _, _| bounds,
                move |_, bounds, window, cx| {
                    let Some(tooltips) = tooltips(window, cx) else {
                        return;
                    };
                    window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
                        if !phase.bubble() {
                            return;
                        }
                        let resting = bounds.contains(&window.mouse_position());
                        tooltips.update(cx, |tooltips, cx| {
                            if resting {
                                tooltips.rest(bounds, text.clone(), window, cx);
                            } else {
                                tooltips.lift(bounds, cx);
                            }
                        });
                    });
                },
            )
            .absolute()
            .inset_0(),
        )
    }
}

impl<E: ParentElement + Sized> RestingTooltip for E {}

/// The tooltips of the window the pointer is in.
///
/// Every window the toolkit opens wraps the view it was given, so this is the
/// window's own without a handle carried to each trigger.
fn tooltips(window: &Window, cx: &App) -> Option<Entity<Tooltips>> {
    let root = window.root::<Root>()??;
    let app = root
        .read(cx)
        .view()
        .clone()
        .downcast::<TerminalWindow>()
        .ok()?;
    Some(app.read(cx).tooltips.clone())
}
