//! Adapting the window's key events to the terminal's own.
//!
//! `keys::encode_key` knows what a child expects to read. It does not know that
//! a Mac calls its Command key "platform", that a Portuguese layout puts `?`
//! behind a dead key, or that a keystroke with no `key_char` is a control key
//! rather than an unknown one. All of that arrives here, and the only thing
//! that leaves is a [`KeyPress`] or a refusal.
//!
//! ### One source of text, on purpose
//!
//! A physical press can reach a view twice: once as a native key event and once
//! as committed text from the input method. Sending both is how a shell ends up
//! with `lls` for two presses of `l`, and it is invisible until something
//! breaks. **The native key event is the only source here.** This view never
//! registers a [`gpui_kit::InputHandler`], so the text-input path never fires
//! for it and there is no second event to deduplicate.
//!
//! What that costs is a real IME *composition* -- the underlining text a person
//! is halfway through typing. A commit is not lost, though: it arrives on the
//! key event as a multi-character `key_char`, and it is carried whole in
//! [`KeyPress::text`] for the encoder to send as bytes. What is lost is the
//! in-progress marking, which is the next milestone's problem and not this one's.

use gpui_kit::Keystroke;

use super::{Key, KeyPress};

/// What one keystroke means to the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// Send this press to the child.
    Press(KeyPress),
    /// The application owns this key. Nothing is sent, and nothing is logged:
    /// every window has keys that are not the terminal's, and one of them
    /// being ignored is not an event.
    Application,
}

/// Where the keyboard is, from the terminal's point of view.
///
/// The view both gates input on this and draws its cursor from it, which is why
/// it is a type rather than two booleans: the two questions have different
/// answers, and collapsing them is how an inactive window keeps eating
/// keystrokes or an active one draws a cursor nobody can see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusState {
    /// This terminal's own focus handle holds the window's focus.
    pub focused: bool,
    /// The operating system is typing into this window.
    pub window_active: bool,
}

impl FocusState {
    /// Whether a keystroke should reach the child.
    ///
    /// Both halves, not either. Focus without an active window is a leftover
    /// handle, and an active window without focus is somebody else's window.
    pub fn accepts_input(self) -> bool {
        self.focused && self.window_active
    }

    /// Whether the cursor is drawn as though nobody is typing.
    pub fn draws_unfocused_cursor(self) -> bool {
        !self.window_active
    }
}

/// Turn one keystroke into a terminal press.
pub fn press(keystroke: &Keystroke) -> Input {
    let modifiers = keystroke.modifiers;

    // Command belongs to the application, not to whatever is in front of it.
    // This is not a nicety on macOS: Cmd+C has to stay a copy shortcut, and
    // the only way to get that is to refuse the key here. Ctrl is a different
    // modifier entirely, so Ctrl+C still reaches the PTY and still interrupts.
    if modifiers.platform {
        return Input::Application;
    }

    let (key, text) = match named_key(&keystroke.key) {
        Some(key) => (key, None),
        None => match printable(keystroke) {
            Some(text) => (
                Key::Char(text.chars().next().expect("printable text is not empty")),
                // Only a commit needs carrying whole; a single character is the
                // key itself and the encoder already knows it.
                (text.chars().count() > 1).then_some(text),
            ),
            None => return Input::Application,
        },
    };

    Input::Press(KeyPress {
        key,
        shift: modifiers.shift,
        ctrl: modifiers.control,
        alt: !cfg!(target_os = "macos") && modifiers.alt,
        // On macOS Option is the ESC prefix; elsewhere Alt is both the keyboard
        // protocol's modifier and the ESC prefix, which is what `KeyPress::alt`
        // and `KeyPress::meta` are for.
        meta: cfg!(target_os = "macos") && modifiers.alt,
        text,
    })
}

/// The named keys a terminal knows, or `None` for a key that is a character.
///
/// Named first on purpose: the key event for Enter carries a `key_char` of
/// "\n" on some layouts, and a press that means Enter must be Enter whatever
/// character rode in on it.
fn named_key(key: &str) -> Option<Key> {
    Some(match key {
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "delete" => Key::Delete,
        "insert" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "space" => Key::Char(' '),
        key => {
            let number = key.strip_prefix('f')?.parse::<u8>().ok()?;
            // `Key::F` is a `u8` because there are twelve of these keys. F0
            // and F13 encode to nothing in the encoder, and dropping them here
            // is the same answer with less work.
            return (1..=12).contains(&number).then_some(Key::F(number));
        }
    })
}

/// The text this press typed, or `None` when it typed none.
///
/// `key_char` first: on a layout where the character is not on the key -- a
/// dead key, Option on macOS, a Dvorak -- it is the only place the character
/// appears. The bare key name is the fallback for Ctrl, which arrives with no
/// character at all and is exactly the case that has to keep working.
fn printable(keystroke: &Keystroke) -> Option<String> {
    if let Some(text) = keystroke
        .key_char
        .as_deref()
        .filter(|text| !text.is_empty())
        .filter(|text| !text.chars().next().is_some_and(char::is_control))
    {
        return Some(text.to_owned());
    }

    let mut chars = keystroke.key.chars();
    let only = chars.next().filter(|_| chars.next().is_none())?;
    // A control character as the whole key means the platform named a control
    // key this terminal does not have. Sending it raw would put a byte in the
    // child's input that no key on the keyboard asked for.
    (!only.is_control()).then(|| only.to_string())
}

#[cfg(test)]
mod tests {
    use gpui_kit::Modifiers;

    use super::*;

    fn keystroke(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.to_owned(),
            key_char: key_char.map(str::to_owned),
        }
    }

    fn plain(key: &str) -> Keystroke {
        keystroke(key, None, Modifiers::none())
    }

    fn sent(key: &str) -> Key {
        match press(&plain(key)) {
            Input::Press(press) => press.key,
            other => panic!("{key} was not sent: {other:?}"),
        }
    }

    #[test]
    fn every_named_key_the_child_can_read_is_mapped() {
        assert_eq!(sent("enter"), Key::Enter);
        assert_eq!(sent("tab"), Key::Tab);
        assert_eq!(sent("backspace"), Key::Backspace);
        assert_eq!(sent("escape"), Key::Escape);
        assert_eq!(sent("up"), Key::Up);
        assert_eq!(sent("pagedown"), Key::PageDown);
        assert_eq!(sent("space"), Key::Char(' '));
        assert_eq!(sent("f5"), Key::F(5));
        assert_eq!(sent("f12"), Key::F(12));
    }

    #[test]
    fn a_press_that_names_a_key_this_terminal_lacks_is_the_applications() {
        // F13 and above encode to nothing, and a control key the terminal never
        // heard of is not a character either.
        assert_eq!(press(&plain("f13")), Input::Application);
        assert_eq!(press(&plain("f0")), Input::Application);
        assert_eq!(
            press(&plain("capslock")),
            Input::Application,
            "an unmapped name must not become text"
        );
    }

    #[test]
    fn command_belongs_to_the_application_and_control_does_not() {
        let command = keystroke(
            "c",
            None,
            Modifiers {
                platform: true,
                ..Modifiers::none()
            },
        );
        assert_eq!(press(&command), Input::Application);

        let interrupt = keystroke(
            "c",
            None,
            Modifiers {
                control: true,
                ..Modifiers::none()
            },
        );
        let Input::Press(press) = press(&interrupt) else {
            panic!("Ctrl+C was taken by the application");
        };
        assert!(press.ctrl, "Ctrl has to reach the PTY to interrupt the child");
        assert_eq!(press.key, Key::Char('c'));
    }

    #[test]
    fn a_layout_that_puts_the_character_elsewhere_still_types_it() {
        let press = press(&keystroke("s", Some("ß"), Modifiers::none()));
        let Input::Press(press) = press else {
            panic!("option-s typed nothing");
        };
        assert_eq!(press.key, Key::Char('ß'));
    }

    #[test]
    fn a_committed_composition_is_carried_whole_as_text() {
        let press = press(&keystroke("key", Some("日本"), Modifiers::none()));
        let Input::Press(press) = press else {
            panic!("a committed composition typed nothing");
        };
        assert_eq!(
            press.text.as_deref(),
            Some("日本"),
            "one commit, whole, on the one event that reported it"
        );
    }

    #[test]
    fn a_single_character_press_is_the_key_itself_and_needs_no_text() {
        let Input::Press(press) = press(&keystroke("a", Some("a"), Modifiers::none())) else {
            panic!("`a` typed nothing");
        };
        assert_eq!(press.key, Key::Char('a'));
        assert_eq!(press.text, None);
    }

    #[test]
    fn focus_gates_input_and_the_cursor_separately() {
        let both = FocusState {
            focused: true,
            window_active: true,
        };
        assert!(both.accepts_input());
        assert!(!both.draws_unfocused_cursor());

        // A handle that still holds focus in a window nobody is typing into.
        let inactive = FocusState {
            focused: true,
            window_active: false,
        };
        assert!(!inactive.accepts_input(), "an inactive window keeps nothing");
        assert!(inactive.draws_unfocused_cursor());
    }
}