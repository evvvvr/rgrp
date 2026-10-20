use owo_colors::OwoColorize;

pub trait Printer {
    /// Reports a single line that had at least one match. `matches` holds
    /// every match on the line, sorted by start offset, so implementations
    /// can render the whole line in one shot instead of being called once
    /// per match.
    fn print(&mut self, file_path: &str, line_no: u64, line: &str, matches: &[regex::Match]);
    fn on_eof(&mut self);
}

pub struct ColorPrinter {
    // assuming one printer per file
    has_printed_filename: bool,
}

impl ColorPrinter {
    pub fn new() -> Self {
        Self {
            has_printed_filename: false,
        }
    }
}

impl Printer for ColorPrinter {
    fn print(&mut self, file_path: &str, line_no: u64, line: &str, matches: &[regex::Match]) {
        if !self.has_printed_filename {
            println!("{}", file_path.bold().purple());
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

        println!("{}: {}", line_no.green(), highlighted)
    }

    fn on_eof(&mut self) {
        println!("");
    }
}

pub struct MonochromePrinter {}

impl Printer for MonochromePrinter {
    fn print(&mut self, file_path: &str, _line_no: u64, line: &str, _matches: &[regex::Match]) {
        println!("{file_path}:{line}");
    }

    fn on_eof(&mut self) {
        // nop
    }
}
