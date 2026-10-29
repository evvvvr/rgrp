use crossbeam_queue::SegQueue;
use regex::bytes::Regex;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use crate::printer::{FlushingPrinter, Printer};
use crate::search::{Error as SearchError, Options as SearchOptions, WorkQueue, search};

/// State every worker thread shares: the queue of paths still to
/// process, and the bookkeeping that lets workers agree when there's
/// nothing left to do.
pub struct SharedWorkState {
    /// Paths waiting to be searched. A directory expands into its
    /// entries here instead of being recursed into directly, so
    /// different workers can pick up different parts of the walk.
    pub work_queue: SegQueue<PathBuf>,
    /// Paths popped but not yet finished - lets a worker that finds
    /// the queue empty tell "we're done" from "someone's mid-directory
    /// and about to push more work".
    pub pending_work_counter: AtomicUsize,
    /// Wakes workers waiting for new work or for everyone to finish.
    pub condition: Condvar,
    /// Paired with `condition`; holds no real data of its own.
    pub condition_guard: Mutex<()>,
    /// Set once (by a Ctrl-C handler) to stop every worker from
    /// picking up new work - whatever's already in progress still
    /// runs to completion.
    pub cancelled: AtomicBool,
    /// Held by whichever worker is currently sending a path's output,
    /// so a large file/archive streaming output in several chunks
    /// can't have another worker's path interleaved in between.
    /// Acquired lazily, on the first chunk a path has to send.
    pub print_lock: Mutex<()>,
}

impl SharedWorkState {
    /// Seeds the work queue with the given paths.
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
            print_lock: Mutex::new(()),
        }
    }
}

impl WorkQueue for SharedWorkState {
    fn enqueue(&self, paths: impl Iterator<Item = PathBuf>) {
        for path in paths {
            self.work_queue.push(path);
        }
        // Wake workers waiting on the queue being empty.
        self.condition.notify_all();
    }
}

/// Signals that at least one path reported an error during
/// `consume_with`. The error itself was already reported to the
/// caller's `on_error` callback; this just carries pass/fail through
/// `Result`.
pub struct SearchFailed;

/// How a search ended, once every worker has stopped.
#[derive(Debug, PartialEq)]
pub enum RunOutcome {
    /// Every path was searched.
    Completed,
    /// Ctrl-C asked the workers to stop; anything still queued was
    /// left unsearched.
    Cancelled,
}

/// What `spawn_workers` hands back - the workers are already running;
/// call [`WorkersHandle::consume_with`] to see their output and find
/// out whether anything failed.
pub struct WorkersHandle {
    threads: Vec<JoinHandle<()>>,
    output_rx: Receiver<Result<String, (SearchError, PathBuf)>>,
    shared_state: Arc<SharedWorkState>,
}

impl WorkersHandle {
    /// Drains results until every worker has finished, then joins them.
    /// Call this exactly once. `on_output` is called with each file's
    /// buffered output as it arrives; `on_error` once per failed path.
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

/// Everything [`spawn_workers`] needs that isn't a closure, grouped to
/// keep its parameter list short.
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

/// Starts the worker thread pool and returns a handle to its results;
/// the workers are already running by the time this returns.
///
/// `make_printer` is called once per path a worker picks up, shared
/// across threads behind an `Arc`, so it needs to be `Send + Sync`.
pub fn spawn_workers(
    opts: SpawnOptions,
    make_printer: impl Fn() -> Box<dyn Printer> + Send + Sync + 'static,
) -> WorkersHandle {
    let shared_state = Arc::new(SharedWorkState::new(opts.files));
    let make_printer = Arc::new(make_printer);

    // Only the first registration per process takes effect - harmless
    // to ignore a second one, which only happens when several tests
    // call `run()` in the same binary.
    let cancel_state = Arc::clone(&shared_state);
    let _ = ctrlc::set_handler(move || {
        // Held across the swap and the notify: a worker's `wait_while`
        // only re-checks its predicate while holding this same mutex,
        // so taking it here stops a notify from firing in the gap
        // between the worker reading `cancelled` and parking on the
        // condvar, where it would otherwise be silently dropped.
        let guard = cancel_state
            .condition_guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A second Ctrl-C while workers are still finishing means
        // "stop now" - skip the graceful path entirely.
        let already_cancelled = cancel_state.cancelled.swap(true, Ordering::AcqRel);
        cancel_state.condition.notify_all();
        drop(guard);

        if already_cancelled {
            eprintln!("rgrp: second interrupt, exiting immediately");
            std::process::exit(130);
        }
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
                    drop(guard);
                    break;
                }

                let Some(path) = shared_state.work_queue.pop() else {
                    // Lost the race for the last item; nothing left if
                    // no one else is still in flight either.
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

                // Caught rather than left to unwind: an uncaught panic
                // here would skip the pending_work_counter decrement
                // below, wedging every other worker's `wait_while`
                // forever even after all other work finished.
                let search_result = panic::catch_unwind(AssertUnwindSafe(|| {
                    let mut printer = make_printer();
                    // Acquired on this path's first chunk and held
                    // through its later ones, so no other path's
                    // output can interleave. Released once this
                    // closure finishes. Never touched by a path with
                    // no matches.
                    let mut print_guard: Option<std::sync::MutexGuard<'_, ()>> = None;
                    let mut emit = |output: String| {
                        if print_guard.is_none() {
                            // Recovers from poisoning rather than
                            // `.unwrap()`-ing: this mutex only ever
                            // guards `()`, so one worker panicking
                            // while holding it should never cascade
                            // into every other worker's lock attempt
                            // panicking too.
                            print_guard = Some(
                                shared_state
                                    .print_lock
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                            );
                        }
                        let _ = output_tx.send(Result::Ok(output));
                    };
                    let mut flushing_printer = FlushingPrinter::new(&mut *printer, &mut emit);

                    search(&patterns, &path, &search_options, &mut flushing_printer, &*shared_state)
                }));

                // On success, output was already sent to `emit` as
                // each member finished, so only an error is left to
                // report here.
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

    // Drop the original sender so the channel closes once every
    // worker's clone is also dropped.
    drop(output_tx);

    WorkersHandle {
        threads,
        output_rx,
        shared_state,
    }
}

/// Pulls a human-readable message out of a caught panic's payload.
/// Falls back to a generic message for anything that isn't a
/// `&'static str` or `String`.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        message.to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".to_string()
    }
}
