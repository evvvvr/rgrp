use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rgrp::printer::{FlushingPrinter, Printer};
use rgrp::search::{self, MatchConfig, WorkQueue, search};

#[derive(Clone)]
#[allow(dead_code)] // fields are read by some test binaries but not others
pub struct RecordedMatch {
    pub file_path: String,
    pub line_no: u64,
    pub line: String,
}

struct Recorded {
    matches: Vec<RecordedMatch>,
    eof_count: u32,
}

/// `run` creates a fresh printer per file, so a plain `Vec`-backed
/// printer can't accumulate results across a directory walk.
/// `SharedRecorder` hands out `RecordingPrinter`s that all funnel into
/// one shared sink, `Arc<Mutex<_>>`-backed so it can move into worker
/// threads.
#[derive(Clone)]
pub struct SharedRecorder(Arc<Mutex<Recorded>>);

impl SharedRecorder {
    pub fn new() -> Self {
        SharedRecorder(Arc::new(Mutex::new(Recorded {
            matches: Vec::new(),
            eof_count: 0,
        })))
    }

    pub fn printer(&self) -> RecordingPrinter {
        RecordingPrinter(self.0.clone())
    }

    pub fn matches(&self) -> Vec<RecordedMatch> {
        self.0.lock().unwrap().matches.clone()
    }

    #[allow(dead_code)] // used by some test binaries but not others
    pub fn eof_count(&self) -> u32 {
        self.0.lock().unwrap().eof_count
    }
}

#[derive(Clone)]
pub struct RecordingPrinter(Arc<Mutex<Recorded>>);

impl Printer for RecordingPrinter {
    fn render_line(
        &mut self,
        file_path: &str,
        line_no: u64,
        line: &[u8],
        _matches: &[(usize, usize)],
    ) -> String {
        self.0.lock().unwrap().matches.push(RecordedMatch {
            file_path: file_path.to_owned(),
            line_no,
            line: String::from_utf8_lossy(line).into_owned(),
        });
        String::new()
    }

    fn reset(&mut self) {
        self.0.lock().unwrap().eof_count += 1;
    }
}

/// A dead-simple [`WorkQueue`]: a plain FIFO drained by hand, single
/// threaded, with no stealing or cancellation - for tests that want to
/// walk a tree deterministically without spinning up the real
/// work-stealing thread pool (`concurrency::spawn_workers`) at all.
struct SequentialQueue {
    queue: Mutex<VecDeque<PathBuf>>,
    seed_paths: HashSet<PathBuf>,
}

impl SequentialQueue {
    fn new(seed_paths: &[PathBuf]) -> Self {
        Self {
            queue: Mutex::new(seed_paths.iter().cloned().collect()),
            seed_paths: seed_paths.iter().cloned().collect(),
        }
    }

    fn pop(&self) -> Option<PathBuf> {
        self.queue.lock().unwrap().pop_front()
    }
}

impl WorkQueue for SequentialQueue {
    fn enqueue(&self, paths: impl Iterator<Item = PathBuf>) {
        self.queue.lock().unwrap().extend(paths);
    }

    fn is_seed_path(&self, path: &Path) -> bool {
        self.seed_paths.contains(path)
    }
}

/// `search` only expands one level of a directory and returns; draining
/// the queue single-threaded is what walks the whole tree.
#[allow(dead_code)] // used by some test binaries but not others
pub fn walk(
    config: &MatchConfig,
    root: &Path,
    mut make_printer: impl FnMut() -> RecordingPrinter,
) -> Result<(), search::Error> {
    let queue = SequentialQueue::new(&[root.to_path_buf()]);
    while let Some(path) = queue.pop() {
        let mut printer = make_printer();
        let mut emit = |_: String| {};
        let mut flushing_printer = FlushingPrinter::new(&mut printer, &mut emit);
        search(config, &path, &mut flushing_printer, &queue)?;
    }
    Ok(())
}
