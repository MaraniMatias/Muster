//! The one place this window's navigation state reaches the disk.
//!
//! ## Why a queue was the wrong shape
//!
//! The writer used to be an unbounded channel of snapshots. It was a queue of
//! *answers*: every divider drag, every selection and every Git sweep enqueued
//! another whole workspace document, and the thread drained them one at a time,
//! long after they stopped being true. A person who dragged the sidebar width
//! produced dozens of states, all but the last of which described a width that
//! no longer existed.
//!
//! ## What it is instead
//!
//! One slot holding the latest accepted snapshot. A new state replaces the one
//! waiting; a state equal to the one waiting is not a new answer at all and is
//! dropped before it is stored. What the thread writes is therefore always the
//! most recent thing the window knew, and how fast the window changes no longer
//! decides how much is queued.
//!
//! ## What a shutdown owes the disk
//!
//! The last accepted state, even if it arrived a millisecond before the
//! process ends. [`StateWriter::drop`] closes the slot and joins the thread,
//! and the thread attempts the pending save before it returns. Failures remain
//! available to the window and are logged, including during the final flush.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

use muster_core::navigation::{self, NavigationState};

/// The signature the thread saves through.
///
/// A value rather than a direct call so the slot's own behaviour — what it
/// keeps, what it drops, what it flushes — can be tested against a writer that
/// is slow or that fails, which a real file is not reliably either.
type Save = Arc<dyn Fn(&Path, &NavigationState) -> Result<(), String> + Send + Sync>;

/// The slot, and the switch that ends it.
struct Slot {
    state: Mutex<Pending>,
    closed: AtomicBool,
    ready: Condvar,
}

#[derive(Default)]
struct Pending {
    latest: Option<NavigationState>,
    dirty: bool,
}

impl Slot {
    /// Store a state, or keep the one already waiting if they are the same.
    ///
    /// `Err` once the slot is closed: a state nobody will read is not a state
    /// that was saved, and the caller has to hear that rather than assume.
    fn offer(&self, state: NavigationState) -> Result<(), ()> {
        let mut held = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if self.closed.load(Ordering::Acquire) {
            return Err(());
        }
        if held.latest.as_ref() == Some(&state) {
            // This document was already accepted, whether pending or written.
            return Ok(());
        }
        held.latest = Some(state);
        held.dirty = true;
        self.ready.notify_one();
        Ok(())
    }

    /// Take the newest state, waiting for one while the slot is open.
    ///
    /// The slot is checked before the switch, so a state that arrived before
    /// the close is still drained after it.
    fn take(&self) -> Option<NavigationState> {
        let mut held = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if held.dirty {
                held.dirty = false;
                return held.latest.clone();
            }
            if self.closed.load(Ordering::Acquire) {
                return None;
            }
            held = self
                .ready
                .wait(held)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn failed(&self, state: &NavigationState) {
        let mut held = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        // Do not discard a newer pending offer, even if it returned to the
        // same value while this write was in flight. No automatic retry.
        if !held.dirty && held.latest.as_ref() == Some(state) {
            held.latest = None;
        }
    }

    fn close(&self) {
        // Under the same lock `offer` takes, so nothing can slip into the slot
        // between the thread's last look and the end of the wait.
        let _held = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.closed.store(true, Ordering::Release);
        self.ready.notify_all();
    }
}

/// One serial writer for one window.
pub struct StateWriter {
    path: PathBuf,
    slot: Arc<Slot>,
    error: Arc<Mutex<Option<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl StateWriter {
    /// A writer that saves through [`navigation::save`].
    pub fn new(path: PathBuf) -> Result<Self, String> {
        Self::saving(path, Arc::new(navigation::save))
    }

    /// A writer that saves through `save`.
    ///
    /// Private, and deliberately so: `save` is a seam for this module's own
    /// tests, not a way for a host to redirect the workspace file somewhere
    /// else. Production gets `new` and the core's own validator.
    fn saving(path: PathBuf, save: Save) -> Result<Self, String> {
        let slot = Arc::new(Slot {
            state: Mutex::new(Pending::default()),
            closed: AtomicBool::new(false),
            ready: Condvar::new(),
        });
        let error = Arc::new(Mutex::new(None));
        let failures = Arc::clone(&error);
        let worker = Arc::clone(&slot);
        let owned = path.clone();
        let thread = thread::Builder::new()
            .name("muster-navigation-save".into())
            .spawn(move || {
                while let Some(state) = worker.take() {
                    if let Err(cause) = save(&owned, &state) {
                        worker.failed(&state);
                        // A final flush failure may happen after the banner is gone.
                        log::error!("navigation state save failed: {cause}");
                        // The newest failure is the one the person can act on;
                        // an older one a later write has overtaken is not.
                        *failures.lock().unwrap_or_else(PoisonError::into_inner) = Some(cause);
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            path,
            slot,
            error,
            thread: Some(thread),
        })
    }

    /// The file this writer owns.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Hand the window's newest state to the writer.
    ///
    /// Never blocks on the disk; the pending state replaces older drag events.
    pub fn send(&self, state: NavigationState) {
        if self.slot.offer(state).is_err() {
            self.fail("the navigation writer is already stopped");
        }
    }

    /// The failure the last save reported, taken once so the window can show
    /// it and stop asking about it on every tick.
    pub fn take_error(&self) -> Option<String> {
        self.error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn fail(&self, message: impl Into<String>) {
        *self.error.lock().unwrap_or_else(PoisonError::into_inner) = Some(message.into());
    }
}

impl Drop for StateWriter {
    fn drop(&mut self) {
        // Close first, so the thread's next wake drains what is left and then
        // returns, rather than waiting for a state that will never come.
        self.slot.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    /// A snapshot that differs from every other one, so "the last one" is
    /// answerable without comparing whole documents.
    fn state(width: f32) -> NavigationState {
        NavigationState {
            sidebar_width: width,
            ..NavigationState::default()
        }
    }

    /// A writer that records what it was asked to write.
    ///
    /// `slow` makes it the kind the window can outrun, which is the only kind
    /// the replacement has to be right about.
    fn recording(slow: bool) -> (Save, Arc<Mutex<Vec<f32>>>, Arc<AtomicUsize>) {
        let written = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let (sink, count) = (Arc::clone(&written), Arc::clone(&calls));
        let save: Save = Arc::new(move |_, snapshot| {
            count.fetch_add(1, Ordering::SeqCst);
            if slow {
                thread::sleep(Duration::from_millis(2));
            }
            sink.lock().unwrap().push(snapshot.sidebar_width);
            Ok(())
        });
        (save, written, calls)
    }

    fn wait_for(written: &Mutex<Vec<f32>>, wanted: f32) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if written.lock().unwrap().contains(&wanted) {
                return true;
            }
            thread::sleep(Duration::from_millis(1));
        }
        false
    }

    fn wait_for_error(writer: &StateWriter) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(error) = writer.take_error() {
                return error;
            }
            assert!(
                Instant::now() < deadline,
                "writer did not report its failure"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn an_identical_offer_after_a_failed_save_retries_and_flushes_on_shutdown() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("native-workspace.json");
        let calls = Arc::new(AtomicUsize::new(0));
        let attempts = calls.clone();
        let save: Save = Arc::new(move |path, snapshot| {
            if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                Err("transient disk error".into())
            } else {
                navigation::save(path, snapshot)
            }
        });
        let writer = StateWriter::saving(path.clone(), save).unwrap();
        writer.send(state(300.));
        assert_eq!(wait_for_error(&writer), "transient disk error");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "failure must not auto-retry"
        );
        writer.send(state(300.));
        writer.send(state(300.));
        drop(writer);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(navigation::load(&path).unwrap(), state(300.));
    }

    #[test]
    fn a_failed_in_flight_save_preserves_the_latest_pending_offer_during_shutdown() {
        use std::sync::mpsc;

        // Also cover A -> B -> A: equality with the failed write does not make
        // the pending A any less recent than B.
        for latest in [301., 300.] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("native-workspace.json");
            let (entered, started) = mpsc::channel();
            let (release, released) = mpsc::channel();
            let released = Mutex::new(released);
            let written = Arc::new(Mutex::new(Vec::new()));
            let attempts = written.clone();
            let save: Save = Arc::new(move |path, snapshot| {
                let first = {
                    let mut attempts = attempts.lock().unwrap();
                    attempts.push(snapshot.sidebar_width);
                    attempts.len() == 1
                };
                if first {
                    entered.send(()).unwrap();
                    released
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap();
                    Err("transient disk error".into())
                } else {
                    navigation::save(path, snapshot)
                }
            });
            let writer = StateWriter::saving(path.clone(), save).unwrap();
            writer.send(state(300.));
            started.recv_timeout(Duration::from_secs(5)).unwrap();
            writer.send(state(300.));
            assert!(
                !writer.slot.state.lock().unwrap().dirty,
                "in-flight offer must coalesce"
            );
            writer.send(state(302.));
            writer.send(state(latest));
            writer.send(state(latest));
            writer.slot.close();
            release.send(()).unwrap();
            assert_eq!(wait_for_error(&writer), "transient disk error");
            drop(writer);
            assert_eq!(*written.lock().unwrap(), vec![300., latest]);
            assert_eq!(navigation::load(&path).unwrap(), state(latest));
        }
    }

    #[test]
    fn a_burst_against_a_slow_writer_leaves_the_last_snapshot_on_disk() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("native-workspace.json");
        let (save, written, _) = recording(true);
        {
            let writer = StateWriter::saving(path.clone(), save).unwrap();
            for step in 0..200 {
                writer.send(state(step as f32));
            }
            // Dropping here is the shutdown: whatever is still in the slot has
            // to be written before the window is gone.
        }
        assert_eq!(written.lock().unwrap().last(), Some(&199.));
    }

    #[test]
    fn an_unchanged_snapshot_is_not_written_again() {
        let (save, written, calls) = recording(false);
        let writer = StateWriter::saving(PathBuf::from("/dev/null"), save).unwrap();
        for _ in 0..50 {
            writer.send(state(240.));
        }
        assert!(wait_for(&written, 240.));
        // Equality also suppresses offers after the worker has drained the slot.
        writer.send(state(240.));
        drop(writer);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "one unchanged state is one write, not one per caller"
        );
    }

    #[test]
    fn the_last_accepted_state_is_flushed_when_the_writer_stops() {
        let (save, written, _) = recording(true);
        let writer = StateWriter::saving(PathBuf::from("/dev/null"), save).unwrap();
        writer.send(state(301.));
        // No wait: the flush has to come from the shutdown, not from a poll.
        drop(writer);
        assert_eq!(*written.lock().unwrap(), vec![301.]);
    }

    #[test]
    fn a_state_offered_after_the_shutdown_is_refused_rather_than_kept() {
        let slot = Arc::new(Slot {
            state: Mutex::new(Pending::default()),
            closed: AtomicBool::new(false),
            ready: Condvar::new(),
        });
        slot.close();
        assert!(slot.offer(state(1.)).is_err());
        assert!(slot.take().is_none(), "a closed slot keeps nothing");
    }

    #[test]
    fn a_state_offered_before_the_shutdown_is_still_drained_by_it() {
        let slot = Arc::new(Slot {
            state: Mutex::new(Pending::default()),
            closed: AtomicBool::new(false),
            ready: Condvar::new(),
        });
        slot.offer(state(7.)).expect("an open slot accepts");
        slot.close();
        assert_eq!(slot.take().map(|held| held.sidebar_width), Some(7.));
        assert!(slot.take().is_none());
    }

    #[test]
    fn a_failed_save_is_retained_for_the_window_and_then_released() {
        let save: Save = Arc::new(|_, _| Err("disk is full".into()));
        let writer = StateWriter::saving(PathBuf::from("/dev/null"), save).unwrap();
        writer.send(state(240.));
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen = None;
        while seen.is_none() && Instant::now() < deadline {
            seen = writer.take_error();
            if seen.is_none() {
                thread::sleep(Duration::from_millis(1));
            }
        }
        assert_eq!(seen.as_deref(), Some("disk is full"));
        assert_eq!(
            writer.take_error(),
            None,
            "the window is shown the failure once, not on every tick"
        );
    }

    #[test]
    fn the_production_writer_does_not_overwrite_an_older_workspace_schema() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("native-workspace.json");
        let old = br#"{"schemaVersion":1,"roots":["/work/repo"],"archivedCheckoutIds":[],"activeCheckoutId":null,"sidebarWidth":240.0}"#;
        std::fs::write(&path, old).unwrap();
        let writer = StateWriter::new(path.clone()).unwrap();
        writer.send(state(320.));
        assert!(wait_for_error(&writer).contains("schema 1 is unsupported"));
        drop(writer);
        assert_eq!(std::fs::read(&path).unwrap(), old);
    }

    #[test]
    fn the_production_writer_saves_through_the_core_state_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("native-workspace.json");
        let writer = StateWriter::new(path.clone()).unwrap();
        assert_eq!(writer.path(), path);
        writer.send(state(321.5));
        drop(writer);
        assert_eq!(navigation::load(&path).unwrap().sidebar_width, 321.5);
    }
}
