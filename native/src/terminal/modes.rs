//! The parser's terminal modes, as the input path needs to see them.
//!
//! `TerminalCore` already owns the `Term` that holds every one of these bits,
//! and re-deriving them here would mean parsing the child's escape sequences a
//! second time -- the one thing that has to happen exactly once, because two
//! parsers disagree about what the child asked for at precisely the moment that
//! matters. So this is a snapshot: filled in from the parser's `TermMode` when
//! the window asks for it, and read by everything that decides which bytes a key
//! press becomes.
//!
//! Only the modes that change what is *sent* are here. A mouse-reporting mode
//! changes what the window does with a click, not what a keystroke is, and a
//! mode nothing on the input path reads is one this crate would have to keep
//! honest for no benefit.
//!
//! No alacritty type crosses this boundary for the same reason
//! [`CellSnapshot`](super::CellSnapshot) exists: the parser's names resolve
//! against the parser's world, and this half has to stay usable without it.

/// Six bools rather than a word of bits, because each one is a question this
/// crate asks of a child -- and a child that has never been asked anything is
/// `false` in all six.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalModes {
    /// `DECCKM`: arrows are `SS3 A`.. rather than `CSI A`..
    pub app_cursor: bool,
    /// `DECKPAM`: F1-F4 are `SS3 P`.. rather than `CSI 11 ~`..
    pub app_keypad: bool,
    /// `DECSET 2004`: the child asked to be told a paste is a paste.
    pub bracketed_paste: bool,
    /// `CSI > 1 u` is on the parser's keyboard-mode stack, so the child may be
    /// sent the disambiguating `CSI ... u` forms.
    ///
    /// The stack is what makes this an agreement rather than a capability: the
    /// child pushed the mode, it did not merely ask whether this terminal knows
    /// it. Nothing in this crate records the asking.
    pub disambiguate_esc_codes: bool,
    /// `CSI > 2 u`, the other half of the same negotiation.
    ///
    /// Carried because a mode snapshot is a picture of the child's stack rather
    /// than of the modes this crate happens to use, and nothing on the input
    /// path reads it: the event type it introduces says what *kind* of press
    /// happened, and a view that could say so has not asked for it.
    pub report_event_types: bool,
    /// `LNM`, `TermMode::LINE_FEED_NEW_LINE`: Enter is a line feed where it
    /// would otherwise be a carriage return.
    pub newline_is_return: bool,
}
