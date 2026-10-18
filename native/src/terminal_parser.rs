//! Parser-level regression tests for the vendored `alacritty_terminal` 0.26.0.
//!
//! `push_keyboard_mode` caps the kitty keyboard mode stack, and the released code
//! checked `keyboard_mode_stack` but removed from `title_stack`. With no title ever
//! pushed that is `Vec::remove(0)` on an empty vector, on the reader thread, from
//! bytes any program inside the terminal can write. The vendored copy carries the
//! one-line fix; these tests are what keeps it from being undone.
//!
//! Nothing here talks to a PTY: every case drives the parser directly, so the tests
//! are deterministic and do not depend on a shell, a window or timing.

use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};

const COLUMNS: usize = 80;
const SCREEN_LINES: usize = 24;

/// `KEYBOARD_MODE_STACK_MAX_DEPTH` in the vendored crate
/// (`native/vendor/alacritty_terminal-0.26.0/src/term/mod.rs:48`). Pushing one entry
/// past it is what reaches the removal the fix is about
/// (`native/vendor/alacritty_terminal-0.26.0/src/term/mod.rs:1295`).
const KEYBOARD_MODE_STACK_MAX_DEPTH: usize = 4096;

/// The push every flood case sends: `CSI > 1 u`, which is
/// `KeyboardModes::DISAMBIGUATE_ESC_CODES`. Nothing pops, so the stack walks straight
/// past its cap.
const PUSH_KEYBOARD_MODE: &[u8] = b"\x1b[>1u";

/// How far past the cap each flood goes. `+ 64` is well clear of the one-entry
/// overshoot the cap produces.
const FLOOD_DEPTH: usize = KEYBOARD_MODE_STACK_MAX_DEPTH + 64;

/// `EventListener::send_event` takes `&self`, so the collector shares one
/// `Mutex<Vec<Event>>` with the test instead of holding the events itself.
struct EventCollector {
    events: Arc<Mutex<Vec<Event>>>,
}

impl EventListener for EventCollector {
    fn send_event(&self, event: Event) {
        self.events
            .lock()
            .expect("event collector mutex poisoned")
            .push(event);
    }
}

/// A kitty-keyboard-enabled terminal plus a handle on the events it emitted.
fn terminal() -> (Term<EventCollector>, Arc<Mutex<Vec<Event>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let config = Config {
        kitty_keyboard: true,
        ..Config::default()
    };
    let size = TermSize::new(COLUMNS, SCREEN_LINES);
    let collector = EventCollector {
        events: Arc::clone(&events),
    };

    (Term::new(config, &size, collector), events)
}

/// Take everything the terminal has reported so far.
fn collected(events: &Mutex<Vec<Event>>) -> Vec<Event> {
    std::mem::take(&mut *events.lock().expect("event collector mutex poisoned"))
}

/// Push keyboard modes until the stack is over its cap.
fn flood(parser: &mut Processor, term: &mut Term<EventCollector>) {
    for _ in 0..FLOOD_DEPTH {
        parser.advance(&mut *term, PUSH_KEYBOARD_MODE);
    }
}

#[test]
fn a_keyboard_mode_flood_does_not_panic_with_an_empty_title_stack() {
    let (mut term, _events) = terminal();
    let mut parser: Processor = Processor::new();

    // The title stack is never touched, which is the ordinary state: most terminals
    // never push a title. Reaching the cap therefore has to evict from the keyboard
    // mode stack (`KEYBOARD_MODE_STACK_MAX_DEPTH`, 4096 entries, `src/term/mod.rs:48`)
    // or the removal at `src/term/mod.rs:1296` pops an empty vector and kills the
    // parser thread. Reaching it is the whole test; surviving it is the assertion.
    flood(&mut parser, &mut term);
}

#[test]
fn the_terminal_still_renders_after_a_keyboard_mode_flood() {
    let (mut term, _events) = terminal();
    let mut parser: Processor = Processor::new();

    flood(&mut parser, &mut term);
    parser.advance(&mut term, b"\x1b[31mred\x1b[0m");

    const RED: [char; 3] = ['r', 'e', 'd'];
    let grid = term.grid();
    let (line, column) = (0..SCREEN_LINES)
        .flat_map(|line| (0..COLUMNS).map(move |column| (line, column)))
        .find(|&(line, column)| {
            column + RED.len() <= COLUMNS
                && RED.iter().enumerate().all(|(offset, &expected)| {
                    grid[Line::from(line)][Column(column + offset)].c == expected
                })
        })
        .expect("`red` should be rendered in three consecutive cells of a visible row");

    for (offset, &expected) in RED.iter().enumerate() {
        let cell = &grid[Line::from(line)][Column(column + offset)];
        assert_eq!(cell.c, expected, "cell {offset} of `red`");
        assert_eq!(
            cell.fg,
            Color::Named(NamedColor::Red),
            "foreground of cell {offset} of `red`"
        );
    }

    // `CSI > 1 u` asks for disambiguated escape codes and nothing else was pushed, so a
    // stack the flood left intact reports exactly that flag. A stack the overflow
    // branch had emptied or corrupted would report no keyboard mode at all.
    assert!(
        term.mode().intersects(TermMode::DISAMBIGUATE_ESC_CODES),
        "keyboard mode after the flood: {:?}",
        *term.mode()
    );
}

#[test]
fn a_keyboard_mode_flood_does_not_disturb_window_titles() {
    let (mut term, events) = terminal();
    let mut parser: Processor = Processor::new();

    parser.advance(&mut term, b"\x1b]0;before\x07");
    flood(&mut parser, &mut term);
    parser.advance(&mut term, b"\x1b]0;after\x07");

    // `Term::title` is private, so the title the terminal reports is the `Event::Title`
    // the listener was handed. The flood must neither drop nor reorder it.
    let titles: Vec<String> = collected(&events)
        .into_iter()
        .filter_map(|event| match event {
            Event::Title(title) => Some(title),
            _ => None,
        })
        .collect();

    assert_eq!(titles, ["before", "after"]);
}

#[test]
fn kitty_keyboard_negotiation_still_replies_after_the_fix() {
    let (mut term, events) = terminal();
    let mut parser: Processor = Processor::new();

    // `CSI > 1 u` pushes DISAMBIGUATE_ESC_CODES; `CSI ? u` is answered by
    // `report_keyboard_mode` (`native/vendor/alacritty_terminal-0.26.0/src/term/mod.rs:1275`)
    // with `CSI ? Ps u` naming the mode on top of the stack. A stack that never records
    // the push answers with mode 0, which is how the fix could quietly break
    // modified-key negotiation without panicking.
    parser.advance(&mut term, PUSH_KEYBOARD_MODE);
    parser.advance(&mut term, b"\x1b[?u");

    let replies: Vec<String> = collected(&events)
        .into_iter()
        .filter_map(|event| match event {
            Event::PtyWrite(text) => Some(text),
            _ => None,
        })
        .collect();

    assert!(
        !replies.is_empty(),
        "the keyboard-protocol query should be answered"
    );
    assert!(
        replies
            .last()
            .is_some_and(|reply| reply.starts_with("\x1b[?") && reply.ends_with('u')),
        "the reply should report a keyboard mode, got {replies:?}"
    );
}
