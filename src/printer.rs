use owo_colors::OwoColorize;

pub trait Printer {
    fn print(&mut self, file_path: &str, line_no: u64, line: &str, _m: &regex::Match);
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
    fn print(&mut self, file_path: &str, line_no: u64, line: &str, m: &regex::Match) {
        let before_match = &line[0..m.start()];
        let match_ = m.as_str();
        let after_match = &line[m.end()..];

        if !self.has_printed_filename {
            println!("{}", file_path.bold().purple());
            self.has_printed_filename = true;
        }

        println!(
            "{}: {}{}{}",
            line_no.green(),
            before_match,
            match_.red(),
            after_match
        )
    }

    fn on_eof(&mut self) {
        println!("");
    }
}

pub struct MonochromePrinter {}

impl Printer for MonochromePrinter {
    fn print(&mut self, file_path: &str, _line_no: u64, line: &str, _m: &regex::Match) {
        println!("{file_path}:{line}");
    }

    fn on_eof(&mut self) {
        // nop
    }
}
