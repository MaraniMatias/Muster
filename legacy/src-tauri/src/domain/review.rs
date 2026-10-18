use serde::Serialize;

pub const MAX_NOTE_CONTENT_BYTES: usize = 8 * 1024;
pub const MAX_NOTE_CODE_BYTES: usize = 8 * 1024;
/// A round ships at most this many notes, and the prompt that carries them.
pub const MAX_ROUND_NOTES: usize = 1000;
pub const MAX_ROUND_PROMPT_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewNote {
    pub id: String,
    pub checkout_id: String,
    pub path: String,
    /// "old" for deleted lines, "new" for added and context lines.
    pub side: String,
    pub line_start: i64,
    pub line_end: Option<i64>,
    pub content: String,
    pub code: String,
    /// "draft", "sent" or "resolved".
    pub status: String,
    /// Fingerprint of the anchored line, captured with the note. See [`review_anchor_hash`].
    pub code_hash: String,
    /// Sticky: set when the anchored line drifts, cleared only by the user.
    pub outdated: bool,
    /// The round that last carried this note, if any.
    pub round_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// One batch of notes delivered to an agent as a single message.
///
/// The round exists so a send is recorded before it is attempted: `dispatching` with the
/// marker already stored is what makes a reconnect safe, because the marker can be looked
/// for in the session's messages to learn whether the send actually landed.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRound {
    pub id: String,
    pub checkout_id: String,
    /// The OpenCode session this round targets; retained after relocation for history.
    pub session_id: String,
    /// "queued", "dispatching", "dispatched", "acked" or "relocated".
    /// A relocated round is historical and is never retried against a different session.
    pub status: String,
    /// Embedded in the prompt so a reconnect can recognize this exact send.
    pub marker: String,
    pub note_ids: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl ReviewRound {
    pub fn is_pending(&self) -> bool {
        self.status == "dispatching" || self.status == "queued"
    }
}

/// The prefix of the marker embedded in every review prompt.
pub const ROUND_MARKER_PREFIX: &str = "muster-review";

/// The fixed instructions every review message starts with.
pub const AGENT_PROMPT_PREFIX: &str = "You are receiving a code review of this checkout, over the current working tree.\nFix directly what the comments make clear;\nask before touching anything you are unsure about.\n";

/// Builds the marker a round is recognized by. Short, opaque, and unique per round.
pub fn review_round_marker(round_id: &str) -> String {
    format!("{ROUND_MARKER_PREFIX}:{round_id}")
}

/// Whether a string is a marker this build writes, and so may be taken for a round's own.
///
/// The grammar is the one `record_round` generates and nothing looser: a round id is
/// `round:` and three numbers. A receipt is evidence only because this app put the string
/// in the message, so a marker-shaped run of characters typed into a session is not one of ours
/// and must not confirm anything.
pub fn is_review_round_marker(value: &str) -> bool {
    let Some(id) = value.strip_prefix(&format!("{ROUND_MARKER_PREFIX}:")) else {
        return false;
    };
    // Exactly `round:` and three numbers, which is what `record_round` writes. A count of "at
    // least" would accept a marker-shaped string that is not one of ours, which is the whole
    // failure this refuses.
    let is_number = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    let mut parts = id.split(':');
    let shape = parts.next() == Some("round")
        && parts.next().is_some_and(is_number)
        && parts.next().is_some_and(is_number)
        && parts.next().is_some_and(is_number);
    shape && parts.next().is_none()
}

/// Pulls a round's marker out of a delivered message.
///
/// Only the bracketed token `build_round_prompt` embeds is matched, so a marker that appears
/// anywhere else in a message — quoted, spelled out in prose, echoed by the agent — is not a
/// receipt. This is what lets an agent's own transcript be read for the marker without a
/// search of the raw text ever standing in for one.
pub fn review_round_marker_in(message: &str) -> Option<&str> {
    let mut rest = message;
    while let Some(at) = rest.find('[') {
        rest = &rest[at + 1..];
        let end = rest.find(']')?;
        let candidate = &rest[..end];
        if is_review_round_marker(candidate) {
            return Some(candidate);
        }
        rest = &rest[end + 1..];
    }
    None
}

/// Fingerprint of a note's anchor line, used to detect that the commented line changed.
///
/// Only the first line of the captured code is hashed. A multi-line note whose block was
/// only partially loaded must never look drifted, and the anchored line is what decides
/// whether the comment still points at the code it was written about. FNV-1a is enough:
/// this detects accidental drift, it is not a security boundary.
pub fn review_anchor_hash(code: &str) -> String {
    let anchor = code.split('\n').next().unwrap_or_default();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in anchor.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::{review_anchor_hash, review_round_marker, review_round_marker_in};

    #[test]
    fn anchor_hash_is_stable_and_separates_different_lines() {
        assert_eq!(
            review_anchor_hash("const a = 1;"),
            review_anchor_hash("const a = 1;")
        );
        assert_ne!(
            review_anchor_hash("const a = 1;"),
            review_anchor_hash("const a = 2;")
        );
        assert_ne!(review_anchor_hash("const a = 1;"), "");
    }

    #[test]
    fn anchor_hash_ignores_everything_after_the_anchored_line() {
        // A range whose later lines were only partly loaded must not look drifted.
        assert_eq!(
            review_anchor_hash("first line\nsecond line"),
            review_anchor_hash("first line")
        );
    }

    /// Only what this build writes is a marker. Everything else is a string somebody typed,
    /// and a receipt built from one would confirm a round nobody sent.
    #[test]
    fn only_a_generated_marker_is_read_as_one() {
        let marker = review_round_marker("round:1762000000:4242:7");
        assert!(super::is_review_round_marker(&marker));
        for impostor in [
            "muster-review:round:1:2",
            "muster-review:round:1:2:3:4",
            "muster-review:round:1:2:x",
            "muster-review::1:2:3",
            "muster-review",
            "please review muster-review:round:1:2:3 for me",
            "",
        ] {
            assert!(!super::is_review_round_marker(impostor), "{impostor}");
        }
    }

    /// A receipt is the bracketed token the prompt carries and nothing else, so an agent
    /// echoing the marker, or a person quoting it, cannot stand in for having received it.
    #[test]
    fn a_receipt_is_the_bracketed_token_and_not_anything_around_it() {
        let marker = review_round_marker("round:1762000000:4242:7");
        assert_eq!(
            review_round_marker_in(&format!("instructions\n[{marker}]\n\n## src/foo.js")),
            Some(marker.as_str())
        );
        assert_eq!(
            review_round_marker_in(&format!("[{marker}]")),
            Some(marker.as_str())
        );
        assert_eq!(review_round_marker_in("no marker here"), None);
        assert_eq!(
            review_round_marker_in(&format!("the agent echoed {marker} back at me")),
            None,
            "an unbracketed marker is prose, not the token this app wrote"
        );
        assert_eq!(
            review_round_marker_in("muster-review:round:1:2:3] then [muster-review:round:1:2:4"),
            None
        );
    }
}
