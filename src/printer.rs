use std::fmt::Write;

use owo_colors::OwoColorize;

/// Turns matched lines into whatever a printer's output format looks like.
///
/// One printer instance is created per file, so implementations can hold
/// per-file state (e.g. "have I already printed this file's name?").
pub trait Printer {
    /// Reports a single line that had at least one match. `matches` holds
    /// every match on the line, sorted by start offset, so implementations
    /// can render the whole line in one shot instead of being called once
    /// per match.
    fn print(&mut self, file_path: &str, line_no: u64, line: &str, matches: &[regex::Match]);
    /// Called once a file has been read to the end. Most printers don't
    /// need this; it exists for ones that need to know when a file is done.
    fn on_eof(&mut self);
}

/// A printer that builds up its output in memory instead of writing it as
/// it goes, so the caller can collect a whole file's worth of output and
/// print it as one piece - keeping multiple threads' output from
/// interleaving mid-file.
pub trait BufferedOutput {
    /// Consumes the printer and returns everything it has buffered so far.
    fn into_output(self) -> String;
}

/// Renders matches with ANSI colors (filename in purple/bold, matched text
/// in red), for output going to an actual terminal.
pub struct ColorPrinter {
    // assuming one printer per file
    has_printed_filename: bool,
    output: String,
}

impl ColorPrinter {
    /// Creates a printer for a single file, with nothing buffered yet.
    pub fn new() -> Self {
        Self {
            has_printed_filename: false,
            output: String::new(),
        }
    }
}

impl Printer for ColorPrinter {
    fn print(&mut self, file_path: &str, line_no: u64, line: &str, matches: &[regex::Match]) {
        if !self.has_printed_filename {
            writeln!(self.output).expect("Error writing output");
            writeln!(self.output, "{}", file_path.bold().purple()).expect("Error writing output");
            self.has_printed_filename = true;
        }

        let mut highlighted = String::new();
        let mut printed_char_index = 0;

        for m in matches {
            if m.start() > printed_char_index {
                highlighted.push_str(&line[printed_char_index..m.start()]);
            }
            highlighted.push_str(&m.as_str().red().to_string());
            printed_char_index = m.end();
        }

        if printed_char_index < line.len() {
            highlighted.push_str(&line[printed_char_index..]);
        }

        let _ = writeln!(self.output, "{}: {}", line_no.green(), highlighted);
    }

    fn on_eof(&mut self) {
        // nop
    }
}

impl BufferedOutput for ColorPrinter {
    fn into_output(self) -> String {
        self.output
    }
}

/// Renders matches as plain `path: line` text, no color codes - for
/// output that isn't going to a terminal (piped to a file, `NO_COLOR`, etc.).
pub struct MonochromePrinter {
    output: String,
}

impl MonochromePrinter {
    /// Creates a printer for a single file, with nothing buffered yet.
    pub fn new() -> Self {
        Self {
            output: String::new(),
        }
    }
}

impl Printer for MonochromePrinter {
    fn print(&mut self, file_path: &str, _line_no: u64, line: &str, _matches: &[regex::Match]) {
        writeln!(self.output, "{file_path}: {line}").expect("Error writing output");
    }

    fn on_eof(&mut self) {
        // nop
    }
}

impl BufferedOutput for MonochromePrinter {
    fn into_output(self) -> String {
        self.output
    }
}
