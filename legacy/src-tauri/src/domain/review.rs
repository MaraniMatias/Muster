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
    /// The OpenCode session this round targeted; retained after relocation for history.
    pub session_id: Option<String>,
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
    use super::review_anchor_hash;

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
}
