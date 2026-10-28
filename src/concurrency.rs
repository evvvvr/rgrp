use crossbeam_queue::SegQueue;
use regex::bytes::Regex;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use crate::printer::Printer;
use crate::search::{Error as SearchError, Options as SearchOptions, WorkQueue, search};

/// The state every worker thread shares while walking a search: the queue
/// of paths still to process, and the bookkeeping that lets workers agree
/// on when there's truly nothing left to do.
pub struct SharedWorkState {
    /// Paths waiting to be searched. A directory expands into its entries
    /// here rather than being recursed into directly, which is what lets
    /// different workers pick up different parts of the same walk.
    pub work_queue: SegQueue<PathBuf>,
    /// How many paths are currently being processed (popped, but not yet
    /// finished). Needed so a worker that finds the queue empty can tell
    /// "we're done" apart from "someone's mid-directory and about to push
    /// more work".
    pub pending_work_counter: AtomicUsize,
    /// Wakes workers that are waiting for new work or for everyone to finish.
    pub condition: Condvar,
    /// Paired with `condition`; holds no real data of its own.
    pub condition_guard: Mutex<()>,
    /// Set once (by a Ctrl-C handler) to ask every worker to stop picking
    /// up new work - whatever each worker is already in the middle of
    /// still runs to completion (so its output is still reported and its
    /// spawned external processes still get reaped), but none of them
    /// pop another path off the queue afterward.
    pub cancelled: AtomicBool,
}

impl SharedWorkState {
    /// Seeds the work queue with the initial paths (the ones given on the
    /// command line) and sets up empty/zeroed bookkeeping around it.
    pub fn new(work_items: &[PathBuf]) -> Self {
        let work_queue = SegQueue::new();

        for work_item in work_items {
            work_queue.push(work_item.clone())
        }

        Self {
            work_queue,
            pending_work_counter: AtomicUsize::new(0),
            condition: Condvar::new(),
            condition_guard: Mutex::new(()),
            cancelled: AtomicBool::new(false),
        }
    }
}

impl WorkQueue for SharedWorkState {
    fn enqueue(&self, paths: impl Iterator<Item = PathBuf>) {
        for path in paths {
            self.work_queue.push(path);
        }
        // wake any workers that are waiting on the queue being empty - the
        // in-flight count for the directory that just expanded is what
        // keeps them from exiting before they can see these new entries
        self.condition.notify_all();
    }
}

/// Signals that at least one path reported an error during `consume_with`.
/// The error itself was already reported to the caller's `on_error`
/// callback as it happened; this only carries the aggregate pass/fail
/// outcome through `Result`'s `?`/`#[must_use]` machinery.
pub struct SearchFailed;

/// How a search ended, once every worker has stopped.
#[derive(Debug, PartialEq)]
pub enum RunOutcome {
    /// Every path was searched.
    Completed,
    /// Ctrl-C (or another `ctrlc`-handled termination signal) asked the
    /// workers to stop - whatever each was already searching finished
    /// normally, but anything still queued was left unsearched.
    Cancelled,
}

/// What `spawn_workers` hands back: a way to collect the results of the
/// search it just started. The workers are already running by the time you
/// have one of these - call [`WorkersHandle::consume_with`] to actually see
/// their output and find out whether anything failed.
pub struct WorkersHandle {
    threads: Vec<JoinHandle<()>>,
    output_rx: Receiver<Result<String, (SearchError, PathBuf)>>,
    shared_state: Arc<SharedWorkState>,
}

impl WorkersHandle {
    /// Drains results until every worker has finished, then joins them.
    ///
    /// Call this exactly once - it takes `self` by value because there's
    /// nothing left to do with the handle afterward. `on_output` is called
    /// with each file's buffered output as it arrives; `on_error` is called
    /// once per path that failed, with the path it failed on.
    pub fn consume_with(
        self,
        mut on_output: impl FnMut(String),
        mut on_error: impl FnMut(SearchError, &Path),
    ) -> Result<RunOutcome, SearchFailed> {
        let mut had_error = false;

        for res in self.output_rx {
            match res {
                Ok(output) => on_output(output),
                Err((err, path)) => {
                    had_error = true;
                    on_error(err, &path);
                }
            }
        }

        for thread in self.threads {
            let _ = thread.join();
        }

        if had_error {
            return Err(SearchFailed);
        }

        if self.shared_state.cancelled.load(Ordering::Acquire) {
            Ok(RunOutcome::Cancelled)
        } else {
            Ok(RunOutcome::Completed)
        }
    }
}

/// Everything [`spawn_workers`] needs that isn't a closure. Grouped into
/// one struct mainly to keep `spawn_workers`'s own parameter list short.
pub struct SpawnOptions<'a> {
    /// The compiled patterns to search for.
    pub patterns: &'a [Regex],
    /// The starting paths given on the command line.
    pub files: &'a [PathBuf],
    /// Search behavior that doesn't depend on the pattern or the path.
    pub search_options: &'a SearchOptions,
    /// How many worker threads to use; `0` picks a number automatically.
    pub max_threads: usize,
}

/// Starts the worker thread pool and returns immediately with a handle to
/// its results - the workers are already running concurrently by the time
/// this function returns.
///
/// `make_printer` is called once per path a worker picks up (file or
/// directory alike), since deciding which it is happens inside `search`
/// itself; it's shared across every thread behind an `Arc`, so it needs to
/// be `Send + Sync`.
pub fn spawn_workers(
    opts: SpawnOptions,
    make_printer: impl Fn() -> Box<dyn Printer> + Send + Sync + 'static,
) -> WorkersHandle {
    let shared_state = Arc::new(SharedWorkState::new(opts.files));
    let make_printer = Arc::new(make_printer);

    // Only the first registration in a process actually takes effect (the
    // `ctrlc` crate errors on a second one) - harmless to ignore here,
    // since the only realistic case of this firing more than once is
    // several tests calling `run()` in the same `cargo test` binary,
    // where Ctrl-C during the test run isn't a scenario worth handling.
    let cancel_state = Arc::clone(&shared_state);
    let _ = ctrlc::set_handler(move || {
        cancel_state.cancelled.store(true, Ordering::Release);
        // Wake every worker currently parked in `wait_while` - without
        // this, one with nothing left in the queue but another still
        // "in flight" would otherwise keep waiting for a notify that
        // only ever comes from work finishing or arriving, neither of
        // which cancellation alone produces.
        cancel_state.condition.notify_all();
    });

    let mut threads: Vec<JoinHandle<()>> = Vec::new();

    let thread_count = if opts.max_threads == 0 {
        thread::available_parallelism().map_or(1, |n| n.get())
    } else {
        opts.max_threads
    };

    let (output_tx, output_rx) = mpsc::channel::<Result<String, (SearchError, PathBuf)>>();

    for _ in 0..thread_count {
        let shared_state = Arc::clone(&shared_state);
        let make_printer = Arc::clone(&make_printer);
        let patterns = opts.patterns.to_vec();
        let search_options = *opts.search_options;
        let output_tx = output_tx.clone();

        let thread = thread::spawn(move || {
            loop {
                let guard = shared_state.condition_guard.lock().unwrap();
                let guard = shared_state
                    .condition
                    .wait_while(guard, |_| {
                        !shared_state.cancelled.load(Ordering::Acquire)
                            && shared_state.work_queue.is_empty()
                            && shared_state.pending_work_counter.load(Ordering::Acquire) > 0
                    })
                    .unwrap();

                if shared_state.cancelled.load(Ordering::Acquire) {
                    // Whatever's left queued stays unsearched - only
                    // work already popped by some other, still-running
                    // worker finishes normally.
                    drop(guard);
                    break;
                }

                let Some(path) = shared_state.work_queue.pop() else {
                    // Lost the race for the last item to another worker; if no
                    // one is still in flight either, there's nothing left.
                    if shared_state.pending_work_counter.load(Ordering::Acquire) == 0 {
                        break;
                    }
                    drop(guard);
                    continue;
                };
                shared_state
                    .pending_work_counter
                    .fetch_add(1, Ordering::AcqRel);
                drop(guard);

                // Caught rather than left to unwind: a panic anywhere in
                // here (e.g. an external tool failing to start) would
                // otherwise skip the pending_work_counter decrement
                // below, permanently wedging every other worker's
                // `wait_while` - the queue would look done (empty) but
                // never actually report as such, hanging the whole
                // program even after all other real work finished.
                let search_result = panic::catch_unwind(AssertUnwindSafe(|| {
                    let mut printer = make_printer();
                    let mut emit = |output: String| {
                        let _ = output_tx.send(Result::Ok(output));
                    };

                    search(
                        &patterns,
                        &path,
                        &search_options,
                        &mut *printer,
                        &*shared_state,
                        &mut emit,
                    )
                }));

                // On success, every bit of output - whether this path was
                // a single plain file or an archive with many members -
                // was already sent to `emit` as each one finished, so
                // there's nothing left to do here but report an error.
                match search_result {
                    Ok(Ok(_outcome)) => {}
                    Ok(Err(err)) => {
                        let _ = output_tx.send(Result::Err((err, path)));
                    }
                    Err(panic_payload) => {
                        let message = panic_message(&panic_payload);
                        let _ = output_tx
                            .send(Result::Err((SearchError::WorkerPanicked(message), path)));
                    }
                }

                shared_state
                    .pending_work_counter
                    .fetch_sub(1, Ordering::AcqRel);
                shared_state.condition.notify_all();
            }
        });
        threads.push(thread);
    }

    // drop the originals so the channels close once every worker's cloned
    // sender has also been dropped, letting the caller's receive loop end
    drop(output_tx);

    WorkersHandle {
        threads,
        output_rx,
        shared_state,
    }
}

/// Pulls a human-readable message out of a caught panic's payload - most
/// panics (including every `.unwrap()`/`.expect()` in this codebase)
/// carry either a `&'static str` or a `String`; anything else (a custom
/// payload from `panic_any`, which nothing here uses) falls back to a
/// generic message rather than failing to report the panic at all.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        message.to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".to_string()
    }
}
