use std::sync::atomic::{AtomicU64, Ordering};

use crate::{
    domain::{
        ipc::{IpcError, IpcErrorCode},
        review::{review_anchor_hash, ReviewNote, MAX_NOTE_CODE_BYTES, MAX_NOTE_CONTENT_BYTES},
    },
    persistence::{timestamp, Database},
};

static NEXT_NOTE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct NewReviewNote {
    pub path: String,
    pub side: String,
    pub line_start: i64,
    pub line_end: Option<i64>,
    pub content: String,
    pub code: String,
}

/// One note's current anchor text, straight from the diff the WebView is rendering.
#[derive(Clone)]
pub struct NoteAnchorCheck {
    pub id: String,
    pub current_code: String,
}

fn invalid(message: &str) -> IpcError {
    IpcError::new(IpcErrorCode::InvalidPath, message)
}

fn validate_relative_path(path: &str) -> Result<(), IpcError> {
    if path.is_empty() || path.len() > 4096 || path.starts_with('/') || path.contains('\\') {
        return Err(invalid("review notes require a repository-relative path"));
    }
    if path
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(invalid("review notes require a repository-relative path"));
    }
    Ok(())
}

fn validate_content(content: &str) -> Result<String, IpcError> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(IpcError::new(
            IpcErrorCode::OperationFailed,
            "a review note needs text",
        ));
    }
    if trimmed.len() > MAX_NOTE_CONTENT_BYTES {
        return Err(IpcError::new(
            IpcErrorCode::OperationFailed,
            "the review note is too long",
        ));
    }
    Ok(trimmed.to_string())
}

pub fn notes(database: &Database, checkout_id: &str) -> Result<Vec<ReviewNote>, IpcError> {
    ensure_checkout(database, checkout_id)?;
    database
        .review_notes(checkout_id)
        .map_err(|error| IpcError::new(IpcErrorCode::OperationFailed, error))
}

fn ensure_checkout(database: &Database, checkout_id: &str) -> Result<(), IpcError> {
    database
        .terminal_checkout_path(checkout_id)
        .map(|_| ())
        .map_err(|error| IpcError::new(IpcErrorCode::InvalidCheckout, error))
}

pub fn add_note(
    database: &Database,
    checkout_id: &str,
    new_note: NewReviewNote,
) -> Result<ReviewNote, IpcError> {
    ensure_checkout(database, checkout_id)?;
    validate_relative_path(&new_note.path)?;
    if new_note.side != "old" && new_note.side != "new" {
        return Err(invalid("a review note must belong to the old or new side"));
    }
    if new_note.line_start < 1
        || new_note
            .line_end
            .is_some_and(|line_end| line_end < new_note.line_start)
    {
        return Err(invalid("a review note must anchor to a valid line range"));
    }
    let content = validate_content(&new_note.content)?;
    let code: String = new_note.code.chars().take(MAX_NOTE_CODE_BYTES).collect();
    let now = timestamp();
    let note = ReviewNote {
        id: format!(
            "note:{}:{}:{}",
            now,
            std::process::id(),
            NEXT_NOTE_ID.fetch_add(1, Ordering::Relaxed)
        ),
        checkout_id: checkout_id.to_string(),
        path: new_note.path,
        side: new_note.side,
        line_start: new_note.line_start,
        // A range that ends where it starts is just a single line.
        line_end: new_note
            .line_end
            .filter(|line_end| *line_end > new_note.line_start),
        content,
        code_hash: review_anchor_hash(&code),
        code,
        status: "draft".into(),
        outdated: false,
        round_id: None,
        created_at: now.clone(),
        updated_at: now,
    };
    database
        .add_review_note(&note)
        .map_err(|error| IpcError::new(IpcErrorCode::OperationFailed, error))
}

/// Marks every note whose anchored line no longer matches the diff as outdated.
///
/// The WebView sends the anchor text it is rendering because the backend cannot always
/// recompute it: a large diff has no patch to reparse. The fingerprint comparison stays
/// here so the sticky mark is only ever set from one place. Marks are never cleared.
pub fn verify_note_anchors(
    database: &Database,
    checkout_id: &str,
    path: &str,
    checks: &[NoteAnchorCheck],
) -> Result<Vec<ReviewNote>, IpcError> {
    ensure_checkout(database, checkout_id)?;
    validate_relative_path(path)?;
    if checks.is_empty() || checks.len() > 1000 {
        return Err(IpcError::new(
            IpcErrorCode::OperationFailed,
            "a review anchor check needs between 1 and 1000 notes",
        ));
    }
    let ids: Vec<String> = checks.iter().map(|check| check.id.clone()).collect();
    // A foreign or unknown ID fails the whole batch instead of silently skipping.
    let stored = database
        .review_note_hashes(checkout_id, path, &ids)
        .map_err(|error| IpcError::new(IpcErrorCode::CheckoutOwnershipMismatch, error))?;
    let drifted: Vec<&str> = checks
        .iter()
        .zip(stored)
        .filter(|(check, (_, code_hash))| *code_hash != review_anchor_hash(&check.current_code))
        .map(|(check, _)| check.id.as_str())
        .collect();
    for id in drifted {
        database
            .mark_review_note_outdated(id, checkout_id)
            .map_err(|error| IpcError::new(IpcErrorCode::CheckoutOwnershipMismatch, error))?;
    }
    notes(database, checkout_id)
}

/// The user accepts that a drifted note points at old code and takes responsibility for it.
pub fn clear_outdated(
    database: &Database,
    checkout_id: &str,
    id: &str,
) -> Result<ReviewNote, IpcError> {
    database
        .clear_review_note_outdated(id, checkout_id)
        .map_err(|error| IpcError::new(IpcErrorCode::CheckoutOwnershipMismatch, error))
}

pub fn update_note(
    database: &Database,
    checkout_id: &str,
    id: &str,
    content: &str,
) -> Result<ReviewNote, IpcError> {
    let content = validate_content(content)?;
    database
        .update_review_note(id, checkout_id, &content)
        .map_err(|error| IpcError::new(IpcErrorCode::CheckoutOwnershipMismatch, error))
}

pub fn delete_note(database: &Database, checkout_id: &str, id: &str) -> Result<(), IpcError> {
    database
        .delete_review_note(id, checkout_id)
        .map_err(|error| IpcError::new(IpcErrorCode::CheckoutOwnershipMismatch, error))
}

pub fn mark_sent(database: &Database, checkout_id: &str, ids: &[String]) -> Result<(), IpcError> {
    if ids.is_empty() || ids.len() > 1000 {
        return Err(IpcError::new(
            IpcErrorCode::OperationFailed,
            "a review round needs between 1 and 1000 notes",
        ));
    }
    database
        .mark_review_notes_sent(checkout_id, ids)
        .map_err(|error| IpcError::new(IpcErrorCode::OperationFailed, error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::{tempdir, TempDir};

    use crate::{domain::workspace::Repo, persistence::Database};

    use super::{
        add_note, clear_outdated, delete_note, mark_sent, notes, update_note, verify_note_anchors,
        NewReviewNote, NoteAnchorCheck,
    };

    fn note_input(path: &str, line_start: i64) -> NewReviewNote {
        NewReviewNote {
            path: path.into(),
            side: "new".into(),
            line_start,
            line_end: None,
            content: "check this".into(),
            code: "const result = a + b;".into(),
        }
    }

    fn database_with_two_checkouts(temp: &TempDir) -> (Database, String, String) {
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let database = Database::open(temp.path().join("workspace.sqlite3")).unwrap();
        let first = Repo::plain(&first, "now").unwrap();
        let second = Repo::plain(&second, "now").unwrap();
        database.register_plain_repo(first.clone()).unwrap();
        database.register_plain_repo(second.clone()).unwrap();
        (
            database,
            first.checkouts[0].id.clone(),
            second.checkouts[0].id.clone(),
        )
    }

    #[test]
    fn notes_are_stored_edited_and_deleted_per_checkout() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, other_checkout_id) = database_with_two_checkouts(&temp);

        let created = add_note(&database, &checkout_id, note_input("src/foo.js", 10)).unwrap();
        assert_eq!(created.status, "draft");
        assert_eq!(
            notes(&database, &checkout_id)
                .unwrap()
                .into_iter()
                .map(|note| note.content)
                .collect::<Vec<_>>(),
            ["check this"]
        );

        let updated = update_note(&database, &checkout_id, &created.id, "revisit this ").unwrap();
        assert_eq!(updated.content, "revisit this");
        assert_eq!(updated.status, "draft");

        mark_sent(&database, &checkout_id, std::slice::from_ref(&created.id)).unwrap();
        assert_eq!(notes(&database, &checkout_id).unwrap()[0].status, "sent");

        assert!(update_note(&database, &checkout_id, &created.id, "draft again").is_ok());
        assert_eq!(notes(&database, &checkout_id).unwrap()[0].status, "draft");

        delete_note(&database, &checkout_id, &created.id).unwrap();
        assert!(notes(&database, &checkout_id).unwrap().is_empty());
        assert!(notes(&database, &other_checkout_id).unwrap().is_empty());
    }

    #[test]
    fn invalid_notes_are_rejected() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);

        assert!(add_note(&database, "checkout:unknown", note_input("src/foo.js", 1)).is_err());

        let mut escaped = note_input("../outside.js", 1);
        assert!(add_note(&database, &checkout_id, escaped.clone()).is_err());

        escaped = note_input("/etc/passwd", 1);
        assert!(add_note(&database, &checkout_id, escaped).is_err());

        let mut wrong_side = note_input("src/foo.js", 1);
        wrong_side.side = "middle".into();
        assert!(add_note(&database, &checkout_id, wrong_side).is_err());

        let mut wrong_line = note_input("src/foo.js", 0);
        wrong_line.line_start = 0;
        assert!(add_note(&database, &checkout_id, wrong_line).is_err());

        let mut inverted = note_input("src/foo.js", 12);
        inverted.line_start = 12;
        inverted.line_end = Some(4);
        assert!(add_note(&database, &checkout_id, inverted).is_err());

        let mut empty = note_input("src/foo.js", 1);
        empty.content = "   ".into();
        assert!(add_note(&database, &checkout_id, empty).is_err());
    }

    #[test]
    fn sec_06_review_note_ids_are_validated_against_the_requested_checkout() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, other_checkout_id) = database_with_two_checkouts(&temp);
        let created = add_note(&database, &checkout_id, note_input("src/foo.js", 10)).unwrap();

        assert!(update_note(&database, &other_checkout_id, &created.id, "stolen").is_err());
        assert!(delete_note(&database, &other_checkout_id, &created.id).is_err());
        assert!(mark_sent(
            &database,
            &other_checkout_id,
            std::slice::from_ref(&created.id)
        )
        .is_err());
        assert_eq!(
            notes(&database, &checkout_id).unwrap()[0].content,
            "check this"
        );
        assert!(notes(&database, "checkout:unknown").is_err());
    }

    #[test]
    fn marking_sent_requires_notes() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);

        assert!(mark_sent(&database, &checkout_id, &[]).is_err());
    }

    #[test]
    fn a_range_keeps_its_end_but_a_single_line_range_is_flattened() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);

        let mut range = note_input("src/foo.js", 10);
        range.line_end = Some(24);
        range.code = "line ten\nline eleven".into();
        let created = add_note(&database, &checkout_id, range).unwrap();
        assert_eq!(created.line_end, Some(24));

        let mut flattened = note_input("src/foo.js", 30);
        flattened.line_end = Some(30);
        assert_eq!(
            add_note(&database, &checkout_id, flattened)
                .unwrap()
                .line_end,
            None
        );
    }

    #[test]
    fn a_drifted_anchor_is_marked_outdated_and_only_the_user_clears_it() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);

        let created = add_note(&database, &checkout_id, note_input("src/foo.js", 10)).unwrap();
        let same = NoteAnchorCheck {
            id: created.id.clone(),
            current_code: "const result = a + b;".into(),
        };
        let after_same = verify_note_anchors(
            &database,
            &checkout_id,
            "src/foo.js",
            std::slice::from_ref(&same),
        )
        .unwrap();
        assert!(!after_same[0].outdated);

        let drifted = NoteAnchorCheck {
            id: created.id.clone(),
            current_code: "const result = a - b;".into(),
        };
        let after_drift = verify_note_anchors(
            &database,
            &checkout_id,
            "src/foo.js",
            std::slice::from_ref(&drifted),
        )
        .unwrap();
        assert!(after_drift[0].outdated);

        // Restoring the original text must not silently heal a sticky mark.
        let after_restore = verify_note_anchors(
            &database,
            &checkout_id,
            "src/foo.js",
            std::slice::from_ref(&same),
        )
        .unwrap();
        assert!(after_restore[0].outdated);

        let cleared = clear_outdated(&database, &checkout_id, &created.id).unwrap();
        assert!(!cleared.outdated);
        // Clearing is not idempotent: there is no mark left to clear.
        assert!(clear_outdated(&database, &checkout_id, &created.id).is_err());
    }

    #[test]
    fn editing_a_note_leaves_its_outdated_mark_alone() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);

        let created = add_note(&database, &checkout_id, note_input("src/foo.js", 10)).unwrap();
        verify_note_anchors(
            &database,
            &checkout_id,
            "src/foo.js",
            &[NoteAnchorCheck {
                id: created.id.clone(),
                current_code: "something else entirely".into(),
            }],
        )
        .unwrap();

        let edited = update_note(&database, &checkout_id, &created.id, "still applies").unwrap();
        assert!(edited.outdated);
        assert_eq!(edited.status, "draft");
    }

    #[test]
    fn anchor_checks_need_a_path_and_at_least_one_note() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, _) = database_with_two_checkouts(&temp);

        assert!(verify_note_anchors(&database, &checkout_id, "src/foo.js", &[]).is_err());
        assert!(verify_note_anchors(
            &database,
            &checkout_id,
            "../escape.js",
            &[NoteAnchorCheck {
                id: "note:1".into(),
                current_code: "x".into()
            }]
        )
        .is_err());
        assert!(verify_note_anchors(
            &database,
            "checkout:unknown",
            "src/foo.js",
            &[NoteAnchorCheck {
                id: "note:1".into(),
                current_code: "x".into()
            }]
        )
        .is_err());
    }

    #[test]
    fn sec_08_anchor_checks_cannot_target_a_foreign_checkout_or_path() {
        let temp = tempdir().unwrap();
        let (database, checkout_id, other_checkout_id) = database_with_two_checkouts(&temp);
        let created = add_note(&database, &checkout_id, note_input("src/foo.js", 10)).unwrap();
        let check = NoteAnchorCheck {
            id: created.id.clone(),
            current_code: "const result = a - b;".into(),
        };

        // Another checkout cannot mark this note, not even with a matching path.
        assert!(verify_note_anchors(
            &database,
            &other_checkout_id,
            "src/foo.js",
            std::slice::from_ref(&check)
        )
        .is_err());
        // Nor can a different file inside the right checkout.
        assert!(verify_note_anchors(
            &database,
            &checkout_id,
            "src/other.js",
            std::slice::from_ref(&check)
        )
        .is_err());
        // One ID that does not belong to this checkout+path fails the batch, so the
        // drifted note in the same batch is left alone rather than half-applied.
        let elsewhere = add_note(
            &database,
            &checkout_id,
            NewReviewNote {
                path: "src/other.js".into(),
                side: "new".into(),
                line_start: 5,
                line_end: None,
                content: "check this too".into(),
                code: "const other = true;".into(),
            },
        )
        .unwrap();
        let foreign = NoteAnchorCheck {
            id: elsewhere.id,
            current_code: "const other = false;".into(),
        };
        assert!(verify_note_anchors(
            &database,
            &checkout_id,
            "src/foo.js",
            &[check.clone(), foreign]
        )
        .is_err());
        let after_failed_batch = notes(&database, &checkout_id).unwrap();
        assert!(!after_failed_batch[0].outdated);
        assert!(!after_failed_batch[1].outdated);

        assert!(clear_outdated(&database, &other_checkout_id, &created.id).is_err());
        assert!(clear_outdated(&database, "checkout:unknown", &created.id).is_err());
        assert!(!notes(&database, &checkout_id).unwrap()[0].outdated);
    }
}
