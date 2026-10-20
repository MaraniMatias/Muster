//! The bounded, serialized queue between "these bytes want to go to the PTY"
//! and the backend's `write`.
//!
//! The backend refuses a second concurrent writer outright -- one write at a
//! time per session -- and its own bound is on a *single* write, not on what a
//! child has been handed in a second. Both halves of that are here: one writer
//! thread, so the refusal can never be provoked by this crate's own callers,
//! and one budget that everything shares, so a paste, a protocol reply and a
//! burst of held-down keys cannot add up to more than the terminal can take.
//!
//! Bytes leave the budget when they leave the queue rather than when they reach
//! the PTY: the backend bounds an in-flight write itself, and a thread that
//! counted bytes it was already writing would refuse input for work it had
//! already done.
//!
//! No GPUI, no terminal, no clock. The sink is a closure so a test can watch
//! exactly what was written and when.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

/// Total queued bytes across every request, whatever produced it.
///
/// The same 1 MiB the backend allows for one write, so a queue that is empty
/// can always hand the whole budget to a single request.
pub const MAX_QUEUED_BYTES: usize = 1024 * 1024;

/// Queued requests, whatever produced them.
///
/// A byte budget alone is not a bound on memory: a hundred thousand empty
/// requests is a hundred thousand allocations. 128 is far past anything a
/// person can press and far short of being the thing that runs the terminal
/// out of memory.
pub const MAX_QUEUED_REQUESTS: usize = 128;

/// Why a request was not queued, with the numbers needed to say why.
///
/// Every variant carries enough to name the limit that was hit and how far the
/// caller was, because "input queue full" is not an error anybody can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueError {
    /// The request would push the queue past [`MAX_QUEUED_BYTES`].
    TooManyBytes {
        queued: usize,
        incoming: usize,
        limit: usize,
    },
    /// The queue already holds [`MAX_QUEUED_REQUESTS`] requests.
    TooManyRequests { queued: usize, limit: usize },
    /// The queue has been stopped, so a request taken now could never be
    /// written. Reported rather than swallowed: a caller pushing into a stopped
    /// queue is a caller that will wait for a reply that is not coming.
    Stopped,
}

/// Bytes on their way to a child, in order, one writer at a time.
///
/// Cheap to push to from anywhere -- the window pushes keys, the PTY reader
/// pushes protocol replies, and neither waits on the other -- and expensive to
/// be, because the writing happens on this crate's own thread.
pub struct WriteQueue {
    shared: Arc<Shared>,
    writer: Mutex<Option<JoinHandle<()>>>,
}

struct Shared {
    state: Mutex<QueueState>,
    /// Signals "there is something to do" rather than "something arrived", so a
    /// push that only hands work to another push does not wake the writer for
    /// no reason.
    ready: Condvar,
}

#[derive(Default)]
struct QueueState {
    requests: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    stopping: bool,
}

impl WriteQueue {
    /// Start the writer thread and hand it `sink`.
    ///
    /// `sink` is called once per request, in the order requests were pushed,
    /// never concurrently with itself, off the caller's thread. Its `Result` is
    /// the backend's own: `Err` is a session that is closing or a write that
    /// timed out, and it is logged rather than retried, because a retry would
    /// reorder the queue -- the request after the failed one would go in first.
    ///
    /// `sink` runs on the writer thread, so it must not push to this queue or
    /// stop it; a closure that did either would be waiting on itself.
    pub fn spawn<S>(mut sink: S) -> std::io::Result<Self>
    where
        S: FnMut(&[u8]) -> Result<(), String> + Send + 'static,
    {
        let shared = Arc::new(Shared {
            state: Mutex::new(QueueState::default()),
            ready: Condvar::new(),
        });
        let writer = Arc::clone(&shared);

        let handle = thread::Builder::new()
            .name("terminal-write-queue".to_owned())
            .spawn(move || writer_loop(&mut sink, &writer))?;

        Ok(Self {
            shared,
            writer: Mutex::new(Some(handle)),
        })
    }

    /// Queue one request, or refuse it whole.
    ///
    /// Both budgets are checked before any of the state below them moves, so a
    /// refusal leaves the queue exactly as it found it: whatever was already in
    /// it is still delivered, which is the whole point of refusing rather than
    /// dropping.
    pub fn push(&self, request: Vec<u8>) -> Result<(), EnqueueError> {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("terminal write queue poisoned");

        if state.stopping {
            return Err(EnqueueError::Stopped);
        }

        // `request.len()`, never `request.chars().count()`: the budget is what
        // the PTY has to hold, and what the caller spent building.
        let queued_bytes = state.queued_bytes + request.len();

        if state.requests.len() >= MAX_QUEUED_REQUESTS {
            return Err(EnqueueError::TooManyRequests {
                queued: state.requests.len(),
                limit: MAX_QUEUED_REQUESTS,
            });
        }
        if queued_bytes > MAX_QUEUED_BYTES {
            return Err(EnqueueError::TooManyBytes {
                queued: state.queued_bytes,
                incoming: request.len(),
                limit: MAX_QUEUED_BYTES,
            });
        }

        state.queued_bytes = queued_bytes;
        state.requests.push_back(request);
        self.shared.ready.notify_one();

        Ok(())
    }

    /// Queued bytes not yet handed to the sink.
    pub fn queued_bytes(&self) -> usize {
        self.shared
            .state
            .lock()
            .expect("terminal write queue poisoned")
            .queued_bytes
    }

    /// Queued requests not yet handed to the sink.
    pub fn queued_requests(&self) -> usize {
        self.shared
            .state
            .lock()
            .expect("terminal write queue poisoned")
            .requests
            .len()
    }

    /// Stop the writer thread and wait for it.
    ///
    /// Whatever is still queued is dropped rather than written: the caller has
    /// said the session is over, and a `stop` that drained first would hang
    /// behind a child that has stopped reading. Safe to call more than once,
    /// and called for you on drop.
    pub fn stop(&self) {
        let handle = self
            .writer
            .lock()
            .expect("terminal write queue poisoned")
            .take();
        let Some(handle) = handle else { return };

        {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("terminal write queue poisoned");
            state.stopping = true;
            self.shared.ready.notify_all();
        }

        // Outside the state lock: the writer takes that lock to notice it was
        // asked to stop, and joining under it would be the deadlock this line
        // is placed to avoid.
        if handle.join().is_err() {
            log::warn!("terminal write queue thread panicked");
        }
    }
}

impl Drop for WriteQueue {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Take requests off the queue in order and write them, until asked to stop.
fn writer_loop<S>(sink: &mut S, shared: &Shared)
where
    S: FnMut(&[u8]) -> Result<(), String>,
{
    loop {
        let request = {
            let mut state = shared.state.lock().expect("terminal write queue poisoned");
            loop {
                // Stopping wins over anything still queued: the queue is the
                // writer's to close, and a request that arrived during the stop
                // has nowhere left to go.
                if state.stopping {
                    // The books are closed on what is left here rather than at
                    // the end of the queue's life: a queue still claiming bytes
                    // it is never going to deliver is a queue whose budget is
                    // wrong for everyone reading it.
                    state.requests.clear();
                    state.queued_bytes = 0;
                    return;
                }
                if let Some(request) = state.requests.pop_front() {
                    state.queued_bytes -= request.len();
                    break request;
                }
                state = shared
                    .ready
                    .wait(state)
                    .expect("terminal write queue poisoned");
            }
        };

        // An empty request is an empty paste, and a zero-length write to the
        // PTY is a syscall that cannot do what the caller wanted.
        if request.is_empty() {
            continue;
        }

        if let Err(error) = sink(&request) {
            // Once per failed request, and the queue keeps draining: a session
            // that is closing answers every one of these, and stopping here
            // would leave the requests behind it undelivered for a session that
            // might only have failed one write.
            log::warn!("terminal write of {} bytes failed: {error}", request.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::Duration;

    use super::*;

    /// How long a test waits for the writer to reach it. Generous, because a
    /// CI machine is slow and a test that fails on a slow machine is a test
    /// about scheduling rather than about the queue.
    const PATIENCE: Duration = Duration::from_secs(5);

    /// How long the sink below stays inside one write without a token.
    ///
    /// Not part of the contract, only a guard: a test that fails while the
    /// writer is held would otherwise panic with the sink still waiting, and
    /// dropping the queue would join a thread that is waiting on that test's
    /// own channel -- a failed assertion reported as a hung suite.
    const HELD: Duration = Duration::from_secs(30);

    /// Every request a sink was handed, shared with the test that is watching.
    type Delivered = Arc<Mutex<Vec<Vec<u8>>>>;

    /// What `WriteQueue::spawn` is given.
    type Sink = Box<dyn FnMut(&[u8]) -> Result<(), String> + Send>;

    /// What a sink records, and the two ends of the gate that holds it.
    struct GatedSink {
        /// Taken once, by `spawn`.
        sink: Option<Sink>,
        /// One token per request the sink has been given.
        entered: Receiver<()>,
        /// `None` once the writer has been let go for good.
        release: Option<Sender<()>>,
        delivered: Delivered,
    }

    /// A sink that records what it was given and then blocks until the test
    /// lets it go.
    ///
    /// That is the only way to fill a queue that has a writer on it: without a
    /// way to hold the writer inside a write, every request below would be
    /// drained before the next one arrived.
    fn gated_sink() -> GatedSink {
        let (on_entered, entered) = mpsc::channel();
        let (release, on_release) = mpsc::channel();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);

        let sink = move |request: &[u8]| {
            recorder
                .lock()
                .expect("sink recorder poisoned")
                .push(request.to_vec());
            // Recorded first, then announced: one token is one request the
            // writer has taken off the queue and handed over, not one it has
            // promised to write.
            let _ = on_entered.send(());
            // Dropping the sender ends the wait for good, which is how a test
            // lets the queue drain instead of releasing request by request.
            let _ = on_release.recv_timeout(HELD);
            Ok(())
        };

        GatedSink {
            sink: Some(Box::new(sink)),
            entered,
            release: Some(release),
            delivered: seen,
        }
    }

    impl GatedSink {
        /// Hand the sink to a queue and keep the gate.
        fn spawn(&mut self) -> WriteQueue {
            WriteQueue::spawn(self.sink.take().expect("a sink is only taken once"))
                .expect("writer starts")
        }

        /// Let the writer go for good.
        ///
        /// A method rather than a field the test drops, because dropping the
        /// field would move half the struct and leave the rest of it
        /// unusable.
        fn release(&mut self) {
            self.release = None;
        }
    }

    fn what_the_sink_saw(gated: &GatedSink) -> Vec<Vec<u8>> {
        gated
            .delivered
            .lock()
            .expect("sink recorder poisoned")
            .clone()
    }

    /// Wait for `count` requests to have reached the sink.
    fn expect_delivered(entered: &Receiver<()>, count: usize) {
        for request in 0..count {
            entered.recv_timeout(PATIENCE).unwrap_or_else(|error| {
                panic!("request {request} never reached the sink: {error}")
            });
        }
    }

    #[test]
    fn the_write_queue_refuses_input_past_its_budget() {
        // One request too big, counted in bytes and counted at four bytes per
        // character.
        {
            let mut gated = gated_sink();
            let queue = gated.spawn();

            // Held inside the sink for the rest of the block, so everything
            // measured below is measured against a queue that cannot drain
            // itself out from under the measurement.
            queue
                .push(b"first".to_vec())
                .expect("a small request is queued");
            gated
                .entered
                .recv_timeout(PATIENCE)
                .expect("the writer takes the first request");

            let oversized = vec![b'x'; MAX_QUEUED_BYTES + 1];
            assert_eq!(
                queue.push(oversized.clone()),
                Err(EnqueueError::TooManyBytes {
                    queued: 0,
                    incoming: MAX_QUEUED_BYTES + 1,
                    limit: MAX_QUEUED_BYTES,
                }),
                "one request over the byte budget is refused whole"
            );
            // Nothing about the refusal is allowed to reach the sink.
            assert_eq!(queue.queued_requests(), 0);
            assert_eq!(queue.queued_bytes(), 0);

            // Exactly at the budget is not over it, and it is reached with
            // four-byte characters: a budget counted in characters would report
            // a quarter of this size and the refusal above would be a quarter
            // as hard to reach.
            let at_budget = "\u{1f600}".repeat(MAX_QUEUED_BYTES / 4);
            assert_eq!(at_budget.len(), MAX_QUEUED_BYTES);
            queue
                .push(at_budget.clone().into_bytes())
                .expect("exactly the budget is within it");
            assert_eq!(queue.queued_bytes(), MAX_QUEUED_BYTES);

            assert_eq!(
                queue.push("\u{1f600}".as_bytes().to_vec()),
                Err(EnqueueError::TooManyBytes {
                    queued: MAX_QUEUED_BYTES,
                    incoming: 4,
                    limit: MAX_QUEUED_BYTES,
                }),
                "one character over the budget is four bytes over it, not one character over it"
            );

            // Both refusals above left the queue alone, so the request that was
            // already in it is still there and still goes out.
            assert_eq!(queue.queued_requests(), 1);
            gated.release();
            expect_delivered(&gated.entered, 1);
            assert_eq!(
                what_the_sink_saw(&gated),
                vec![b"first".to_vec(), at_budget.into_bytes()]
            );

            queue.stop();
            assert_eq!(
                queue.queued_bytes(),
                0,
                "everything handed over is off the books"
            );
        }

        // Requests, counted separately, on a queue whose bytes are not already
        // full -- otherwise both refusals would be the same one.
        {
            let mut gated = gated_sink();
            let queue = gated.spawn();

            queue
                .push(b"first".to_vec())
                .expect("a small request is queued");
            gated
                .entered
                .recv_timeout(PATIENCE)
                .expect("the writer takes the first request");

            for request in 0..MAX_QUEUED_REQUESTS {
                queue
                    .push(vec![b'x'; request % 251 + 1])
                    .unwrap_or_else(|error| {
                        panic!("request {request} of the budget should be queued: {error:?}")
                    });
            }
            assert_eq!(queue.queued_requests(), MAX_QUEUED_REQUESTS);

            assert_eq!(
                queue.push(b"one too many".to_vec()),
                Err(EnqueueError::TooManyRequests {
                    queued: MAX_QUEUED_REQUESTS,
                    limit: MAX_QUEUED_REQUESTS
                })
            );

            // The refusal moved nothing: every request that was queued is still
            // queued, and the one that was refused is the only one that never
            // reaches the sink.
            assert_eq!(queue.queued_requests(), MAX_QUEUED_REQUESTS);

            gated.release();
            expect_delivered(&gated.entered, MAX_QUEUED_REQUESTS);

            let delivered = what_the_sink_saw(&gated);
            assert_eq!(delivered.len(), MAX_QUEUED_REQUESTS + 1);
            assert_eq!(
                delivered.first().expect("the held request came first"),
                b"first"
            );

            // Still accepting once it has room: a refusal is not the queue
            // giving up, and the request taken now is delivered like any other.
            queue
                .push(b"after the refusal".to_vec())
                .expect("the queue still accepts input");
            expect_delivered(&gated.entered, 1);
            assert_eq!(
                what_the_sink_saw(&gated).last().expect("the last request"),
                b"after the refusal"
            );

            queue.stop();
        }
    }

    #[test]
    fn a_queue_is_written_in_the_order_it_was_pushed() {
        let mut gated = gated_sink();
        let queue = gated.spawn();

        // Pushed while the writer is held, so nothing can overtake anything.
        queue.push(b"first".to_vec()).expect("queued");
        gated
            .entered
            .recv_timeout(PATIENCE)
            .expect("the writer takes the first request");
        for request in [&b"second"[..], b"third", b"fourth"] {
            queue.push(request.to_vec()).expect("queued");
        }

        gated.release();
        expect_delivered(&gated.entered, 3);

        // The child reads this as one stream: a reordering here is a paste that
        // runs its own first line last.
        assert_eq!(
            what_the_sink_saw(&gated),
            vec![
                b"first".to_vec(),
                b"second".to_vec(),
                b"third".to_vec(),
                b"fourth".to_vec()
            ]
        );
    }

    #[test]
    fn an_empty_request_is_queued_but_never_handed_to_the_sink() {
        let mut gated = gated_sink();
        let queue = gated.spawn();

        // An empty paste encodes to no bytes at all, and a zero-length write to
        // the PTY is a syscall that cannot do anything anybody wanted.
        queue
            .push(Vec::new())
            .expect("an empty request is not a refusal");
        queue.push(b"real".to_vec()).expect("queued");

        gated.release();
        expect_delivered(&gated.entered, 1);

        assert_eq!(what_the_sink_saw(&gated), vec![b"real".to_vec()]);
    }

    #[test]
    fn a_stopped_queue_refuses_input_rather_than_taking_it() {
        let mut gated = gated_sink();
        let queue = gated.spawn();

        queue.push(b"first".to_vec()).expect("queued");
        gated
            .entered
            .recv_timeout(PATIENCE)
            .expect("the writer takes the first request");
        queue.push(b"still queued".to_vec()).expect("queued");

        // Released before the stop, because the writer is inside the sink and
        // the stop joins it. A stop that drained first would wait behind a child
        // that has stopped reading, which is why the remainder is dropped.
        gated.release();
        queue.stop();

        // The books are closed on whatever was left behind: a queue that still
        // claimed bytes it was never going to deliver would hand a caller a
        // budget that is wrong.
        assert_eq!(queue.queued_bytes(), 0);
        assert_eq!(queue.queued_requests(), 0);

        // A request taken now cannot be written by anything, so it is refused
        // rather than accepted and lost.
        assert_eq!(
            queue.push(b"after the stop".to_vec()),
            Err(EnqueueError::Stopped)
        );

        // Stopping twice is what dropping the queue does, and it is not a panic.
        queue.stop();

        assert_eq!(
            what_the_sink_saw(&gated).first().map(Vec::as_slice),
            Some(b"first".as_slice())
        );
    }
}
