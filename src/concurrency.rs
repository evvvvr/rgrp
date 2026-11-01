use crossbeam_deque::{Injector, Stealer, Worker};
use regex::bytes::Regex;
use std::collections::HashSet;
use std::iter;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use crate::glob::GlobSpec;
use crate::printer::{FlushingPrinter, LockedSink, OutputSink, Printer};
use crate::search::{Error as SearchError, MatchConfig, WorkQueue, search};

/// State shared across a whole run, regardless of which queueing
/// strategy is actually walking paths with it - how work items flow
/// between workers lives entirely in whatever implements [`WorkQueue`]
/// ([`WorkerQueue`] for `spawn_workers`'s own work-stealing engine, or
/// a test's own simple stand-in), never here.
struct SharedWorkState {
    /// The paths given directly on the command line, exempt from
    /// `--glob`/`--iglob` - set once in `new` and never changed.
    seed_paths: HashSet<PathBuf>,
    /// Set once (by a Ctrl-C handler) to stop every worker from
    /// picking up new work - whatever's already in progress still
    /// runs to completion.
    cancelled: AtomicBool,
    /// Held by whichever worker is currently sending a path's output,
    /// so a large file/archive streaming output in several chunks
    /// can't have another worker's path interleaved in between.
    /// Acquired lazily, on the first chunk a path has to send.
    print_lock: Mutex<()>,
}

impl SharedWorkState {
    fn new(seed_paths: &[PathBuf]) -> Self {
        Self {
            seed_paths: seed_paths.iter().cloned().collect(),
            cancelled: AtomicBool::new(false),
            print_lock: Mutex::new(()),
        }
    }

    fn is_seed_path(&self, path: &Path) -> bool {
        self.seed_paths.contains(path)
    }
}

/// [`WorkQueue`] for `spawn_workers`'s own work-stealing engine, and
/// the one place that knows `crossbeam_deque` exists at all - `search`
/// only ever sees this behind the `WorkQueue` trait, so it has no idea
/// whether it's driving a work-stealing pool or `tests/common::walk`'s
/// single FIFO queue.
///
/// One of these belongs to exactly one worker thread (`local` is that
/// thread's own deque, moved in rather than shared), but `global` and
/// `stealers` are the same `Arc`s cloned into every worker, and
/// `shared_state` is the one `seed_paths` set every worker reads
/// without each keeping its own copy.
struct WorkerQueue {
    local: Worker<PathBuf>,
    global: Arc<Injector<PathBuf>>,
    /// Every worker's own stealer, including this one's - stealing
    /// from yourself just never finds anything, since you'd have
    /// already popped it out of `local` first.
    stealers: Arc<Vec<Stealer<PathBuf>>>,
    /// How many workers across the whole pool are currently mid-task
    /// (popped, not yet finished). Shared by every worker's
    /// `WorkerQueue`, not per-worker.
    active_workers: Arc<AtomicUsize>,
    shared_state: Arc<SharedWorkState>,
    /// This worker's own clone of the shared error-reporting channel -
    /// kept here (not on `SharedWorkState`) specifically so
    /// `WorkersHandle` never ends up holding a live sender on itself:
    /// it keeps its own `Arc<SharedWorkState>` clone around after the
    /// channel closes, to check `cancelled` once every worker is done,
    /// and a sender living there would mean the channel could never
    /// close while something's still waiting to drain it.
    ///
    /// Successful output never travels over this (or any) channel - it's
    /// printed directly by the worker that produced it, while holding
    /// `shared_state.print_lock`; only a failed path is reported back
    /// to the calling thread, since `on_error` is a plain `FnMut` that's
    /// only ever called there.
    error_tx: Sender<(SearchError, PathBuf)>,
}

impl WorkerQueue {
    /// One attempt at finding a task: this worker's own deque first,
    /// then a batch stolen from the global queue, then a single task
    /// from a peer - retrying while a steal reports "retry" (another
    /// thief got there first) rather than treating that the same as
    /// "genuinely nothing left".
    fn steal_one(&self) -> Option<PathBuf> {
        self.local.pop().or_else(|| {
            iter::repeat_with(|| {
                self.global
                    .steal_batch_and_pop(&self.local)
                    .or_else(|| self.stealers.iter().map(Stealer::steal).collect())
            })
            .find(|s| !s.is_retry())
            .and_then(|s| s.success())
        })
    }

    /// Blocks (by spinning) until either a task is available, the run
    /// is cancelled, or every worker is confirmed idle with nothing
    /// left anywhere - the only three ways this can return.
    ///
    /// A single failed `steal_one` never means "stop": another worker
    /// might be mid-directory and about to push a batch of new
    /// entries. Only safe to stop once `active_workers` reads `0` -
    /// and even then, only after one more `steal_one`, not before:
    /// whoever was last active is guaranteed to have pushed everything
    /// it was going to *before* decrementing, so once this reads `0`,
    /// every push that could still happen already has - that final
    /// `steal_one` is what actually proves none of them left work
    /// behind. Checking it in the other order (queue-then-active)
    /// would leave a real gap where a push lands in between and never
    /// gets seen by anyone.
    fn find_next(&self) -> Option<PathBuf> {
        loop {
            if self.shared_state.cancelled.load(Ordering::Acquire) {
                return None;
            }

            if let Some(path) = self.steal_one() {
                return Some(path);
            }

            if self.active_workers.load(Ordering::Acquire) == 0 {
                return self.steal_one();
            }

            thread::yield_now();
        }
    }

    /// Marks one task as started; pairs with [`Self::task_done`],
    /// bracketing the whole time a path is being searched (including
    /// anything it pushes via `enqueue`) so [`Self::find_next`]'s
    /// "everyone's idle" check stays accurate.
    fn task_started(&self) {
        self.active_workers.fetch_add(1, Ordering::AcqRel);
    }

    fn task_done(&self) {
        self.active_workers.fetch_sub(1, Ordering::AcqRel);
    }

    /// Reports that a path failed outright (an `Err` from `search`, or
    /// a caught panic turned into [`crate::search::Error::WorkerPanicked`]).
    fn report_error(&self, err: SearchError, path: PathBuf) {
        let _ = self.error_tx.send((err, path));
    }
}

impl WorkQueue for WorkerQueue {
    fn enqueue(&self, paths: impl Iterator<Item = PathBuf>) {
        for path in paths {
            self.local.push(path);
        }
    }

    fn is_seed_path(&self, path: &Path) -> bool {
        self.shared_state.is_seed_path(path)
    }
}

/// Signals that at least one path reported an error during
/// `consume_with`. The error itself was already reported to the
/// caller's `on_error` callback; this just carries pass/fail through
/// `Result`.
pub(crate) struct SearchFailed;

/// How a search ended, once every worker has stopped.
#[derive(Debug, PartialEq)]
pub(crate) enum RunOutcome {
    /// Every path was searched.
    Completed,
    /// Ctrl-C asked the workers to stop; anything still queued was
    /// left unsearched.
    Cancelled,
}

/// What `spawn_workers` hands back - the workers are already running;
/// call [`WorkersHandle::consume_with`] to find out whether anything
/// failed - output was already printed as it was found.
pub(crate) struct WorkersHandle {
    threads: Vec<JoinHandle<()>>,
    error_rx: Receiver<(SearchError, PathBuf)>,
    shared_state: Arc<SharedWorkState>,
}

impl WorkersHandle {
    /// Drains errors until every worker has finished, then joins them.
    /// Call this exactly once. Successful output was already printed
    /// directly by whichever worker produced it; `on_error` is called
    /// here, on the calling thread, once per failed path.
    pub(crate) fn consume_with(
        self,
        mut on_error: impl FnMut(SearchError, &Path),
    ) -> Result<RunOutcome, SearchFailed> {
        let mut had_error = false;

        for (err, path) in self.error_rx {
            had_error = true;
            on_error(err, &path);
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

/// The patterns/globs-independent behavior knobs out of `MatchConfig`,
/// as a `'static`, `Copy` bundle a worker thread can own outright -
/// `MatchConfig` itself can't move into a `'static` thread closure,
/// since its `patterns`/`globs` fields still borrow from `opts`, which
/// doesn't outlive `spawn_workers`.
#[derive(Clone, Copy)]
struct MatchBehavior {
    invert_matches: bool,
    search_binary: bool,
    crlf: bool,
    search_archives: bool,
}

impl From<MatchConfig<'_>> for MatchBehavior {
    fn from(config: MatchConfig<'_>) -> Self {
        Self {
            invert_matches: config.invert_matches,
            search_binary: config.search_binary,
            crlf: config.crlf,
            search_archives: config.search_archives,
        }
    }
}

/// Everything [`spawn_workers`] needs that isn't a closure, grouped to
/// keep its parameter list short.
pub(crate) struct SpawnOptions<'a> {
    /// Patterns, globs, and behavior knobs - identical for every
    /// worker, for the whole run.
    pub(crate) config: MatchConfig<'a>,
    /// The starting paths given on the command line.
    pub(crate) files: &'a [PathBuf],
    /// How many worker threads to use; `0` picks a number automatically.
    pub(crate) max_threads: usize,
}

/// Starts the worker thread pool and returns a handle to its results;
/// the workers are already running by the time this returns.
///
/// `make_printer` and `write_output` are both shared across threads
/// behind an `Arc`, so both need to be `Send + Sync`. `make_printer` is
/// called once per worker thread; `write_output` once per chunk of
/// output a worker has decided is definitely going out (see
/// [`crate::printer::LockedSink`]).
pub(crate) fn spawn_workers(
    opts: SpawnOptions,
    make_printer: impl Fn() -> Box<dyn Printer> + Send + Sync + 'static,
    write_output: impl Fn(&str) + Send + Sync + 'static,
) -> WorkersHandle {
    let shared_state = Arc::new(SharedWorkState::new(opts.files));
    let make_printer: Arc<dyn Fn() -> Box<dyn Printer> + Send + Sync> = Arc::new(make_printer);
    let write_output: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(write_output);
    let behavior = MatchBehavior::from(opts.config);

    install_cancel_handler(&shared_state);

    let mut threads: Vec<JoinHandle<()>> = Vec::new();

    let thread_count = if opts.max_threads == 0 {
        thread::available_parallelism().map_or(1, |n| n.get())
    } else {
        opts.max_threads
    };

    // Seeds the global queue once; each worker's own local deque is
    // what actually grows from there as directories get expanded.
    let global_queue = Arc::new(Injector::new());
    for file in opts.files {
        global_queue.push(file.clone());
    }

    let local_queues: Vec<Worker<PathBuf>> = (0..thread_count).map(|_| Worker::new_fifo()).collect();
    // Shared so every worker can try stealing from every other one.
    let stealers: Arc<Vec<Stealer<PathBuf>>> =
        Arc::new(local_queues.iter().map(Worker::stealer).collect());
    let active_workers = Arc::new(AtomicUsize::new(0));

    let (error_tx, error_rx) = mpsc::channel::<(SearchError, PathBuf)>();

    for local_queue in local_queues {
        let queue = WorkerQueue {
            local: local_queue,
            global: Arc::clone(&global_queue),
            stealers: Arc::clone(&stealers),
            active_workers: Arc::clone(&active_workers),
            shared_state: Arc::clone(&shared_state),
            error_tx: error_tx.clone(),
        };
        let make_printer = Arc::clone(&make_printer);
        let write_output = Arc::clone(&write_output);
        let patterns = opts.config.patterns.to_vec();
        let globs = opts.config.globs.to_vec();

        let thread =
            thread::spawn(move || worker_loop(queue, patterns, globs, behavior, make_printer, write_output));
        threads.push(thread);
    }

    // Drop the original sender so the channel closes once every
    // worker's clone is also dropped.
    drop(error_tx);

    WorkersHandle {
        threads,
        error_rx,
        shared_state,
    }
}

/// Installs the Ctrl-C handler for a whole run. Only the first
/// registration per process takes effect - harmless to ignore a
/// second one, which only happens when several tests call `run()` in
/// the same binary.
fn install_cancel_handler(shared_state: &Arc<SharedWorkState>) {
    let cancel_state = Arc::clone(shared_state);
    let _ = ctrlc::set_handler(move || {
        // A second Ctrl-C while workers are still finishing means
        // "stop now" - skip the graceful path entirely. No condvar to
        // hold here: workers never block waiting for a wakeup, they
        // poll `cancelled` once per loop iteration, so a plain swap is
        // enough for them to notice on their very next spin.
        let already_cancelled = cancel_state.cancelled.swap(true, Ordering::AcqRel);

        if already_cancelled {
            eprintln!("rgrp: second interrupt, exiting immediately");
            std::process::exit(130);
        }
    });
}

/// One worker thread's whole lifetime: pulls paths until there's
/// nothing left or the run is cancelled, searching each one with one
/// `Printer` instance reused across all of them (see
/// `FlushingPrinter::finish`/`discard`'s per-file `reset` for why
/// that's safe).
fn worker_loop(
    queue: WorkerQueue,
    patterns: Vec<Regex>,
    globs: Vec<GlobSpec>,
    behavior: MatchBehavior,
    make_printer: Arc<dyn Fn() -> Box<dyn Printer> + Send + Sync>,
    write_output: Arc<dyn Fn(&str) + Send + Sync>,
) {
    let config = MatchConfig {
        patterns: &patterns,
        globs: &globs,
        invert_matches: behavior.invert_matches,
        search_binary: behavior.search_binary,
        crlf: behavior.crlf,
        search_archives: behavior.search_archives,
    };

    let mut printer = make_printer();

    while let Some(path) = queue.find_next() {
        queue.task_started();
        search_one_path(&queue, &mut *printer, &*write_output, &config, path);
        queue.task_done();
    }
}

/// Searches one path, catching a panic so one bad file can't take the
/// whole worker thread down, and reports whatever happened back
/// through `queue`.
fn search_one_path(
    queue: &WorkerQueue,
    printer: &mut dyn Printer,
    write_output: &(dyn Fn(&str) + Send + Sync),
    config: &MatchConfig,
    path: PathBuf,
) {
    let search_result = panic::catch_unwind(AssertUnwindSafe(|| {
        // `LockedSink` holds the print lock lazily for as long as it
        // lives, which here is exactly one path - so no other path's
        // output can interleave with this one's, but a path with no
        // matches never touches the lock at all.
        let mut sink = LockedSink::new(&queue.shared_state.print_lock, write_output);
        let mut emit = |output: String| sink.write(&output);
        let mut flushing_printer = FlushingPrinter::new(&mut *printer, &mut emit);

        search(config, &path, &mut flushing_printer, queue)
    }));

    // On success, output was already sent to `emit` as each member
    // finished, so only an error is left to report here.
    match search_result {
        Ok(Ok(_outcome)) => {}
        Ok(Err(err)) => {
            queue.report_error(err, path);
        }
        Err(panic_payload) => {
            let message = panic_message(&panic_payload);
            queue.report_error(SearchError::WorkerPanicked(message), path);
            // A panic can leave the reused `printer` holding per-file
            // state (e.g. "have I printed this file's name yet") from
            // whatever path it was mid-rendering - `finish`/`discard`
            // never ran to clear it, so the next path this thread
            // picks up would otherwise inherit it.
            printer.reset();
        }
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
