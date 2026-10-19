use std::cell::RefCell;
use std::rc::Rc;

use rgrp::printer::Printer;

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

/// `run`/`search` create a fresh printer per file via a `FnMut() -> Printer`
/// factory, so a plain `Vec`-backed printer can't accumulate results across
/// an entire directory walk. `SharedRecorder` hands out lightweight
/// `RecordingPrinter`s that all funnel into the same shared sink, mirroring
/// how `ColorPrinter`/`MonochromePrinter` are used in production.
#[derive(Clone)]
pub struct SharedRecorder(Rc<RefCell<Recorded>>);

impl SharedRecorder {
    pub fn new() -> Self {
        SharedRecorder(Rc::new(RefCell::new(Recorded {
            matches: Vec::new(),
            eof_count: 0,
        })))
    }

    pub fn printer(&self) -> RecordingPrinter {
        RecordingPrinter(self.0.clone())
    }

    pub fn matches(&self) -> Vec<RecordedMatch> {
        self.0.borrow().matches.clone()
    }

    #[allow(dead_code)] // used by some test binaries but not others
    pub fn eof_count(&self) -> u32 {
        self.0.borrow().eof_count
    }
}

pub struct RecordingPrinter(Rc<RefCell<Recorded>>);

impl Printer for RecordingPrinter {
    fn print(&mut self, file_path: &str, line_no: u64, line: &str, _m: &regex::Match) {
        self.0.borrow_mut().matches.push(RecordedMatch {
            file_path: file_path.to_string(),
            line_no,
            line: line.to_string(),
        });
    }

    fn on_eof(&mut self) {
        self.0.borrow_mut().eof_count += 1;
    }
}
