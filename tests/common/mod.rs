use std::path::Path;
use std::sync::{Arc, Mutex};

use regex::bytes::Regex;
use rgrp::concurrency::SharedWorkState;
use rgrp::glob::GlobSpec;
use rgrp::printer::{FlushingPrinter, Printer};
use rgrp::search::{self, search};

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

/// `search` only expands one level of a directory and returns; draining
/// the queue single-threaded is what walks the whole tree.
#[allow(dead_code)] // used by some test binaries but not others
pub fn walk(
    patterns: &[Regex],
    globs: &[GlobSpec],
    root: &Path,
    options: &search::Options,
    mut make_printer: impl FnMut() -> RecordingPrinter,
) -> Result<(), search::Error> {
    let shared_state = SharedWorkState::new(&[root.to_path_buf()]);
    while let Some(path) = shared_state.work_queue.pop() {
        let mut printer = make_printer();
        let mut emit = |_: String| {};
        let mut flushing_printer = FlushingPrinter::new(&mut printer, &mut emit);
        search(
            patterns,
            globs,
            &path,
            options,
            &mut flushing_printer,
            &shared_state,
        )?;
    }
    Ok(())
}
