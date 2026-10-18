//! Turning a key press into the bytes a terminal child expects to read.
//!
//! This is the negotiation half of the terminal. Everything a child can ask for
//! -- application cursor keys, application keypad, the kitty keyboard protocol
//! -- is a different answer to the same key, and the wrong answer is not
//! silence but garbage the child reads as input. So every branch here is keyed
//! on what the child actually negotiated, never on what it might support.
//!
//! Enter is the case that matters most in Muster, because it is the coding
//! agent's newline key: an agent that cannot tell Shift+Enter from Enter has
//! lost the ability to put a line on the wire without running it, and it loses
//! it silently, as a keystroke that came out wrong rather than as an error.

use super::modes::TerminalModes;

/// A key as the view reports it, before any terminal meaning is applied.
///
/// The view maps its own key events onto this and onto nothing else: these are
/// the keys a terminal knows, not the keys a toolkit happens to report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A printable character. What gets sent for it is decided by the press,
    /// not by the character: see [`KeyPress::text`].
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// F1..F12.
    ///
    /// A `u8` because there are exactly twelve of these and no reason to make
    /// the view's mapping wide enough to be wrong. A number outside 1..=12
    /// encodes to nothing rather than to a nearest neighbour: a child has no
    /// reading for F13, and sending it F1 would be worse than sending it
    /// nothing the view can report.
    F(u8),
}

/// One press: the key, everything held while it was pressed, and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPress {
    pub key: Key,
    pub shift: bool,
    pub ctrl: bool,
    /// Alt, as the keyboard protocol encodes it: the bit worth `2`, so Alt on
    /// its own is modifier `3`.
    ///
    /// Not the ESC prefix. On Linux and Windows the view sets both, because
    /// there Alt is both; on macOS Option is [`KeyPress::meta`] and Command
    /// belongs to the app.
    pub alt: bool,
    /// The ESC-prefix modifier: `ESC` in front of whatever the press would have
    /// sent on its own.
    pub meta: bool,
    /// Printable text for this press, or the committed result of an IME
    /// composition. `None` for a key with no text of its own.
    ///
    /// Set by the view rather than derived from `key`, because a commit is
    /// several characters long and arrived on one key event.
    pub text: Option<String>,
}

/// The bytes to send, or `None` when this press has no terminal meaning at all.
///
/// `None` is a decision the caller has to act on -- drop the press -- rather
/// than a byte count of zero. Sending nothing in answer to a key the child will
/// never hear about looks exactly like a dropped key, and for Shift+Enter in a
/// coding agent that is the difference between a newline and a mistake.
pub fn encode_key(press: &KeyPress, modes: &TerminalModes) -> Option<Vec<u8>> {
    let mut bytes = encode_press(press, modes)?;

    // Meta is a prefix and never anything else, which is what makes it work for
    // every key without a second table: `ESC` followed by the encoding this
    // press had on its own. A press with no encoding stays without one -- `ESC`
    // alone is Alt pressed with nothing else, which is a different key rather
    // than an answer to this one.
    if press.meta {
        bytes.insert(0, 0x1b);
    }

    Some(bytes)
}

/// One press, without the ESC prefix [`encode_key`] adds for `meta`.
fn encode_press(press: &KeyPress, modes: &TerminalModes) -> Option<Vec<u8>> {
    // Computed once because most of the branches below ask the same question:
    // is anything held that the negotiated protocol could say something about.
    let modifier = kitty_modifier(press);

    match press.key {
        Key::Char(c) => encode_char(c, press),

        // Enter, which is the key this whole module is careful about.
        //
        // Plain Enter is a carriage return, or a line feed where the child set
        // LNM: those are the same key and the mode is the only thing telling
        // them apart.
        Key::Enter => match modifier {
            None => Some(
                if modes.newline_is_return {
                    b"\n"
                } else {
                    b"\r"
                }
                .to_vec(),
            ),
            // The agent's newline. Only a child that pushed `CSI > 1 u` can tell
            // this from Enter at all, and this is the only form such a child
            // reads as anything but Enter.
            Some(modifier) if modes.disambiguate_esc_codes => {
                Some(format!("\x1b[13;{modifier}u").into_bytes())
            }
            // Without the negotiation there is exactly one legacy byte that is
            // not Enter, and it is LF. Ctrl has no legacy encoding for a key
            // that already sent CR, so it collapses onto the same bytes rather
            // than inventing one the child has never seen.
            Some(_) => Some(if press.shift { b"\n" } else { b"\r" }.to_vec()),
        },

        Key::Tab => match modifier {
            Some(modifier) if modes.disambiguate_esc_codes => {
                Some(format!("\x1b[9;{modifier}u").into_bytes())
            }
            // The one modified Tab that predates the keyboard protocol, and the
            // only legacy way to ask a shell for history completion without
            // binding it to something else.
            Some(_) if press.shift && !press.ctrl && !press.alt => Some(b"\x1b[Z".to_vec()),
            // Ctrl and Alt have no legacy Tab at all. An unmodified Tab is the
            // only thing a legacy child reads here, and sending it that when
            // something else was held would be sending it a key it never asked
            // for.
            _ => Some(b"\t".to_vec()),
        },

        // The arrows. Their negotiated form is `CSI 1 ; modifier A`, which is
        // also why application cursor mode is gone the moment the negotiation
        // happened: a child that pushed `CSI > 1 u` reads all four arrows as
        // CSI, and `SS3 A` arriving under that protocol is a key it cannot see.
        Key::Up | Key::Down | Key::Right | Key::Left => {
            let final_byte = arrow_final_byte(press.key);
            match (modes.disambiguate_esc_codes, modes.app_cursor) {
                (true, _) => Some(csi(&negotiated_parameters(modifier), final_byte)),
                (false, true) => Some(format!("\x1bO{final_byte}").into_bytes()),
                (false, false) => Some(csi("", final_byte)),
            }
        }

        // Home and End are the two keys whose legacy encoding happens to be a
        // CSI letter rather than a `~` number, which is why they can be
        // disambiguated at all without inventing a number for them. The
        // parameter belongs to the negotiated form only: the legacy pair is two
        // fixed bytes in every terminal that has it.
        Key::Home => Some(home_end_key('H', modes, modifier)),
        Key::End => Some(home_end_key('F', modes, modifier)),

        Key::Insert => Some(tilde_key(2, modes, modifier)),
        Key::Delete => Some(tilde_key(3, modes, modifier)),
        Key::PageUp => Some(tilde_key(5, modes, modifier)),
        Key::PageDown => Some(tilde_key(6, modes, modifier)),

        // F1-F4 are the only keys with two legacy encodings: application keypad
        // turns `CSI 11 ~`.. into `SS3 P`.., and the negotiated form is neither.
        Key::F(number) => match number {
            1..=4 => {
                let final_byte = ['P', 'Q', 'R', 'S'][(number - 1) as usize];
                if modes.disambiguate_esc_codes {
                    if let Some(modifier) = modifier {
                        return Some(csi(&negotiated_parameters(Some(modifier)), final_byte));
                    }
                }
                Some(if modes.app_keypad {
                    format!("\x1bO{final_byte}").into_bytes()
                } else {
                    tilde_key(number + 10, modes, None)
                })
            }
            5..=12 => Some(tilde_key(
                FUNCTION_KEY_NUMBERS[(number - 5) as usize],
                modes,
                modifier,
            )),
            // See `Key::F`.
            _ => None,
        },

        // The two keys with no modifier story at all. Their encoding is one
        // byte, and a child reading that byte is reading that byte: there is no
        // parameter to add a modifier to without leaving the protocol.
        Key::Backspace => Some(b"\x7f".to_vec()),
        Key::Escape => Some(b"\x1b".to_vec()),
    }
}

/// Printable input, and the one place in the module with a precedence worth
/// stating out loud.
///
/// Ctrl wins over the text, because Ctrl does not prefix a letter, it *replaces*
/// it: the terminal has one byte for "control-a" and it is not an `a`. Below
/// that the text wins over the character, because an IME commit arrives as text
/// and the `Key::Char` it rode in on says nothing about what was typed.
fn encode_char(c: char, press: &KeyPress) -> Option<Vec<u8>> {
    if press.ctrl {
        // A longer commit under Ctrl has no control encoding in any terminal,
        // so it is refused instead of being sent as though Ctrl were not held.
        let letter = match press.text.as_deref() {
            Some(text) => only_char(text)?,
            None => c,
        };

        return control_byte(letter).map(|byte| vec![byte]);
    }

    // Without Ctrl the text is the input, byte for byte, a multi-character IME
    // commit included. The character is the fallback for a view that had
    // nothing else to report -- and empty text is nothing to report, rather than
    // an instruction to send no bytes at all.
    Some(match press.text.as_deref() {
        Some(text) if !text.is_empty() => text.as_bytes().to_vec(),
        _ => c.to_string().into_bytes(),
    })
}

/// The keyboard protocol's modifier parameter: 1 is nothing held, and each
/// modifier adds its own bit on top -- shift 1, alt 2, ctrl 4.
///
/// `None` rather than `Some(1)` when nothing was held, because a `;1` is the
/// same bytes plus a parameter no child asked for. Every unmodified encoding
/// here has to stay byte-for-byte what it was before this module existed.
fn kitty_modifier(press: &KeyPress) -> Option<u8> {
    // The bits are the ones the protocol documents, and the `1` is what makes
    // them additive: shift alone is `2`, alt alone is `3`, ctrl alone is `5`, so
    // Ctrl+Shift is `6` and Alt+Ctrl is `7`. Shift is a bit like the others, not
    // a fixed `2` -- reading it as a fixed value would turn Shift+Alt into Alt
    // and Shift+Alt+Ctrl into Alt+Ctrl, which is to say it would quietly drop
    // the Shift from every key that also has another modifier on it.
    let value = 1 + u8::from(press.shift) + 2 * u8::from(press.alt) + 4 * u8::from(press.ctrl);
    (value > 1).then_some(value)
}

/// `CSI 1 ; modifier A` for a key whose negotiated form is a CSI letter. Only
/// called on a branch that already knows the negotiation happened.
///
/// The leading `1` goes with the parameter, because an unmodified arrow is
/// `CSI A` in every terminal that has one: emitting `CSI 1 ; 1 A` instead would
/// be the same key and one more parameter no child asked for.
fn negotiated_parameters(modifier: Option<u8>) -> String {
    modifier.map_or_else(String::new, |modifier| format!("1;{modifier}"))
}

/// `CSI <letter>` for Home and End, which gain a modifier parameter only when
/// the child negotiated it.
fn home_end_key(final_byte: char, modes: &TerminalModes, modifier: Option<u8>) -> Vec<u8> {
    match (modes.disambiguate_esc_codes, modifier) {
        (true, _) => csi(&negotiated_parameters(modifier), final_byte),
        _ => csi("", final_byte),
    }
}

/// The `CSI <number> ~` number for each of F5..F12.
///
/// A table rather than `number + 10`, because the xterm numbering this encoding
/// comes from is not contiguous: 16 and 22 belong to no key, so F6 is 17 and F11
/// is 23. A `number + 10` would answer F6 with the bytes a child reads as
/// another key, which in a TUI means running that key's action for this one.
const FUNCTION_KEY_NUMBERS: [u8; 8] = [15, 17, 18, 19, 20, 21, 23, 24];

/// `CSI <number> ~` for Insert, Delete, PageUp, PageDown and F5-F12.
///
/// Only the negotiated form can carry a modifier. A child reading `CSI 5 ~` is
/// looking for exactly those four bytes and nothing after the digits, so a
/// modified key without the negotiation is sent as the unmodified one rather
/// than with a parameter appended to an encoding that has no room for it.
fn tilde_key(number: u8, modes: &TerminalModes, modifier: Option<u8>) -> Vec<u8> {
    let parameters = match (modes.disambiguate_esc_codes, modifier) {
        (true, Some(modifier)) => format!("{number};{modifier}"),
        _ => number.to_string(),
    };
    csi(&parameters, '~')
}

/// `CSI <parameters> <final_byte>`, the shape half of every sequence above.
fn csi(parameters: &str, final_byte: char) -> Vec<u8> {
    format!("\x1b[{parameters}{final_byte}").into_bytes()
}

fn arrow_final_byte(key: Key) -> char {
    match key {
        Key::Up => 'A',
        Key::Down => 'B',
        Key::Right => 'C',
        _ => 'D',
    }
}

/// The one byte a terminal has for "Ctrl was held with this key".
///
/// Uppercase folds down before the subtraction: Ctrl+Shift+C is the same key as
/// Ctrl+C, and a view reporting the shifted character has not asked for a
/// different one.
///
/// The six punctuation keys that name a C0 control in every terminal are listed
/// as well, because no letter can reach the four below ESC and a child binding
/// Ctrl+Space or Ctrl+`[` would otherwise never hear the press at all. They are
/// bytes rather than parameters under the keyboard protocol for the reason the
/// letters are: a control byte is one byte in every terminal.
///
/// `None` is still the answer for a character with no control byte, which is
/// what makes Ctrl plus an arbitrary character an unanswered press rather than a
/// guess.
fn control_byte(c: char) -> Option<u8> {
    let lower = c.to_ascii_lowercase();
    // `then` rather than `then_some`, which evaluates its argument either way:
    // `lower as u8` on a non-ASCII character is a truncated code point, and
    // subtracting 0x60 from that underflows rather than failing to be a letter.
    if lower.is_ascii_lowercase() {
        return Some(lower as u8 - 0x60);
    }
    match c {
        ' ' => Some(0x00),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        _ => None,
    }
}

/// The single character of a `KeyPress::text`, or nothing for anything longer.
fn only_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A press with nothing held. Most of what follows is about what one key
    /// sends on its own.
    fn press(key: Key) -> KeyPress {
        KeyPress {
            key,
            shift: false,
            ctrl: false,
            alt: false,
            meta: false,
            text: None,
        }
    }

    fn with_shift(mut press: KeyPress) -> KeyPress {
        press.shift = true;
        press
    }

    fn with_ctrl(mut press: KeyPress) -> KeyPress {
        press.ctrl = true;
        press
    }

    fn with_alt(mut press: KeyPress) -> KeyPress {
        press.alt = true;
        press
    }

    fn with_meta(mut press: KeyPress) -> KeyPress {
        press.meta = true;
        press
    }

    fn with_text(mut press: KeyPress, text: &str) -> KeyPress {
        press.text = Some(text.to_owned());
        press
    }

    /// What a child that negotiated nothing gets. Also the answer for a child
    /// that is asked questions and answers no: see
    /// [`a_program_that_only_queried_keyboard_support_did_not_agree_to_use_it`].
    fn unnegotiated() -> TerminalModes {
        TerminalModes::default()
    }

    /// The child that pushed `CSI > 1 u`.
    fn negotiated() -> TerminalModes {
        TerminalModes {
            disambiguate_esc_codes: true,
            ..TerminalModes::default()
        }
    }

    fn encoded(press: &KeyPress, modes: &TerminalModes) -> Vec<u8> {
        encode_key(press, modes).expect("a press with a terminal meaning")
    }

    #[test]
    fn shift_enter_sends_the_kitty_sequence_only_when_disambiguation_was_negotiated() {
        let plain_enter = press(Key::Enter);
        let shift_enter = with_shift(press(Key::Enter));

        assert_eq!(encoded(&plain_enter, &negotiated()), b"\r");
        assert_eq!(encoded(&shift_enter, &negotiated()), b"\x1b[13;2u");

        // The branch that matters for a coding agent that was handed a shell
        // which never negotiated: LF is the only byte that is not Enter, and it
        // is the whole answer -- asserted as an exact `Vec`, so a second
        // carriage return or a stray CSI would fail this rather than pass it.
        assert_eq!(encoded(&shift_enter, &unnegotiated()), b"\n");

        // Shift is a bit like the others rather than a fixed `2`, so each of
        // these adds it: Shift+Alt is 4, Ctrl+Shift is 6, and Alt+Ctrl+Shift
        // is 8. Answering 3 and 7 instead would name Alt+Enter and
        // Alt+Ctrl+Enter to the child, which is the one thing this whole case
        // exists to avoid.
        assert_eq!(
            encoded(&with_alt(with_shift(press(Key::Enter))), &negotiated()),
            b"\x1b[13;4u"
        );
        assert_eq!(
            encoded(&with_ctrl(with_shift(press(Key::Enter))), &negotiated()),
            b"\x1b[13;6u"
        );
        assert_eq!(
            encoded(
                &with_ctrl(with_alt(with_shift(press(Key::Enter)))),
                &negotiated()
            ),
            b"\x1b[13;8u"
        );
        assert_eq!(
            encoded(&with_alt(press(Key::Enter)), &negotiated()),
            b"\x1b[13;3u"
        );
        assert_eq!(
            encoded(&with_ctrl(press(Key::Enter)), &negotiated()),
            b"\x1b[13;5u"
        );

        // Enter still answers Enter when the child asked for line feeds instead
        // of carriage returns, because LNM is about the unmodified key.
        assert_eq!(
            encode_key(
                &plain_enter,
                &TerminalModes {
                    newline_is_return: true,
                    ..negotiated()
                }
            ),
            Some(b"\n".to_vec())
        );
    }

    #[test]
    fn a_program_that_only_queried_keyboard_support_did_not_agree_to_use_it() {
        // There is no "queried" flag, and that absence is the contract. What a
        // child gets is decided by the mode it *reported*, never by whether it
        // asked: a shell that answers `CSI ? u` with mode 0 and then declines
        // has to be indistinguishable, here, from a shell that was never asked,
        // or the CSI u forms go out into a child reading its newline as Enter.
        // `core::only_a_pushed_keyboard_mode_counts_as_negotiation` is the other
        // half: it drives a real parser through the query and asserts the
        // snapshot this module is handed still says `false`.
        let asked_and_declined = unnegotiated();

        assert!(!asked_and_declined.disambiguate_esc_codes);
        assert_eq!(
            encoded(&with_shift(press(Key::Enter)), &asked_and_declined),
            b"\n"
        );
        assert_eq!(
            encoded(
                &with_ctrl(with_shift(press(Key::Enter))),
                &asked_and_declined
            ),
            b"\n"
        );
        assert_eq!(encoded(&press(Key::Up), &asked_and_declined), b"\x1b[A");
        assert_eq!(
            encoded(&with_text(press(Key::Char('c')), "c"), &asked_and_declined),
            b"c"
        );
    }

    #[test]
    fn cursor_keys_follow_the_modes_the_child_set() {
        // Every cell asserted, because the two modes do not combine: the
        // negotiated form wins outright, and application cursor mode is what
        // the legacy answer is.
        for (key, csi_form, ss3_form) in [
            (Key::Up, b"\x1b[A".as_slice(), b"\x1bOA".as_slice()),
            (Key::Down, b"\x1b[B".as_slice(), b"\x1bOB".as_slice()),
            (Key::Right, b"\x1b[C".as_slice(), b"\x1bOC".as_slice()),
            (Key::Left, b"\x1b[D".as_slice(), b"\x1bOD".as_slice()),
        ] {
            let table = [
                (TerminalModes::default(), csi_form),
                (
                    TerminalModes {
                        app_cursor: true,
                        ..TerminalModes::default()
                    },
                    ss3_form,
                ),
                (
                    TerminalModes {
                        disambiguate_esc_codes: true,
                        ..TerminalModes::default()
                    },
                    csi_form,
                ),
                (
                    TerminalModes {
                        app_cursor: true,
                        disambiguate_esc_codes: true,
                        ..TerminalModes::default()
                    },
                    csi_form,
                ),
            ];

            for (modes, expected) in table {
                assert_eq!(
                    encode_key(&press(key), &modes),
                    Some(expected.to_vec()),
                    "{key:?} with {modes:?}"
                );
            }
        }

        // An unmodified arrow under the negotiation is `CSI A` still, not
        // `CSI 1 ; 1 A`: the parameter belongs to a modifier that is not there.
        assert_eq!(encoded(&press(Key::Up), &negotiated()), b"\x1b[A");
        assert_eq!(
            encoded(&with_ctrl(press(Key::Up)), &negotiated()),
            b"\x1b[1;5A"
        );
        assert_eq!(
            encoded(&with_ctrl(with_alt(press(Key::Right))), &negotiated()),
            b"\x1b[1;7C"
        );
        // A legacy child has no modified arrow at all, so it gets the
        // unmodified one rather than a parameter it will not read.
        assert_eq!(
            encoded(&with_ctrl(press(Key::Left)), &unnegotiated()),
            b"\x1b[D"
        );
    }

    #[test]
    fn ctrl_letters_become_control_bytes_and_meta_prefixes_with_escape() {
        for (letter, byte) in [('a', 0x01_u8), ('c', 0x03), ('z', 0x1a)] {
            assert_eq!(
                encode_key(&with_ctrl(press(Key::Char(letter))), &unnegotiated()),
                Some(vec![byte])
            );
        }

        // Ctrl+Shift+C is Ctrl+C: the shift is in the character the view
        // reported, not in a separate request for a different byte.
        assert_eq!(
            encode_key(
                &with_ctrl(with_shift(press(Key::Char('C')))),
                &unnegotiated()
            ),
            Some(vec![0x03])
        );

        // A control byte beats the text the press carries, because the text a
        // view reports alongside Ctrl is the letter it is the control *of*.
        assert_eq!(
            encode_key(
                &with_text(with_ctrl(press(Key::Char('c'))), "c"),
                &unnegotiated()
            ),
            Some(vec![0x03])
        );

        // A character with no control byte has none. Sending the character
        // instead would put a `€` in a Ctrl press, and answering with nothing at
        // all would look like a dropped key, so the caller is told instead.
        assert_eq!(
            encode_key(&with_ctrl(press(Key::Char('€'))), &unnegotiated()),
            None
        );
        // A key with an encoding of its own is not that case: Ctrl+Up is still
        // Up to a child that negotiated nothing, and has somewhere to go once
        // it did.
        assert_eq!(
            encoded(&with_ctrl(press(Key::Up)), &unnegotiated()),
            b"\x1b[A"
        );
        assert_eq!(
            encoded(&with_ctrl(press(Key::Up)), &negotiated()),
            b"\x1b[1;5A"
        );

        // Meta prefixes whatever the press had on its own, text and keys alike.
        assert_eq!(
            encoded(&with_meta(press(Key::Char('b'))), &unnegotiated()),
            b"\x1bb"
        );
        assert_eq!(
            encoded(&with_meta(press(Key::Up)), &unnegotiated()),
            b"\x1b\x1b[A"
        );
        assert_eq!(
            encoded(&with_meta(press(Key::Backspace)), &unnegotiated()),
            b"\x1b\x7f"
        );
        assert_eq!(
            encoded(&with_meta(with_shift(press(Key::Enter))), &negotiated()),
            b"\x1b\x1b[13;2u"
        );
        // ... and a press that had no meaning of its own still has none.
        assert_eq!(
            encode_key(
                &with_meta(with_ctrl(press(Key::Char('€')))),
                &unnegotiated()
            ),
            None
        );
    }

    #[test]
    fn ctrl_punctuation_answers_with_the_control_byte_every_terminal_gives_it() {
        // The six keys whose Ctrl combination names a C0 control, asserted as
        // the exact byte rather than as "something was sent": a child binding
        // Ctrl+Space reads NUL and a child binding Ctrl+`[` reads ESC, and the
        // four below ESC are the only bytes no letter can reach.
        for (punctuation, byte) in [
            (' ', 0x00_u8),
            ('[', 0x1b),
            ('\\', 0x1c),
            (']', 0x1d),
            ('^', 0x1e),
            ('_', 0x1f),
        ] {
            assert_eq!(
                encode_key(&with_ctrl(press(Key::Char(punctuation))), &unnegotiated()),
                Some(vec![byte]),
                "Ctrl+{punctuation:?}"
            );
        }

        // These follow the letters, so the negotiation does not change them
        // either: a control byte is one byte in every terminal.
        assert_eq!(
            encoded(&with_ctrl(press(Key::Char(' '))), &negotiated()),
            b"\x00"
        );
        // Meta is still a prefix in front of it, not a second ESC.
        assert_eq!(
            encoded(
                &with_meta(with_ctrl(press(Key::Char(' ')))),
                &unnegotiated()
            ),
            b"\x1b\x00"
        );

        // Punctuation outside that table still has no control byte, so the six
        // are the six rather than a rule about every symbol.
        assert_eq!(
            encode_key(&with_ctrl(press(Key::Char('-'))), &unnegotiated()),
            None
        );
    }

    #[test]
    fn committed_text_is_sent_whole_and_ctrl_only_applies_to_a_single_letter() {
        // An IME commit is several characters on one key event, and it is the
        // text rather than the `Key::Char` it arrived on that is the input.
        assert_eq!(
            encoded(
                &with_text(press(Key::Char('あ')), "こんにちは"),
                &unnegotiated()
            ),
            "こんにちは".as_bytes()
        );
        assert_eq!(encoded(&press(Key::Char('a')), &unnegotiated()), b"a");
        // Empty text is a view with nothing to report, so the character stands.
        assert_eq!(
            encoded(&with_text(press(Key::Char('a')), ""), &unnegotiated()),
            b"a"
        );

        // A commit longer than one letter under Ctrl has no control encoding in
        // any terminal, so it is refused rather than sent as if Ctrl were not
        // held.
        assert_eq!(
            encode_key(
                &with_text(with_ctrl(press(Key::Char('こ'))), "こ"),
                &unnegotiated()
            ),
            None
        );
    }

    #[test]
    fn tab_keeps_its_legacy_shift_form_and_the_negotiated_one_carries_the_modifier() {
        assert_eq!(encoded(&press(Key::Tab), &unnegotiated()), b"\t");
        assert_eq!(
            encoded(&with_shift(press(Key::Tab)), &unnegotiated()),
            b"\x1b[Z"
        );
        // Ctrl+Tab has no legacy form, so it collapses onto the unmodified one.
        assert_eq!(encoded(&with_ctrl(press(Key::Tab)), &unnegotiated()), b"\t");

        assert_eq!(encoded(&press(Key::Tab), &negotiated()), b"\t");
        assert_eq!(
            encoded(&with_shift(press(Key::Tab)), &negotiated()),
            b"\x1b[9;2u"
        );
        assert_eq!(
            encoded(&with_ctrl(press(Key::Tab)), &negotiated()),
            b"\x1b[9;5u"
        );
    }

    #[test]
    fn home_and_end_gain_a_modifier_only_under_the_negotiation() {
        for (key, legacy, negotiated_modifiers) in [
            (Key::Home, b"\x1b[H".as_slice(), b"\x1b[1;4H".as_slice()),
            (Key::End, b"\x1b[F".as_slice(), b"\x1b[1;4F".as_slice()),
        ] {
            assert_eq!(encoded(&press(key), &unnegotiated()), legacy);
            assert_eq!(encoded(&press(key), &negotiated()), legacy);
            assert_eq!(
                encoded(&with_alt(with_shift(press(key))), &negotiated()),
                negotiated_modifiers
            );
            // The legacy pair is two fixed bytes in every terminal that has it.
            assert_eq!(
                encoded(&with_alt(with_shift(press(key))), &unnegotiated()),
                legacy
            );
        }

        assert_eq!(encoded(&press(Key::PageUp), &unnegotiated()), b"\x1b[5~");
        assert_eq!(encoded(&press(Key::PageDown), &unnegotiated()), b"\x1b[6~");
        assert_eq!(encoded(&press(Key::Insert), &unnegotiated()), b"\x1b[2~");
        assert_eq!(encoded(&press(Key::Delete), &unnegotiated()), b"\x1b[3~");
        assert_eq!(
            encoded(&with_ctrl(press(Key::Delete)), &negotiated()),
            b"\x1b[3;5~"
        );
    }

    #[test]
    fn function_keys_pick_between_three_encodings_and_refuse_a_thirteenth() {
        let app_keypad = TerminalModes {
            app_keypad: true,
            ..TerminalModes::default()
        };

        for (number, legacy, ss3, disambiguated) in [
            (
                1_u8,
                b"\x1b[11~".as_slice(),
                b"\x1bOP".as_slice(),
                b"\x1b[1;2P".as_slice(),
            ),
            (2, b"\x1b[12~", b"\x1bOQ", b"\x1b[1;2Q"),
            (3, b"\x1b[13~", b"\x1bOR", b"\x1b[1;2R"),
            (4, b"\x1b[14~", b"\x1bOS", b"\x1b[1;2S"),
        ] {
            assert_eq!(encoded(&press(Key::F(number)), &unnegotiated()), legacy);
            assert_eq!(encoded(&press(Key::F(number)), &app_keypad), ss3);
            // Negotiated, the four answer as `CSI 1 ; modifier P`.. whatever the
            // keypad mode says, and the final byte is the one F1 already had.
            assert_eq!(encoded(&press(Key::F(number)), &negotiated()), legacy);
            assert_eq!(
                encoded(&with_shift(press(Key::F(number))), &negotiated()),
                disambiguated
            );
            assert_eq!(
                encoded(&with_shift(press(Key::F(number))), &app_keypad),
                ss3
            );
        }

        // F5 and up have one encoding and application keypad does not touch it.
        // Every key's own number is spelled out rather than derived, because the
        // xterm numbering skips 16 and 22 and a test that computed the
        // expectation would agree with any table.
        for (number, legacy, disambiguated) in [
            (5_u8, b"\x1b[15~".as_slice(), b"\x1b[15;2~".as_slice()),
            (6, b"\x1b[17~", b"\x1b[17;2~"),
            (7, b"\x1b[18~", b"\x1b[18;2~"),
            (8, b"\x1b[19~", b"\x1b[19;2~"),
            (9, b"\x1b[20~", b"\x1b[20;2~"),
            (10, b"\x1b[21~", b"\x1b[21;2~"),
            (11, b"\x1b[23~", b"\x1b[23;2~"),
            (12, b"\x1b[24~", b"\x1b[24;2~"),
        ] {
            assert_eq!(encoded(&press(Key::F(number)), &unnegotiated()), legacy);
            assert_eq!(encoded(&press(Key::F(number)), &app_keypad), legacy);
            // Unmodified is unmodified under the negotiation too: the parameter
            // belongs to a modifier that is not there.
            assert_eq!(encoded(&press(Key::F(number)), &negotiated()), legacy);
            assert_eq!(
                encoded(&with_shift(press(Key::F(number))), &negotiated()),
                disambiguated
            );
            // Without it there is nowhere to put a modifier, so a modified key
            // is the unmodified one.
            assert_eq!(
                encoded(&with_shift(press(Key::F(number))), &unnegotiated()),
                legacy
            );
        }
        assert_eq!(
            encoded(&with_ctrl(press(Key::F(12))), &negotiated()),
            b"\x1b[24;5~"
        );

        // A key no terminal can be sent is refused, not rounded to a neighbour.
        for number in [0_u8, 13, 255] {
            assert_eq!(
                encode_key(&press(Key::F(number)), &negotiated()),
                None,
                "F{number}"
            );
        }
    }

    #[test]
    fn the_two_single_byte_keys_are_the_two_single_bytes() {
        assert_eq!(encoded(&press(Key::Backspace), &negotiated()), b"\x7f");
        assert_eq!(encoded(&press(Key::Escape), &negotiated()), b"\x1b");
        // Their encoding has no parameter to put a modifier in, so a modified
        // one is the unmodified one rather than a shape that does not exist.
        assert_eq!(
            encoded(&with_ctrl(press(Key::Backspace)), &negotiated()),
            b"\x7f"
        );
    }
}
