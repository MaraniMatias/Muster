//! Review notes anchored to source lines in a diff.

use std::{
    collections::BTreeMap,
    path::{Component, Path},
};

use serde::{Deserialize, Serialize};

pub const MAX_NOTE_CONTENT_BYTES: usize = 8 * 1024;
pub const MAX_NOTE_CODE_BYTES: usize = 8 * 1024;
pub const MAX_REVIEW_NOTES: usize = 1000;
/// A review round ships at most this many notes, and the prompt that carries them.
pub const MAX_ROUND_NOTES: usize = 1000;
/// A round goes out as one message, so it cannot be larger than one message.
pub const MAX_ROUND_PROMPT_BYTES: usize = 512 * 1024;
/// The prefix of the marker embedded in every review prompt.
pub const ROUND_MARKER_PREFIX: &str = "muster-review";
/// The fixed instructions every review message starts with.
pub const AGENT_PROMPT_PREFIX: &str = "You are receiving a code review of this checkout, over the current working tree.\nFix directly what the comments make clear;\nask before touching anything you are unsure about.\n";

/// Builds the marker a round is recognized by. Short, opaque, and unique per round.
///
/// It is the deduplication key: it is embedded in the prompt *before* the send is
/// attempted, so a send whose outcome is unknown can be looked for in the session's
/// messages instead of guessed at.
pub fn review_round_marker(round_id: &str) -> String {
    format!("{ROUND_MARKER_PREFIX}:{round_id}")
}

/// Builds the prompt for a round: the fixed instructions, the round marker, and the notes.
///
/// `marker` already carries the `muster-review:` prefix, so it is embedded verbatim:
/// the deduplication key must be exactly what the session will hold.
pub fn build_round_prompt(review_markdown: &str, marker: &str) -> String {
    format!("{AGENT_PROMPT_PREFIX}\n[{marker}]\n{review_markdown}")
}

/// Rejects a prompt that cannot be delivered as one message.
pub fn check_round_prompt(prompt: &str) -> Result<(), String> {
    if prompt.trim().is_empty() {
        return Err("a review round cannot be empty".into());
    }
    if prompt.len() > MAX_ROUND_PROMPT_BYTES {
        return Err("the review is too large to send to the agent in one message".into());
    }
    Ok(())
}

/// Where exported review markdown is saved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStorage {
    /// `~/.muster/tmp/reviews`.
    Default,
    /// `.muster/reviews` inside the checkout.
    #[default]
    Workdir,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSide {
    Old,
    New,
}

impl ReviewSide {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Old => "old",
            Self::New => "new",
        }
    }
}

/// The local date and minute used together for a review's heading and filename.
pub fn local_review_timestamp() -> Result<(String, String), String> {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    if now == -1 {
        return Err("could not read the local clock".into());
    }
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&now, &mut local) }.is_null() {
        return Err("could not format the local clock".into());
    }
    let date = format!(
        "{:04}-{:02}-{:02}",
        local.tm_year + 1900,
        local.tm_mon + 1,
        local.tm_mday
    );
    let timestamp = format!("{date}-{:02}{:02}", local.tm_hour, local.tm_min);
    Ok((date, timestamp))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub text: String,
    pub mark: char,
}

/// A note as it is persisted: the exported shape, so a review that has been sent can
/// still be shown and exported again without re-reading the diff.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewNote {
    pub id: String,
    pub checkout_id: String,
    pub path: String,
    pub side: ReviewSide,
    pub line_start: usize,
    pub line_end: Option<usize>,
    pub content: String,
    /// Captured from the diff when the note was created; exports never re-read it.
    pub code: String,
    pub code_hash: String,
    /// Sticky: drift sets it; only an explicit user action clears it.
    pub outdated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnchorOutcome {
    Unchanged,
    Changed,
    Missing,
    Unknown,
}

/// The line's anchor hash is deliberately not a security primitive; it detects drift.
pub fn review_anchor_hash(code: &str) -> String {
    digest_text(code.split('\n').next().unwrap_or_default())
}

/// Every byte of `text`, as one digest.
///
/// FNV-1a over the whole string rather than its first line. It answers whether a line
/// has drifted; it is not used for persistent review deduplication.
pub fn digest_text(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// A note as one canonical string: what a round that carried it claimed about it.
///
/// Only what would be **delivered** is in it — the id, the checkout, the file, the side,
/// the line range, the comment and the code it was captured with. Drift bookkeeping is
/// deliberately left out: `outdated` flips because the working tree moved and
/// `code_hash` is the anchor's own fingerprint, so carrying either would report a comment
/// that was already delivered as a new one and put it on the wire a second time.
///
/// Each field writes its own byte length first, so a comment that contains the separator
/// cannot be spelled the same as a path that ends with it, and what comes back is the
/// note itself rather than a digest of it: two notes are the same note exactly when their
/// canonical strings are equal.
pub fn review_note_snapshot(note: &ReviewNote) -> String {
    let line_start = note.line_start.to_string();
    let line_end = note
        .line_end
        .map(|line| line.to_string())
        .unwrap_or_default();
    let mut snapshot = String::new();
    for field in [
        note.id.as_str(),
        note.checkout_id.as_str(),
        note.path.as_str(),
        note.side.as_str(),
        line_start.as_str(),
        line_end.as_str(),
        note.content.as_str(),
        note.code.as_str(),
    ] {
        snapshot.push_str(&field.len().to_string());
        snapshot.push(':');
        snapshot.push_str(field);
        snapshot.push('\n');
    }
    snapshot
}

/// What a round that claimed `note` keeps about it.
///
/// The note's id and a digest of the canonical string above, which is equality
/// compressed: what a duplicate send is decided against is whether two notes are the
/// same note, and a key carries that without carrying the comment and the captured
/// source the note is made of. That is the whole difference between a store that can
/// give bytes back and one that cannot: the canonical string is unbounded per note, so
/// keeping it per round is keeping the reviews themselves forever.
///
/// The canonical snapshot is hashed with SHA-256: unlike a short drift fingerprint,
/// this key is used to decide whether a persistent user review may be sent again.
pub fn review_note_key(note: &ReviewNote) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(review_note_snapshot(note).as_bytes());
    format!("{}:{digest:x}", note.id)
}

/// Refuses a note that could not be delivered as a comment about a line.
///
/// Asked where a note becomes durable and where the review goes on the wire, so a note
/// assembled from a stale or forged source cannot become part of a round: an empty
/// comment, a file that is not inside the checkout, a range no diff line has, or a body
/// too big to send as one message.
pub fn check_review_note(note: &ReviewNote) -> Result<(), String> {
    // The id is one line of one field in the store: a round's claim is its ids beside
    // their digests, one per line, and an id with a line break in it would split a key
    // in half and leave a delivered note looking unsent.
    if note.id.trim().is_empty() || note.id.len() > 256 || note.id.chars().any(char::is_control) {
        return Err("a review note needs an identifier".into());
    }
    if note.checkout_id.is_empty() {
        return Err("a review note needs the checkout it belongs to".into());
    }
    if note.path.is_empty()
        || note.path.len() > 4096
        || Path::new(&note.path)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("a review note must name a file inside the checkout".into());
    }
    if note.content.trim().is_empty() {
        return Err("a review note cannot be empty".into());
    }
    if note.content.len() > MAX_NOTE_CONTENT_BYTES {
        return Err("a review note cannot be longer than 8 KiB".into());
    }
    if note.code.len() > MAX_NOTE_CODE_BYTES {
        return Err("a review note cannot anchor more than 8 KiB of code".into());
    }
    if note.line_start == 0 || note.line_end.is_some_and(|end| end < note.line_start) {
        return Err("a review note must sit on a line the diff has".into());
    }
    Ok(())
}

/// Lines from the unified patch, keyed by `(side, one-based source line)`.
pub fn read_diff_lines(patch: &str) -> BTreeMap<(ReviewSide, usize), DiffLine> {
    let mut lines = BTreeMap::new();
    let (mut old, mut new) = (0, 0);
    let mut in_hunk = false;
    for line in patch.lines() {
        if let Some((old_start, new_start)) = hunk_starts(line) {
            old = old_start;
            new = new_start;
            in_hunk = true;
            continue;
        }
        if !in_hunk {
            continue;
        }
        if let Some(text) = line.strip_prefix('+') {
            lines.insert(
                (ReviewSide::New, new),
                DiffLine {
                    text: text.into(),
                    mark: '+',
                },
            );
            new += 1;
        } else if let Some(text) = line.strip_prefix('-') {
            lines.insert(
                (ReviewSide::Old, old),
                DiffLine {
                    text: text.into(),
                    mark: '-',
                },
            );
            old += 1;
        } else if let Some(text) = line.strip_prefix(' ') {
            let entry = DiffLine {
                text: text.into(),
                mark: ' ',
            };
            lines.insert((ReviewSide::Old, old), entry.clone());
            lines.insert((ReviewSide::New, new), entry);
            old += 1;
            new += 1;
        }
    }
    lines
}

fn hunk_starts(line: &str) -> Option<(usize, usize)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let number = |range: &str| range.split(',').next()?.parse().ok();
    Some((number(old)?, number(new)?))
}

/// Captures a complete range, refusing to create a note from partially loaded lines.
pub fn review_range_code(
    lines: &BTreeMap<(ReviewSide, usize), DiffLine>,
    side: ReviewSide,
    start: usize,
    end: usize,
) -> Option<String> {
    if start == 0 || end < start {
        return None;
    }
    (start..=end)
        .map(|line| lines.get(&(side, line)).map(|entry| entry.text.as_str()))
        .collect::<Option<Vec<_>>>()
        .map(|lines| lines.join("\n"))
}

/// Compares the captured anchor with the currently loaded diff.
pub fn anchor_outcome(
    note: &ReviewNote,
    current: Option<&DiffLine>,
    whole_diff_loaded: bool,
) -> AnchorOutcome {
    anchor_outcome_hash(
        note,
        current
            .map(|line| review_anchor_hash(&line.text))
            .as_deref(),
        whole_diff_loaded,
    )
}

/// Compares a captured anchor with its cached current-line fingerprint.
pub fn anchor_outcome_hash(
    note: &ReviewNote,
    current_hash: Option<&str>,
    whole_diff_loaded: bool,
) -> AnchorOutcome {
    let Some(current_hash) = current_hash else {
        return if whole_diff_loaded {
            AnchorOutcome::Missing
        } else {
            AnchorOutcome::Unknown
        };
    };
    if note.outdated || current_hash != note.code_hash {
        AnchorOutcome::Changed
    } else {
        AnchorOutcome::Unchanged
    }
}

#[derive(Clone, Debug)]
pub struct ReviewContext<'a> {
    pub branch: Option<&'a str>,
    pub default_branch: Option<&'a str>,
    pub date: &'a str,
    /// The host's existing filename-to-language resolver.
    pub language: fn(&str) -> &'static str,
}

impl Default for ReviewContext<'_> {
    fn default() -> Self {
        Self {
            branch: None,
            default_branch: None,
            date: "",
            language: |_| "",
        }
    }
}

/// Markdown in the default CodeReview.nvim format, prefixed with the ownership marker.
pub fn build_review_markdown(notes: &[ReviewNote], context: &ReviewContext<'_>) -> String {
    if notes.is_empty() {
        return String::new();
    }
    let mut paths = notes
        .iter()
        .map(|note| note.path.as_str())
        .collect::<Vec<_>>();
    paths.sort_unstable();
    paths.dedup();
    let range = [context.default_branch, context.branch]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("..");
    let range = if range.is_empty() {
        "working tree"
    } else {
        &range
    };
    let mut sections = Vec::new();
    for path in &paths {
        let mut at_path = notes
            .iter()
            .filter(|note| note.path == *path)
            .collect::<Vec<_>>();
        at_path.sort_by_key(|note| note.line_start);
        let blocks = at_path
            .into_iter()
            .map(|note| {
                // The lines the note covers, in the fence's own words: one
                // number for a line, the two ends for a range, and a minus on
                // every one of them when the change removed those lines. The
                // sign is the only thing in the document that says which side
                // of the change the comment is about.
                let sign = match note.side {
                    ReviewSide::Old => "-",
                    ReviewSide::New => "",
                };
                let lines = match note.line_end.filter(|end| *end != note.line_start) {
                    Some(end) => format!("{sign}{},{sign}{end}", note.line_start),
                    None => format!("{sign}{}", note.line_start),
                };
                let language = (context.language)(path);
                let code = if note.code.is_empty() {
                    String::new()
                } else {
                    let fence = code_fence(&note.code);
                    format!("{fence}{language}{{{lines}}}\n{}\n{fence}\n", note.code)
                };
                let quote = note
                    .content
                    .trim()
                    .lines()
                    .map(|line| format!("> {}", line.trim_end()))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("{code}\n{quote}")
            })
            .collect::<Vec<_>>();
        sections.push(format!("## {path}\n\n{}", blocks.join("\n\n---\n\n")));
    }
    format!(
        "<!-- muster-review-export -->\n# Code Review {}\n\n> `{range}` — {} {}, {} {}\n\n{}\n",
        context.date,
        paths.len(),
        if paths.len() == 1 { "file" } else { "files" },
        notes.len(),
        if notes.len() == 1 { "note" } else { "notes" },
        sections.join("\n\n"),
    )
}

/// The backtick run that fences `code`.
///
/// A closing fence only has to be at least as long as the opening one, so a
/// three-backtick fence is closed by the first three backticks inside the
/// captured source -- a reviewed Markdown file, a generated string, anything
/// that quotes code -- and everything after it renders as the document rather
/// than as the code that was reviewed. The fence is therefore always longer
/// than the longest run the code holds, and never shorter than three.
///
/// The language and the range notation stay on the opening line: an info string
/// that carried a backtick would not open a fence at all, and the language comes
/// from the host's own filename resolver.
fn code_fence(code: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for character in code.chars() {
        if character == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat(longest.max(2) + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_lines_keep_both_sides_and_capture_ranges() {
        let lines =
            read_diff_lines("diff --git a/a.rs b/a.rs\n@@ -3,2 +3,2 @@\n old\n-before\n+after");
        assert_eq!(lines[&(ReviewSide::Old, 3)].text, "old");
        assert_eq!(lines[&(ReviewSide::New, 3)].text, "old");
        assert_eq!(
            review_range_code(&lines, ReviewSide::Old, 4, 4).as_deref(),
            Some("before")
        );
        assert_eq!(
            review_range_code(&lines, ReviewSide::New, 4, 4).as_deref(),
            Some("after")
        );
        assert!(review_range_code(&lines, ReviewSide::New, 3, 5).is_none());
    }

    #[test]
    fn anchor_verdict_distinguishes_drift_absence_and_unloaded_lines() {
        assert_eq!(review_anchor_hash("old\nsecond"), review_anchor_hash("old"));
        let note = ReviewNote {
            id: "n1".into(),
            checkout_id: "checkout:/work".into(),
            path: "a.rs".into(),
            side: ReviewSide::New,
            line_start: 1,
            line_end: Some(2),
            content: "note".into(),
            code: "old\nsecond".into(),
            code_hash: review_anchor_hash("old"),
            outdated: false,
        };
        let same = DiffLine {
            text: "old".into(),
            mark: '+',
        };
        let changed = DiffLine {
            text: "new".into(),
            mark: '+',
        };
        assert_eq!(
            anchor_outcome(&note, Some(&same), true),
            AnchorOutcome::Unchanged
        );
        assert_eq!(
            anchor_outcome(&note, Some(&changed), true),
            AnchorOutcome::Changed
        );
        assert_eq!(anchor_outcome(&note, None, true), AnchorOutcome::Missing);
        assert_eq!(anchor_outcome(&note, None, false), AnchorOutcome::Unknown);
        let accepted = ReviewNote {
            code_hash: review_anchor_hash(&changed.text),
            outdated: false,
            ..note.clone()
        };
        assert_eq!(
            anchor_outcome(&accepted, Some(&changed), true),
            AnchorOutcome::Unchanged
        );
        let sticky = ReviewNote {
            outdated: true,
            ..note
        };
        assert_eq!(
            anchor_outcome(&sticky, Some(&same), true),
            AnchorOutcome::Changed
        );
    }

    #[test]
    fn a_note_is_the_same_note_by_what_it_delivers_and_by_nothing_else() {
        let note = ReviewNote {
            id: "n1".into(),
            checkout_id: "checkout:/work".into(),
            path: "src/main.rs".into(),
            side: ReviewSide::New,
            line_start: 10,
            line_end: None,
            content: "check this".into(),
            code: "let x = 1;".into(),
            code_hash: review_anchor_hash("let x = 1;"),
            outdated: false,
        };
        // Drift bookkeeping says where the line is now, not what was said about it.
        assert_eq!(
            review_note_snapshot(&note),
            review_note_snapshot(&ReviewNote {
                outdated: true,
                code_hash: review_anchor_hash("something else"),
                ..note.clone()
            })
        );
        // A separator inside a field cannot be mistaken for the boundary after it: the
        // two notes below are the same string to a joined snapshot.
        assert_ne!(
            review_note_snapshot(&ReviewNote {
                content: "\u{1}src/main.rs".into(),
                ..note.clone()
            }),
            review_note_snapshot(&ReviewNote {
                path: "src/main.rs\u{1}".into(),
                content: "src/main.rs".into(),
                ..note
            })
        );
    }

    #[test]
    fn a_key_says_the_same_thing_about_a_note_as_the_note_itself_does() {
        let note = ReviewNote {
            id: "n1".into(),
            checkout_id: "checkout:/work".into(),
            path: "src/main.rs".into(),
            side: ReviewSide::New,
            line_start: 10,
            line_end: None,
            content: "check this".into(),
            code: "let x = 1;".into(),
            code_hash: review_anchor_hash("let x = 1;"),
            outdated: false,
        };

        // The key is what equality is decided on, so it must not move when anything
        // that is not the note moves.
        assert_eq!(review_note_key(&note), review_note_key(&note.clone()));
        assert_eq!(
            review_note_key(&ReviewNote {
                outdated: true,
                ..note.clone()
            }),
            review_note_key(&note),
            "drift bookkeeping is not what the note said"
        );
        // And it must not be the same key for anything else: the fields that make a
        // comment a comment about a line all count.
        for other in [
            ReviewNote {
                content: "check that".into(),
                ..note.clone()
            },
            ReviewNote {
                line_start: 11,
                ..note.clone()
            },
            ReviewNote {
                side: ReviewSide::Old,
                ..note.clone()
            },
            ReviewNote {
                code: "let x = 2;".into(),
                ..note.clone()
            },
            ReviewNote {
                id: "n2".into(),
                ..note.clone()
            },
        ] {
            assert_ne!(review_note_key(&other), review_note_key(&note), "{other:?}");
        }

        // The point of a key rather than the canonical string: it does not grow with
        // the note it stands for, which is what lets a round that has given its notes
        // up still be judged by them.
        let base = ReviewNote {
            content: "c".repeat(MAX_NOTE_CONTENT_BYTES),
            code: "x".repeat(MAX_NOTE_CODE_BYTES),
            ..note
        };
        let shorter = ReviewNote {
            content: "c".repeat(MAX_NOTE_CONTENT_BYTES - 1),
            ..base.clone()
        };
        assert!(
            review_note_key(&base).len() < 80,
            "{}",
            review_note_key(&base)
        );
        assert_ne!(review_note_key(&base), review_note_key(&shorter));
    }

    #[test]
    fn a_note_that_could_not_be_delivered_is_named_and_refused() {
        let note = ReviewNote {
            id: "n1".into(),
            checkout_id: "checkout:/work".into(),
            path: "src/main.rs".into(),
            side: ReviewSide::New,
            line_start: 10,
            line_end: Some(11),
            content: "check this".into(),
            code: "let x = 1;".into(),
            code_hash: review_anchor_hash("let x = 1;"),
            outdated: false,
        };
        assert_eq!(check_review_note(&note), Ok(()));

        for break_it in [
            |note: &mut ReviewNote| note.id = " ".into(),
            |note: &mut ReviewNote| note.checkout_id = String::new(),
            |note: &mut ReviewNote| note.path = "/etc/hosts".into(),
            |note: &mut ReviewNote| note.path = "src/../src/main.rs".into(),
            |note: &mut ReviewNote| note.content = " ".into(),
            |note: &mut ReviewNote| note.content = "x".repeat(MAX_NOTE_CONTENT_BYTES + 1),
            |note: &mut ReviewNote| note.code = "x".repeat(MAX_NOTE_CODE_BYTES + 1),
            |note: &mut ReviewNote| note.line_start = 0,
            |note: &mut ReviewNote| note.line_end = Some(9),
        ] {
            let mut broken = note.clone();
            break_it(&mut broken);
            assert!(
                check_review_note(&broken).is_err(),
                "{broken:?} cannot be delivered"
            );
        }
    }

    #[test]
    fn markdown_carries_the_range_in_the_fence_the_way_a_reader_sees_it() {
        let note = |id: &str,
                    side: ReviewSide,
                    line_start: usize,
                    line_end: Option<usize>,
                    code: &str,
                    content: &str| ReviewNote {
            id: id.into(),
            checkout_id: "checkout:/work".into(),
            path: "src/foo.js".into(),
            side,
            line_start,
            line_end,
            content: content.into(),
            code: code.into(),
            code_hash: review_anchor_hash(code),
            outdated: false,
        };
        let notes = [
            note(
                "n1",
                ReviewSide::New,
                10,
                None,
                "const result = a + b;",
                "revisit this calculation",
            ),
            note(
                "n2",
                ReviewSide::New,
                67,
                Some(72),
                "function handleUser(user) {\n  if (user.name) {\n    return user.name;\n  }\n}",
                "null check `user` before `.name`",
            ),
            // A note about a line the change removed is written with the minus
            // the picker showed, which is the only thing in the document that
            // says the comment is about the old side rather than the new one.
            note(
                "n3",
                ReviewSide::Old,
                3,
                Some(3),
                "const gone = true;",
                "this whole branch goes",
            ),
        ];
        // The fence carries the lines the note covers as two numbers, one for a
        // line and the two ends for a range, and the note itself is the quote
        // under it. The marker on the first line is invisible in a reader and
        // is what makes an export this app's own to delete later.
        assert_eq!(
            build_review_markdown(
                &notes,
                &ReviewContext {
                    branch: Some("feature"),
                    default_branch: Some("main"),
                    date: "2026-03-14",
                    language: |_| "js",
                }
            ),
            r#"<!-- muster-review-export -->
# Code Review 2026-03-14

> `main..feature` — 1 file, 3 notes

## src/foo.js

```js{-3}
const gone = true;
```

> this whole branch goes

---

```js{10}
const result = a + b;
```

> revisit this calculation

---

```js{67,72}
function handleUser(user) {
  if (user.name) {
    return user.name;
  }
}
```

> null check `user` before `.name`
"#
        );
        assert_eq!(build_review_markdown(&[], &ReviewContext::default()), "");
    }

    #[test]
    fn code_that_quotes_a_fence_gets_a_longer_one() {
        let note = |code: &str| ReviewNote {
            id: "n1".into(),
            checkout_id: "checkout:/work".into(),
            path: "README.md".into(),
            side: ReviewSide::New,
            line_start: 3,
            line_end: None,
            content: "the fence inside this code".into(),
            code: code.into(),
            code_hash: review_anchor_hash(code),
            outdated: false,
        };
        let exported = |code: &str| {
            build_review_markdown(
                &[note(code)],
                &ReviewContext {
                    branch: Some("feature"),
                    default_branch: Some("main"),
                    date: "2026-03-14",
                    language: |_| "md",
                },
            )
        };

        // Source that closes a three-backtick fence: the opening fence has to be
        // longer than the longest run inside the code, or the rest of the
        // reviewed document renders as the review rather than as its code.
        assert_eq!(
            exported("```rust\nlet x = 1;\n```\n```\nrest of it"),
            "<!-- muster-review-export -->\n\
             # Code Review 2026-03-14\n\
             \n\
             > `main..feature` — 1 file, 1 note\n\
             \n\
             ## README.md\n\
             \n\
             ````md{3}\n\
             ```rust\n\
             let x = 1;\n\
             ```\n\
             ```\n\
             rest of it\n\
             ````\n\
             \n\
             > the fence inside this code\n"
        );

        // A run longer than three escalates the same way, and the range notation
        // and the language stay on the opening line.
        assert_eq!(code_fence("`"), "```");
        assert_eq!(code_fence("no backticks here"), "```");
        assert_eq!(code_fence("``````"), "```````");
        assert_eq!(code_fence("`` and `"), "```");
        assert!(
            exported("``````js").contains("\n```````md{3}\n"),
            "{:?}",
            exported("``````js")
        );
    }
}
