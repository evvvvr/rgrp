use crossbeam::queue::SegQueue;
use regex::bytes::Regex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use crate::printer::{BufferedOutput, Printer};
use crate::search::{Error as SearchError, Options as SearchOptions, Outcome, WorkQueue, search};

/// The state every worker thread shares while walking a search: the queue
/// of paths still to process, and the bookkeeping that lets workers agree
/// on when there's truly nothing left to do.
pub struct SharedWorkState {
    /// Paths waiting to be searched. A directory expands into its entries
    /// here rather than being recursed into directly, which is what lets
    /// different workers pick up different parts of the same walk.
    pub work_queue: SegQueue<String>,
    /// How many paths are currently being processed (popped, but not yet
    /// finished). Needed so a worker that finds the queue empty can tell
    /// "we're done" apart from "someone's mid-directory and about to push
    /// more work".
    pub pending_work_counter: AtomicUsize,
    /// Wakes workers that are waiting for new work or for everyone to finish.
    pub condition: Condvar,
    /// Paired with `condition`; holds no real data of its own.
    pub condition_guard: Mutex<()>,
}

impl SharedWorkState {
    /// Seeds the work queue with the initial paths (the ones given on the
    /// command line) and sets up empty/zeroed bookkeeping around it.
    pub fn new(work_items: &[String]) -> Self {
        let work_queue = SegQueue::new();

        for work_item in work_items {
            work_queue.push(work_item.clone())
        }

        Self {
            work_queue: work_queue,
            pending_work_counter: AtomicUsize::new(0),
            condition: Condvar::new(),
            condition_guard: Mutex::new(()),
        }
    }
}

impl WorkQueue for SharedWorkState {
    fn enqueue(&self, paths: impl Iterator<Item = String>) {
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

/// What `spawn_workers` hands back: a way to collect the results of the
/// search it just started. The workers are already running by the time you
/// have one of these - call [`WorkersHandle::consume_with`] to actually see
/// their output and find out whether anything failed.
pub struct WorkersHandle {
    threads: Vec<JoinHandle<()>>,
    output_rx: Receiver<Result<String, (SearchError, String)>>,
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
        mut on_error: impl FnMut(SearchError, &str),
    ) -> Result<(), SearchFailed> {
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

        if had_error { Err(SearchFailed) } else { Ok(()) }
    }
}

/// Everything [`spawn_workers`] needs that isn't a closure. Grouped into
/// one struct mainly to keep `spawn_workers`'s own parameter list short.
pub struct SpawnOptions<'a> {
    /// The compiled patterns to search for.
    pub patterns: &'a [Regex],
    /// The starting paths given on the command line.
    pub files: &'a [String],
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
/// itself; it's cloned into every thread, so it needs to be `Send`.
pub fn spawn_workers<P: Printer + BufferedOutput>(
    opts: SpawnOptions,
    make_printer: impl FnMut() -> P + Send + Clone + 'static,
) -> WorkersHandle {
    let shared_state = Arc::new(SharedWorkState::new(opts.files));

    let mut threads: Vec<JoinHandle<()>> = Vec::new();

    let thread_count = if opts.max_threads == 0 {
        thread::available_parallelism().map_or(1, |n| n.get())
    } else {
        opts.max_threads
    };

    let (output_tx, output_rx) = mpsc::channel::<Result<String, (SearchError, String)>>();

    for _ in 0..thread_count {
        let shared_state = Arc::clone(&shared_state);
        let mut make_printer = make_printer.clone();
        let patterns = opts.patterns.to_vec();
        let search_options = *opts.search_options;
        let output_tx = output_tx.clone();
        let thread = thread::spawn(move || {
            // forwards the soft, in-flight errors `search` reports while
            // expanding a directory (e.g. a non-UTF8 entry) into the same
            // channel as path-level failures, so the consumer sees every
            // error from one place
            let report_tx = output_tx.clone();
            let mut report_error = move |err: SearchError, path: &str| {
                let _ = report_tx.send(Err((err, path.to_string())));
            };

            loop {
                let guard = shared_state.condition_guard.lock().unwrap();
                let guard = shared_state
                    .condition
                    .wait_while(guard, |_| {
                        shared_state.work_queue.is_empty()
                            && shared_state.pending_work_counter.load(Ordering::Acquire) > 0
                    })
                    .unwrap();

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

                let mut printer = make_printer();
                let res = search(
                    &patterns,
                    &path,
                    &search_options,
                    &mut printer,
                    &mut report_error,
                    &*shared_state,
                );

                match res {
                    Ok(Outcome::Done) => {
                        let output = printer.into_output();
                        if !output.is_empty() {
                            let _ = output_tx.send(Result::Ok(output));
                        }
                    }
                    Ok(Outcome::BinarySkipped) => {}
                    Err(err) => {
                        let _ = output_tx.send(Result::Err((err, path)));
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

    WorkersHandle { threads, output_rx }
}
