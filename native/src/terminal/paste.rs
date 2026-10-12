//! Turning a paste into the bytes a terminal child expects to read.
//!
//! The whole of a paste is the negotiation: a child that asked for bracketed
//! paste has to be *told* where the paste begins, or it runs the pasted text as
//! commands, and a child that did not ask must not be sent the markers, because
//! `\x1b[200~` in a shell that never heard of it is six visible characters at
//! the start of its input line. There is no middle answer, which is why the
//! decision is read from the child's reported mode rather than from a setting.

use super::modes::TerminalModes;

/// Longest paste accepted, in bytes of the pasted text.
///
/// A paste is not a keystroke: it is the one input path that can deliver a
/// megabyte from the clipboard in a single press, and the one where a bug in
/// the caller that produced it turns into a megabyte at the PTY. 1 MiB is far
/// past any real paste and the same bound the backend puts on one write.
pub const MAX_PASTE_BYTES: usize = 1024 * 1024;

/// `CSI 200 ~`, `CSI 201 ~`.
const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

/// Why a paste was refused, with both numbers.
///
/// The attempted size is reported because the useful message is "this paste was
/// 3.1 MB and the limit is 1 MB", and a caller that cannot say that is showing
/// the same error for a paste one byte over the line and for a clipboard that
/// holds a video file's worth of text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteError {
    TooLarge { bytes: usize, limit: usize },
}

/// Encode a paste for `modes`, or refuse it.
///
/// The size is checked against the pasted text before anything is normalised or
/// wrapped, because that is the number the caller has to report and the one
/// they already hold; the two markers and the CRLF folding can only make the
/// result smaller.
pub fn encode_paste(text: &str, modes: &TerminalModes) -> Result<Vec<u8>, PasteError> {
    if text.len() > MAX_PASTE_BYTES {
        return Err(PasteError::TooLarge {
            bytes: text.len(),
            limit: MAX_PASTE_BYTES,
        });
    }

    // A clipboard on macOS or Windows carries CRLF, and a terminal in raw mode
    // reads CR as a byte in its own right rather than as half of a line ending.
    // Every terminal emulator folds it on paste, and a child that wanted the
    // carriage return never asked for it -- it was in the clipboard, not in the
    // keypress.
    let normalised = text.replace("\r\n", "\n").replace('\r', "\n");

    let bracketed = modes.bracketed_paste;
    let mut bytes = Vec::with_capacity(normalised.len() + 2 * BRACKETED_PASTE_START.len());
    if bracketed {
        bytes.extend_from_slice(BRACKETED_PASTE_START);
    }
    bytes.extend_from_slice(normalised.as_bytes());
    if bracketed {
        bytes.extend_from_slice(BRACKETED_PASTE_END);
    }

    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> TerminalModes {
        TerminalModes::default()
    }

    fn bracketed() -> TerminalModes {
        TerminalModes {
            bracketed_paste: true,
            ..TerminalModes::default()
        }
    }

    #[test]
    fn a_paste_is_wrapped_only_when_the_child_asked() {
        let text = "git status\n";

        assert_eq!(
            encode_paste(text, &bracketed()).expect("a paste under the limit"),
            b"\x1b[200~git status\n\x1b[201~"
        );

        // Markers sent to a child that never asked are six and six characters
        // of escape sequence sitting in its input line, which is exactly what
        // the markers exist to prevent in the other direction.
        assert_eq!(
            encode_paste(text, &plain()).expect("a paste under the limit"),
            b"git status\n"
        );

        // Bracketing is the child's mode and nothing else: the two modes agree
        // on the payload, so a paste that crosses a `DECSET 2004` mid-flight is
        // only ever the payload that changes shape.
        let wrapping = TerminalModes {
            bracketed_paste: true,
            ..bracketed()
        };
        assert_eq!(
            encode_paste(text, &wrapping).expect("a paste under the limit"),
            b"\x1b[200~git status\n\x1b[201~"
        );
    }

    #[test]
    fn carriage_returns_are_folded_to_line_feeds_before_either_branch() {
        // Both shapes a clipboard can arrive in, and a lone CR that no editor
        // produces on its own but a terminal, a paste from a pipe and some
        // remote clipboard tools all do.
        for (input, expected) in [("a\r\nb", "a\nb"), ("a\rb", "a\nb"), ("a\nb", "a\nb")] {
            assert_eq!(
                encode_paste(input, &bracketed()).expect("a paste under the limit"),
                format!("\x1b[200~{expected}\x1b[201~").into_bytes(),
                "{input:?}"
            );
            assert_eq!(
                encode_paste(input, &plain()).expect("a paste under the limit"),
                expected.as_bytes(),
                "{input:?}"
            );
        }

        // The two-character sequence is one line ending, not two: folding `\r`
        // on its own first would turn a CR into a LF and leave the LF that
        // followed it as a blank line of its own.
        assert_eq!(
            encode_paste("a\r\nb", &plain()).expect("a paste under the limit"),
            b"a\nb"
        );
    }

    #[test]
    fn an_oversized_paste_is_refused_rather_than_truncated() {
        // Built to exceed the limit by one byte, so the refusal below cannot
        // pass against a limit that is merely close.
        let oversized = "x".repeat(MAX_PASTE_BYTES + 1);

        assert_eq!(
            encode_paste(&oversized, &plain()),
            Err(PasteError::TooLarge {
                bytes: MAX_PASTE_BYTES + 1,
                limit: MAX_PASTE_BYTES
            })
        );
        assert_eq!(
            encode_paste(&oversized, &bracketed()),
            Err(PasteError::TooLarge {
                bytes: MAX_PASTE_BYTES + 1,
                limit: MAX_PASTE_BYTES
            })
        );

        // Exactly at the limit is not over it. Without this the refusal above
        // would also pass against a limit one byte too tight, and a paste of
        // precisely the documented size would be the thing nobody tested.
        assert_eq!(
            encode_paste(&"x".repeat(MAX_PASTE_BYTES), &plain()),
            Ok(vec![b'x'; MAX_PASTE_BYTES])
        );

        // Nothing comes back that a caller could mistake for the payload. Half
        // a paste is worse than none, because it looks like the paste worked.
        assert!(encode_paste(&oversized, &plain()).is_err());
    }

    #[test]
    fn a_multibyte_paste_is_measured_in_bytes() {
        // Four bytes each, so a limit counted in characters would let this
        // through at a quarter of the size it really is.
        let emoji = "😀";
        assert_eq!(emoji.len(), 4);

        let under = emoji.repeat(MAX_PASTE_BYTES / 4);
        assert_eq!(under.len(), MAX_PASTE_BYTES);
        assert!(encode_paste(&under, &plain()).is_ok());

        // One character past the limit is still over it.
        assert_eq!(
            encode_paste(&format!("{under}{emoji}"), &plain()),
            Err(PasteError::TooLarge {
                bytes: MAX_PASTE_BYTES + 4,
                limit: MAX_PASTE_BYTES
            })
        );
    }

    #[test]
    fn an_empty_paste_encodes_to_nothing_rather_than_to_just_the_markers() {
        // There is nothing to tell the child about, and a child in bracketed
        // paste mode treats the two markers as an empty paste it should run.
        assert_eq!(
            encode_paste("", &bracketed()),
            Ok(b"\x1b[200~\x1b[201~".to_vec())
        );
        assert_eq!(encode_paste("", &plain()), Ok(Vec::new()));
    }
}
