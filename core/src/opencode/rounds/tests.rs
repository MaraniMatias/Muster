//! What the round store promises about a send that may not have gone out.
//!
//! The service here is the loopback stand-in from `crate::opencode::mock`: every test
//! drives the real client against it, so the marker that is looked for is the one that
//! was actually written to the wire.

use std::{
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
};

use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::{
    opencode::mock::{self, Mock, Request},
    review::{review_anchor_hash, ReviewSide},
    workspace::Repo,
};

const SESSION: &str = "ses_review";
const ELSEWHERE: &str = "checkout:/somewhere/else";

/// An OpenCode service that behaves like the real one for one checkout: it answers
/// every session it is asked about, records the prompts it takes in that session's
/// transcript, and can be told to lose or refuse the answer to one of them.
struct Service {
    mock: Mock,
    /// Everything the session has been told, which is what the marker is looked for in.
    transcript: Arc<Mutex<Vec<String>>>,
    /// How many more prompt posts to take without answering, and then to refuse, and then
    /// to accept without ever appearing in the transcript.
    silent: Arc<AtomicUsize>,
    refused: Arc<AtomicUsize>,
    swallowed: Arc<AtomicUsize>,
}

impl Service {
    /// Serves on `port`, or on a free one when it is `0`.
    fn start_on(port: u16, directory: &str, probe: impl Fn() + Send + Sync + 'static) -> Self {
        let transcript: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let silent = Arc::new(AtomicUsize::new(0));
        let refused = Arc::new(AtomicUsize::new(0));
        let swallowed = Arc::new(AtomicUsize::new(0));
        let (taken, silent_prompts, refused_prompts, swallowed_prompts) = (
            Arc::clone(&transcript),
            Arc::clone(&silent),
            Arc::clone(&refused),
            Arc::clone(&swallowed),
        );
        let probe: Arc<dyn Fn() + Send + Sync> = Arc::new(probe);
        let on_prompt = Arc::clone(&probe);
        let location = directory.to_string();
        let handler = move |request: &Request| {
            let answer = |data: serde_json::Value| {
                mock::reply(
                    200,
                    json!({ "data": data, "location": { "directory": location } }),
                )
            };
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/api/info") => mock::reply(200, json!({ "data": {} })),
                ("POST", "/api/session") => answer(json!({ "id": SESSION })),
                ("GET", path) if path == format!("/api/session/{SESSION}") => {
                    answer(json!({ "id": SESSION }))
                }
                ("POST", path) if path == format!("/api/session/{SESSION}/prompt") => {
                    let body = request.json();
                    let text = body["text"].as_str().expect("a prompt body").to_string();
                    on_prompt();
                    if take_one(&swallowed_prompts) {
                        // Taken, and the transcript never shows it: what a service that
                        // accepted a prompt and has not written it down yet looks like.
                        return mock::silent();
                    }
                    // Taken first, answered after: losing the answer does not un-take it.
                    taken.lock().unwrap().push(text);
                    if take_one(&silent_prompts) {
                        return mock::silent();
                    }
                    if take_one(&refused_prompts) {
                        return mock::reply(500, json!({}));
                    }
                    answer(json!({}))
                }
                ("GET", path) if path == format!("/api/session/{SESSION}/message") => {
                    let messages = taken
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|text| json!({ "parts": [{ "text": text }] }))
                        .collect::<Vec<_>>();
                    answer(json!(messages))
                }
                _ => mock::reply(404, json!({})),
            }
        };
        let mock = if port == 0 {
            Mock::start(handler)
        } else {
            Mock::start_on(port, handler)
        };
        Self {
            mock,
            transcript,
            silent,
            refused,
            swallowed,
        }
    }

    /// Take the next `count` prompts and answer none of them.
    fn lose_next_answers(&self, count: usize) {
        self.silent.store(count, Ordering::SeqCst);
    }

    /// Take the next `count` prompts and answer none of them with a refusal.
    fn refuse_next(&self, count: usize) {
        self.refused.store(count, Ordering::SeqCst);
    }

    /// Take the next `count` prompts and leave no trace of them anywhere.
    fn swallow_next(&self, count: usize) {
        self.swallowed.store(count, Ordering::SeqCst);
    }

    /// The prompts this service took, in order.
    fn prompts(&self) -> Vec<String> {
        self.transcript.lock().unwrap().clone()
    }

    /// Every prompt post this service was asked for, answered or not.
    fn prompt_posts(&self) -> Vec<String> {
        self.mock
            .requests()
            .into_iter()
            .filter(|request| request.path.ends_with("/prompt"))
            .map(|request| {
                request.json()["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }
}

fn take_one(counter: &AtomicUsize) -> bool {
    counter
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
            left.checked_sub(1)
        })
        .is_ok()
}

struct Fixture {
    home: TempDir,
    checkout: Checkout,
    service: Service,
    client: Arc<Client>,
    store: RoundStore,
    /// The rounds the store held at the moment each prompt arrived.
    at_prompt: Arc<Mutex<Vec<Vec<ReviewRound>>>>,
}

impl Fixture {
    fn start() -> Self {
        let home = tempfile::tempdir().unwrap();
        let checkout_path = home.path().join("checkout");
        fs::create_dir_all(&checkout_path).unwrap();
        let checkout = Repo::plain(&checkout_path, "now").unwrap().checkouts[0].clone();
        let at_prompt: Arc<Mutex<Vec<Vec<ReviewRound>>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&at_prompt);
        let state = home.path().join(".muster");
        let checkout_id = checkout.id.clone();
        let service = Service::start_on(0, &checkout.canonical_path, move || {
            // The store is read, never locked: this runs inside the send that holds the lock.
            let store = RoundStore::open_in(&state).expect("the store is readable");
            recorded
                .lock()
                .expect("the record should not be poisoned")
                .push(store.rounds(&checkout_id).unwrap_or_default());
        });
        register(&home, service.mock.port());
        let client = Client::connect(home.path(), Path::new(&checkout.path)).unwrap();
        let store = RoundStore::open_in(&home.path().join(".muster")).unwrap();
        Self {
            home,
            checkout,
            service,
            client: Arc::new(client),
            store,
            at_prompt,
        }
    }

    fn notes(&self) -> Vec<ReviewNote> {
        vec![note(&self.checkout.id, "note-1")]
    }

    fn request<'a>(&'a self, notes: &'a [ReviewNote], markdown: &'a str) -> RoundRequest<'a> {
        RoundRequest {
            checkout: &self.checkout,
            session_id: SESSION,
            notes,
            review_markdown: markdown,
        }
    }

    /// Sends the notes as they are, the way the window does.
    fn send(&self, notes: &[ReviewNote]) -> Result<ReviewRound, DispatchError> {
        self.store.send_round(
            &self.client,
            self.request(notes, "# Code Review 2026-06-02\n"),
        )
    }

    fn settle(&self, round_id: &str) -> ReviewRound {
        self.store
            .reconcile_round(&self.client, &self.checkout.id, round_id)
            .unwrap()
    }

    fn status(&self, round_id: &str) -> RoundStatus {
        self.store
            .round(&self.checkout.id, round_id)
            .unwrap()
            .expect("the round is recorded")
            .status
    }
}

fn register(home: &TempDir, port: u16) {
    let directory = home.path().join(".local/state/opencode");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("service.json"),
        json!({
            "version": "2.0.22",
            "url": format!("http://127.0.0.1:{port}"),
            "password": "secret",
        })
        .to_string(),
    )
    .unwrap();
}

fn note(checkout_id: &str, id: &str) -> ReviewNote {
    ReviewNote {
        id: id.into(),
        checkout_id: checkout_id.into(),
        path: "src/main.rs".into(),
        side: ReviewSide::New,
        line_start: 10,
        line_end: None,
        content: "check this".into(),
        code: "let x = 1;".into(),
        code_hash: review_anchor_hash("let x = 1;"),
        outdated: false,
    }
}

#[test]
fn a_round_waiting_to_be_offered_is_durable_as_dispatching_before_the_request_goes_out() {
    let fixture = Fixture::start();
    let port = fixture.service.mock.port();
    fixture.service.mock.stop();

    // The first send was never taken by the service, so the round is queued: provably not
    // delivered, and safe to offer again. Nothing about it says the next offer started.
    assert!(fixture.send(&fixture.notes()).unwrap_err().never_sent());
    let queued = fixture
        .store
        .rounds(&fixture.checkout.id)
        .unwrap()
        .remove(0);
    assert_eq!(queued.status, RoundStatus::Queued);

    // The service comes back on the same port and the same review is asked for again.
    let at_prompt: Arc<Mutex<Vec<Vec<ReviewRound>>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&at_prompt);
    let state = fixture.store.path().parent().unwrap().to_path_buf();
    let checkout_id = fixture.checkout.id.clone();
    let replacement = Service::start_on(port, &fixture.checkout.canonical_path, move || {
        let store = RoundStore::open_in(&state).expect("the store is readable");
        recorded
            .lock()
            .expect("the record should not be poisoned")
            .push(store.rounds(&checkout_id).unwrap_or_default());
    });
    let round = fixture
        .store
        .send_round(
            &fixture.client,
            fixture.request(&fixture.notes(), "# Code Review 2026-06-02\n"),
        )
        .unwrap();

    assert_eq!(round.status, RoundStatus::Dispatched);
    // What the file said while the prompt was in flight: a question to settle, never a
    // retry. A round left `queued` here would be offered a second time by `flush_rounds`
    // if this process had died between the request and the answer.
    let at_prompt = at_prompt.lock().unwrap();
    assert_eq!(at_prompt.len(), 1, "one prompt arrived");
    assert_eq!(
        at_prompt[0][0].id, queued.id,
        "it is the same round, offered again"
    );
    assert_eq!(at_prompt[0][0].status, RoundStatus::Dispatching);
    assert_eq!(replacement.prompts(), vec![queued.prompt]);
}

#[test]
fn a_client_scoped_to_another_checkout_cannot_carry_this_checkouts_review() {
    let fixture = Fixture::start();
    let elsewhere = fixture.home.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let other = Client::connect(fixture.home.path(), &elsewhere).unwrap();

    let error = fixture
        .store
        .send_round(
            &other,
            fixture.request(&fixture.notes(), "# Code Review 2026-06-02\n"),
        )
        .unwrap_err();

    assert!(matches!(error, DispatchError::Foreign(_)), "{error}");
    assert!(
        fixture
            .store
            .rounds(&fixture.checkout.id)
            .unwrap()
            .is_empty(),
        "nothing is recorded for a review nobody offered"
    );
    assert!(
        fixture.service.prompt_posts().is_empty(),
        "nothing goes on the wire for another checkout"
    );
}

#[test]
fn a_state_directory_that_is_a_link_or_a_public_place_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let elsewhere = home.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let state = home.path().join(".muster");

    // A link is not a directory this app keeps its prompts in, and nothing is written
    // through it.
    std::os::unix::fs::symlink(&elsewhere, &state).unwrap();
    let refused =
        |error: DispatchError| assert!(matches!(error, DispatchError::Foreign(_)), "{error:?}");
    refused(RoundStore::open_in(&state).unwrap_err());
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);

    // And a directory anybody may write is not private to the person whose reviews these
    // are, so it is refused as well.
    fs::remove_file(&state).unwrap();
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, std::os::unix::fs::PermissionsExt::from_mode(0o777)).unwrap();
    refused(RoundStore::open_in(&state).unwrap_err());
}

#[test]
fn a_round_is_on_disk_with_its_marker_and_its_exact_prompt_before_the_request_goes_out() {
    let fixture = Fixture::start();

    let round = fixture.send(&fixture.notes()).unwrap();

    assert_eq!(round.status, RoundStatus::Dispatched);
    assert_eq!(round.checkout_id, fixture.checkout.id);
    assert_eq!(round.session_id, SESSION);
    assert_eq!(round.marker, format!("muster-review:{}", round.id));
    assert_eq!(round.note_ids, vec!["note-1".to_string()]);
    assert_eq!(round.notes, fixture.notes());

    // What the service was asked to send is exactly what the round holds, marker and all.
    assert_eq!(fixture.service.prompts(), vec![round.prompt.clone()]);

    // The ordering the marker rests on: the round was already written, marker and prompt
    // included, at the moment the prompt arrived and not one moment later.
    let at_prompt = fixture.at_prompt.lock().unwrap();
    assert_eq!(at_prompt.len(), 1, "one prompt arrived");
    let recorded = at_prompt[0]
        .iter()
        .find(|candidate| candidate.id == round.id)
        .expect("the round was on disk when the request went out");
    assert_eq!(recorded.marker, round.marker);
    assert_eq!(recorded.prompt, round.prompt);
    assert_eq!(recorded.status, RoundStatus::Dispatching);
    drop(at_prompt);

    // Re-read through a second handle: it survived the write.
    let reopened = RoundStore::open_in(&fixture.home.path().join(".muster")).unwrap();
    assert_eq!(
        reopened.round(&fixture.checkout.id, &round.id).unwrap(),
        Some(round)
    );
}

#[test]
fn the_store_is_the_apps_own_state_and_never_touches_the_checkout() {
    let fixture = Fixture::start();
    fixture.send(&fixture.notes()).unwrap();

    let store = fixture.store.path().to_path_buf();
    assert!(
        store.starts_with(fixture.home.path().join(".muster")),
        "{store:?}"
    );
    assert_eq!(fs::read_dir(&fixture.checkout.path).unwrap().count(), 0);
}

#[test]
fn a_round_refuses_a_checkout_this_workspace_did_not_register() {
    let fixture = Fixture::start();
    let notes = fixture.notes();

    let forged_id = Checkout {
        id: ELSEWHERE.into(),
        ..fixture.checkout.clone()
    };
    let forged_path = Checkout {
        path: "/somewhere/else".into(),
        ..fixture.checkout.clone()
    };
    let missing = Checkout {
        is_missing: true,
        ..fixture.checkout.clone()
    };
    for checkout in [&forged_id, &forged_path, &missing] {
        let error = fixture
            .store
            .send_round(
                &fixture.client,
                RoundRequest {
                    checkout,
                    session_id: SESSION,
                    notes: &notes,
                    review_markdown: "# Code Review",
                },
            )
            .unwrap_err();
        assert!(matches!(error, DispatchError::Foreign(_)), "{error}");
    }

    // A note that belongs to another checkout cannot ride along in this one's round.
    let foreign = vec![note(ELSEWHERE, "note-2")];
    let error = fixture
        .store
        .send_round(
            &fixture.client,
            RoundRequest {
                checkout: &fixture.checkout,
                session_id: SESSION,
                notes: &foreign,
                review_markdown: "# Code Review",
            },
        )
        .unwrap_err();
    assert!(matches!(error, DispatchError::Foreign(_)), "{error}");
    assert!(fixture
        .store
        .rounds(&fixture.checkout.id)
        .unwrap()
        .is_empty());
    assert!(fixture.service.prompt_posts().is_empty());
}

#[test]
fn a_settled_round_is_never_offered_again() {
    let fixture = Fixture::start();

    let round = fixture.send(&fixture.notes()).unwrap();

    assert_eq!(fixture.status(&round.id), RoundStatus::Dispatched);
    assert_eq!(
        fixture
            .store
            .flush_rounds(&fixture.client, &fixture.checkout.id)
            .unwrap(),
        0
    );
    assert_eq!(fixture.service.prompt_posts().len(), 1);

    // The same notes, asked for again, are the round that already carries them.
    assert_eq!(fixture.send(&fixture.notes()).unwrap().id, round.id);
    assert_eq!(fixture.service.prompt_posts().len(), 1);
}

#[test]
fn a_send_whose_answer_was_lost_is_left_unresolved_and_is_never_repeated() {
    let fixture = Fixture::start();
    fixture.service.lose_next_answers(1);

    // The service took the prompt and never answered. Nothing here may guess.
    let error = fixture.send(&fixture.notes()).unwrap_err();
    assert!(matches!(error, DispatchError::Unavailable(_)), "{error}");
    assert!(!error.never_sent(), "the answer was lost, not the request");
    assert_eq!(
        fixture.status(&fixture.store.rounds(&fixture.checkout.id).unwrap()[0].id),
        RoundStatus::Dispatching
    );

    // Asking again is answered from the store, not from the service: one review, one round.
    let again = fixture.send(&fixture.notes()).unwrap();
    assert_eq!(
        fixture.status(&again.id),
        RoundStatus::Dispatching,
        "the round is still the one question"
    );
    assert_eq!(
        fixture
            .store
            .flush_rounds(&fixture.client, &fixture.checkout.id)
            .unwrap(),
        0,
        "an unresolved round must not be offered again"
    );
    assert_eq!(fixture.service.prompt_posts().len(), 1);

    // The marker is in the transcript, so the send did land after all.
    let round = fixture
        .store
        .rounds(&fixture.checkout.id)
        .unwrap()
        .remove(0);
    assert!(fixture.service.prompts()[0].contains(&round.marker));
    assert_eq!(fixture.settle(&round.id).status, RoundStatus::Dispatched);
    assert_eq!(fixture.service.prompt_posts().len(), 1);
}

#[test]
fn a_send_the_service_never_heard_is_a_retry_rather_than_a_question() {
    let fixture = Fixture::start();
    let port = fixture.service.mock.port();
    fixture.service.mock.stop();

    // Nothing is listening, so `connect` failed and no byte of the request was written.
    let error = fixture.send(&fixture.notes()).unwrap_err();
    assert!(error.never_sent(), "{error}");
    let round = fixture
        .store
        .rounds(&fixture.checkout.id)
        .unwrap()
        .remove(0);
    assert_eq!(
        fixture.status(&round.id),
        RoundStatus::Queued,
        "provably not taken is safe to offer again"
    );
    assert_eq!(
        fixture
            .store
            .pending(&fixture.checkout.id)
            .unwrap()
            .iter()
            .map(|pending| pending.id.as_str())
            .collect::<Vec<_>>(),
        vec![round.id.as_str()],
        "it is on the list to retry"
    );

    // The service comes back where it was, and the retry goes out as itself.
    let replacement = Service::start_on(port, &fixture.checkout.canonical_path, || {});
    register(&fixture.home, replacement.mock.port());
    let client = Client::connect(fixture.home.path(), Path::new(&fixture.checkout.path)).unwrap();
    assert_eq!(
        fixture
            .store
            .flush_rounds(&client, &fixture.checkout.id)
            .unwrap(),
        1
    );
    assert_eq!(replacement.prompts(), vec![round.prompt]);
}

#[test]
fn a_send_that_leaves_no_trace_is_blocked_and_only_a_person_unblocks_it() {
    let fixture = Fixture::start();
    fixture.service.swallow_next(1);

    let error = fixture.send(&fixture.notes()).unwrap_err();
    assert!(!error.never_sent(), "{error}");
    let round = fixture
        .store
        .rounds(&fixture.checkout.id)
        .unwrap()
        .remove(0);

    // The transcript has no marker, which is not proof the prompt was never taken: a
    // service may take a message and write it down later, or restart before it does.
    assert_eq!(fixture.settle(&round.id).status, RoundStatus::Unknown);
    assert_eq!(
        fixture
            .store
            .flush_rounds(&fixture.client, &fixture.checkout.id)
            .unwrap(),
        0,
        "a blocked round is not offered on its own"
    );
    assert_eq!(fixture.service.prompt_posts().len(), 1);

    // The way out is a person asking for it.
    assert_eq!(
        fixture
            .store
            .resend_round(&fixture.checkout.id, &round.id)
            .unwrap()
            .status,
        RoundStatus::Queued
    );
    assert_eq!(
        fixture
            .store
            .flush_rounds(&fixture.client, &fixture.checkout.id)
            .unwrap(),
        1
    );
    assert_eq!(fixture.service.prompt_posts().len(), 2);
}

#[test]
fn a_refused_send_leaves_the_round_unresolved_for_the_marker_to_settle() {
    let fixture = Fixture::start();
    fixture.service.refuse_next(1);

    assert!(fixture.send(&fixture.notes()).is_err());

    // A status the service chose is not proof either way, so the round stays a
    // question — and asking again is the only way to settle it.
    let round = fixture
        .store
        .rounds(&fixture.checkout.id)
        .unwrap()
        .remove(0);
    assert_eq!(fixture.status(&round.id), RoundStatus::Dispatching);
    assert_eq!(fixture.settle(&round.id).status, RoundStatus::Dispatched);
}

#[test]
fn two_windows_sending_one_review_produce_one_round_and_one_prompt() {
    let fixture = Fixture::start();

    let workers = (0..4)
        .map(|_| {
            let (store, client) = (fixture.store.clone(), Arc::clone(&fixture.client));
            let checkout = fixture.checkout.clone();
            thread::spawn(move || {
                let notes = vec![note(&checkout.id, "note-1")];
                store
                    .send_round(
                        &client,
                        RoundRequest {
                            checkout: &checkout,
                            session_id: SESSION,
                            notes: &notes,
                            review_markdown: "# Code Review",
                        },
                    )
                    .map(|round| round.id)
            })
        })
        .collect::<Vec<_>>();

    let ids = workers
        .into_iter()
        .map(|worker| worker.join().unwrap().unwrap())
        .collect::<Vec<_>>();

    assert!(
        ids.windows(2).all(|pair| pair[0] == pair[1]),
        "every window gets the same round: {ids:?}"
    );
    assert_eq!(
        fixture.service.prompt_posts().len(),
        1,
        "exactly one window may offer a review"
    );
    assert_eq!(fixture.store.rounds(&fixture.checkout.id).unwrap().len(), 1);
}

#[test]
fn a_claim_does_not_depend_on_the_order_the_notes_arrived_in() {
    let fixture = Fixture::start();
    let first = note(&fixture.checkout.id, "note-1");
    let second = note(&fixture.checkout.id, "note-2");
    let both = vec![first.clone(), second.clone()];

    let sent = fixture.send(&both).unwrap();
    let reordered = fixture.send(&[second, first]).unwrap();

    assert_eq!(reordered.id, sent.id, "one review, one round");
    assert_eq!(fixture.service.prompt_posts().len(), 1);
}

#[test]
fn a_note_that_is_already_someone_elses_review_is_not_carried_again() {
    let fixture = Fixture::start();
    let notes = vec![
        note(&fixture.checkout.id, "note-1"),
        note(&fixture.checkout.id, "note-2"),
    ];
    let sent = fixture.send(&notes).unwrap();

    // One of them is that review, unchanged; the other is new. A round cannot be the
    // mixture, because sending it would put the sent comment on the wire a second time.
    let mixed = vec![notes[0].clone(), note(&fixture.checkout.id, "note-3")];
    let error = fixture.send(&mixed).unwrap_err();
    assert!(matches!(error, DispatchError::Foreign(_)), "{error}");
    assert!(error.to_string().contains(&sent.id), "{error}");
    assert!(error.to_string().contains("note-1"), "{error}");
    assert_eq!(fixture.service.prompt_posts().len(), 1);
}

#[test]
fn a_note_that_was_edited_is_a_new_comment_and_may_be_sent_again() {
    let fixture = Fixture::start();
    let notes = fixture.notes();
    let sent = fixture.send(&notes).unwrap();
    let rounds = fixture.store.rounds(&fixture.checkout.id).unwrap();
    assert!(
        unsent_notes(&notes, &rounds).is_empty(),
        "the note as it was sent is that review"
    );

    // Every field that says what the agent would read, and nothing else: editing any of
    // them is a different review, so it may go out again.
    let edits: [fn(&mut ReviewNote); 5] = [
        |note| note.content = "check this too".into(),
        |note| note.code = "let x = 2;".into(),
        |note| note.path = "src/other.rs".into(),
        |note| note.line_start += 1,
        |note| note.line_end = Some(12),
    ];
    for edit in edits {
        let mut edited = notes[0].clone();
        edit(&mut edited);
        assert_eq!(
            unsent_notes(std::slice::from_ref(&edited), &rounds),
            vec![edited.clone()],
            "an edited note is a new comment, not the review already sent"
        );
        assert_ne!(
            fixture.send(std::slice::from_ref(&edited)).unwrap().id,
            sent.id
        );
    }
    assert_eq!(fixture.service.prompt_posts().len(), edits.len() + 1);
}

#[test]
fn a_note_that_only_drifted_is_the_review_that_was_already_delivered() {
    let fixture = Fixture::start();
    let notes = fixture.notes();
    let sent = fixture.send(&notes).unwrap();
    let rounds = fixture.store.rounds(&fixture.checkout.id).unwrap();

    // The working tree moved under the comment: it is flagged as drifted, and its anchor
    // fingerprint is recomputed. Neither says anything about what was delivered, and a
    // comment that was already sent is not sent again because the line it sits on moved.
    for drift in [
        ReviewNote {
            outdated: true,
            ..notes[0].clone()
        },
        ReviewNote {
            code_hash: review_anchor_hash("let x = 2;"),
            ..notes[0].clone()
        },
    ] {
        assert!(
            unsent_notes(std::slice::from_ref(&drift), &rounds).is_empty(),
            "drift is not a new comment: {drift:?}"
        );
        assert_eq!(
            fixture.send(std::slice::from_ref(&drift)).unwrap().id,
            sent.id,
            "asking again is that round, not a second send of the same comment"
        );
    }
    assert_eq!(fixture.service.prompt_posts().len(), 1);
}

#[test]
fn two_notes_a_field_separator_cannot_tell_apart_are_two_different_reviews() {
    let fixture = Fixture::start();
    // One note's path ends with the separator and the other's comment begins with it, so
    // a joined string spells both the same way and a digest of it cannot see them apart.
    let mut in_comment = note(&fixture.checkout.id, "note-1");
    in_comment.content = "\u{1}src/main.rs".into();
    let mut in_path = note(&fixture.checkout.id, "note-1");
    in_path.path = "src/main.rs\u{1}".into();
    in_path.content = "src/main.rs".into();
    assert_ne!(
        review_note_snapshot(&in_comment),
        review_note_snapshot(&in_path),
        "each field writes its own length, so the two cannot be spelled alike"
    );

    let sent = fixture.send(&[in_comment]).unwrap();
    let again = fixture.send(&[in_path]).unwrap();

    assert_ne!(again.id, sent.id, "a different note is a different review");
    assert_eq!(fixture.service.prompt_posts().len(), 2);
}

#[test]
fn a_note_that_could_not_be_delivered_is_refused_before_it_is_claimed() {
    let fixture = Fixture::start();
    let notes = fixture.notes();
    let undeliverable: [fn(&mut ReviewNote); 4] = [
        |note| note.content = "   ".into(),
        |note| note.content = "x".repeat(crate::review::MAX_NOTE_CONTENT_BYTES + 1),
        |note| note.code = "x".repeat(crate::review::MAX_NOTE_CODE_BYTES + 1),
        |note| note.path = "../../outside.rs".into(),
    ];
    for break_it in undeliverable {
        let mut note = notes[0].clone();
        break_it(&mut note);
        let error = fixture.send(&[note]).unwrap_err();
        assert!(matches!(error, DispatchError::Failed(_)), "{error}");
    }
    assert!(fixture
        .store
        .rounds(&fixture.checkout.id)
        .unwrap()
        .is_empty());
    assert!(fixture.service.prompt_posts().is_empty());
}

#[test]
fn rounds_recorded_at_the_same_moment_are_all_kept() {
    let fixture = Fixture::start();
    let workers = (0..8)
        .map(|index| {
            let (store, client) = (fixture.store.clone(), Arc::clone(&fixture.client));
            let checkout = fixture.checkout.clone();
            thread::spawn(move || {
                let notes = vec![note(&checkout.id, &format!("note-{index}"))];
                store
                    .send_round(
                        &client,
                        RoundRequest {
                            checkout: &checkout,
                            session_id: SESSION,
                            notes: &notes,
                            review_markdown: "# Code Review",
                        },
                    )
                    .map(|round| round.id)
            })
        })
        .collect::<Vec<_>>();

    let ids = workers
        .into_iter()
        .map(|worker| worker.join().unwrap().unwrap())
        .collect::<Vec<_>>();

    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "round ids must not collide");
    assert_eq!(
        fixture.store.rounds(&fixture.checkout.id).unwrap().len(),
        ids.len()
    );
    assert_eq!(fixture.service.prompt_posts().len(), ids.len());
}

#[test]
fn the_destination_is_remembered_per_checkout() {
    let fixture = Fixture::start();
    let other_path = fixture.home.path().join("other");
    fs::create_dir_all(&other_path).unwrap();
    let other = Repo::plain(&other_path, "now").unwrap().checkouts[0].clone();

    fixture
        .store
        .set_destination(&other.id, Destination::Markdown)
        .unwrap();
    fixture
        .store
        .set_destination(
            &fixture.checkout.id,
            Destination::Opencode {
                session_id: SESSION.into(),
            },
        )
        .unwrap();

    assert_eq!(
        fixture.store.destination(&fixture.checkout.id).unwrap(),
        Some(Destination::Opencode {
            session_id: SESSION.into()
        })
    );
    assert_eq!(
        fixture.store.destination(&other.id).unwrap(),
        Some(Destination::Markdown)
    );
    assert_eq!(fixture.store.destination(ELSEWHERE).unwrap(), None);
    fixture
        .store
        .set_destination(&other.id, Destination::OpencodeNew)
        .unwrap();
    let reopened = RoundStore::open_in(fixture.store.path().parent().unwrap()).unwrap();
    assert_eq!(
        reopened.destination(&other.id).unwrap(),
        Some(Destination::OpencodeNew)
    );
    assert!(reopened.rounds(&other.id).unwrap().is_empty());
    assert!(
        fixture.service.prompt_posts().is_empty(),
        "choosing a target must never send"
    );
}

#[test]
fn a_store_that_is_not_this_apps_file_is_refused_rather_than_read() {
    let fixture = Fixture::start();
    let path = fixture.store.path().to_path_buf();
    let refused =
        |error: DispatchError| assert!(matches!(error, DispatchError::Foreign(_)), "{error:?}");

    // An older schema, and a file with a field this build does not know.
    fs::write(
        &path,
        br#"{"schemaVersion":0,"rounds":[],"destinations":{}}"#,
    )
    .unwrap();
    refused(fixture.store.rounds(&fixture.checkout.id).unwrap_err());
    // A store whose claim was a digest rather than the notes' own canonical string is
    // refused rather than read: two different reviews could have been spelled the same.
    fs::write(
        &path,
        br#"{"schemaVersion":1,"rounds":[],"destinations":{}}"#,
    )
    .unwrap();
    refused(fixture.store.rounds(&fixture.checkout.id).unwrap_err());
    fs::write(
        &path,
        br#"{"schemaVersion":2,"rounds":[],"destinations":{},"sessions":[]}"#,
    )
    .unwrap();
    refused(fixture.store.rounds(&fixture.checkout.id).unwrap_err());
    fs::write(&path, b"not json").unwrap();
    refused(fixture.store.rounds(&fixture.checkout.id).unwrap_err());

    // A store that is a link is not this app's state, and nothing follows it.
    let target = fixture.home.path().join("target");
    fs::write(&target, b"untouched").unwrap();
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(fixture.store.rounds(&fixture.checkout.id).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"untouched");
}

#[test]
fn a_settled_round_is_never_reopened_by_a_reconnect() {
    let fixture = Fixture::start();
    let round = fixture.send(&fixture.notes()).unwrap();
    assert_eq!(fixture.status(&round.id), RoundStatus::Dispatched);

    // A service that lost its transcript must not put a round the service already
    // answered for back in the queue.
    fixture.service.transcript.lock().unwrap().clear();
    assert_eq!(fixture.settle(&round.id).status, RoundStatus::Dispatched);
    assert_eq!(
        fixture
            .store
            .flush_rounds(&fixture.client, &fixture.checkout.id)
            .unwrap(),
        0
    );
    assert_eq!(fixture.service.prompt_posts().len(), 1);
}

#[test]
fn an_acked_round_is_finished_and_a_round_only_moves_forward() {
    let fixture = Fixture::start();
    fixture.service.swallow_next(1);
    fixture.send(&fixture.notes()).unwrap_err();
    let round = fixture
        .store
        .rounds(&fixture.checkout.id)
        .unwrap()
        .remove(0);
    assert_eq!(fixture.settle(&round.id).status, RoundStatus::Unknown);

    assert!(
        matches!(
            fixture
                .store
                .ack_round(&fixture.checkout.id, &round.id)
                .unwrap_err(),
            DispatchError::Failed(_)
        ),
        "a round that was never confirmed cannot be acked"
    );
    assert!(fixture.store.rounds(ELSEWHERE).unwrap().is_empty());
    assert!(matches!(
        fixture
            .store
            .resend_round(ELSEWHERE, &round.id)
            .unwrap_err(),
        DispatchError::Foreign(_)
    ));
    assert!(matches!(
        fixture
            .store
            .resend_round(&fixture.checkout.id, &round.id)
            .unwrap()
            .status,
        RoundStatus::Queued
    ));

    // And once the service has confirmed it, the round is closed and cannot be re-offered.
    fixture.service.swallow_next(0);
    let settled = fixture
        .store
        .flush_rounds(&fixture.client, &fixture.checkout.id)
        .unwrap();
    assert_eq!(settled, 1);
    let round = fixture
        .store
        .rounds(&fixture.checkout.id)
        .unwrap()
        .remove(0);
    assert_eq!(
        fixture
            .store
            .ack_round(&fixture.checkout.id, &round.id)
            .unwrap()
            .status,
        RoundStatus::Acked
    );
    assert!(matches!(
        fixture
            .store
            .resend_round(&fixture.checkout.id, &round.id)
            .unwrap_err(),
        DispatchError::Failed(_)
    ));
}
