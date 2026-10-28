use std::sync::{Arc, Mutex};

use rgrp::printer::{BufferedOutput, Printer};

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

/// `run` creates a fresh printer per file via a `FnMut() -> Printer` factory
/// and hands each one to `search`, so a plain `Vec`-backed printer can't
/// accumulate results across an entire directory walk. `SharedRecorder`
/// hands out lightweight
/// `RecordingPrinter`s that all funnel into the same shared sink, mirroring
/// how `ColorPrinter`/`MonochromePrinter` are used in production. It's
/// `Arc<Mutex<_>>`-backed (rather than `Rc<RefCell<_>>`) so it can be moved
/// into the worker threads `run` spawns.
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
    fn print(&mut self, file_path: &str, line_no: u64, line: &[u8], _matches: &[(usize, usize)]) {
        self.0.lock().unwrap().matches.push(RecordedMatch {
            file_path: file_path.to_owned(),
            line_no,
            line: String::from_utf8_lossy(line).into_owned(),
        });
    }

    fn on_eof(&mut self) {
        self.0.lock().unwrap().eof_count += 1;
    }

    fn into_output(self: Box<Self>) -> String {
        String::new()
    }
}

impl BufferedOutput for RecordingPrinter {
    fn into_output(self) -> String {
        // RecordingPrinter records structured matches directly into the
        // shared recorder rather than producing printable text.
        String::new()
    }
}
