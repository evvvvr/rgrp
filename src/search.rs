use regex::Regex;
use std::fs;
use std::io::BufRead;
use std::io::BufReader;

use crate::printer::Printer;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Pattern(regex::Error),
    NonUtf8Path,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(err) => write!(f, "{err}"),
            Error::Pattern(err) => write!(f, "invalid search pattern: {err}"),
            Error::NonUtf8Path => write!(f, "skipping non-UTF8 path"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<regex::Error> for Error {
    fn from(e: regex::Error) -> Self {
        Error::Pattern(e)
    }
}

pub fn search<T: Printer>(
    patterns: &[Regex],
    path: &str,
    make_printer: &mut impl FnMut() -> T,
    on_error: &mut impl FnMut(Error, &str),
) -> Result<(), Error> {
    // symlink_metadata (unlike metadata) reports the entry's own type instead
    // of following the symlink, so this check actually catches symlinks
    let metadata = fs::symlink_metadata(path)?;

    if metadata.is_symlink() {
        // skip for now - this could lead to infinite cycles
        return Ok(());
    }

    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let entry_path = entry.path();
            let Some(entry_path) = entry_path.to_str() else {
                // non-UTF8 path - skip rather than aborting the whole walk
                on_error(Error::NonUtf8Path, &entry_path.to_string_lossy());
                continue;
            };
            search(patterns, entry_path, make_printer, on_error)?;
        }
        return Ok(());
    }

    if !metadata.is_file() {
        // Something we don't know how to work with
        return Ok(());
    }

    search_file(patterns, path, &mut make_printer())
}

fn search_file<T: Printer>(
    patterns: &[Regex],
    file_path: &str,
    printer: &mut T,
) -> Result<(), Error> {
    let file = fs::File::open(file_path)?;
    search_reader(patterns, file_path, BufReader::new(file), printer)
}

fn search_reader<T: Printer>(
    patterns: &[Regex],
    file_path: &str,
    mut reader: impl BufRead,
    printer: &mut T,
) -> Result<(), Error> {
    let mut buf = String::new();
    let mut line_number = 1;

    loop {
        let res = reader.read_line(&mut buf);

        match res {
            Ok(0) => {
                // Reached EOF
                printer.on_eof();
                return Ok(());
            }
            Ok(_) => {
                let line = buf.trim_end_matches(['\r', '\n']);
                for pattern in patterns {
                    for m in pattern.find_iter(line) {
                        printer.print(file_path, line_number, line, &m);
                    }
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
                // non-UTF8 file contents - skip rather than aborting the whole walk
                return Ok(());
            }
            Err(err) => return Err(Error::Io(err)),
        }

        buf.clear();
        line_number = line_number + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct RecordedMatch {
        line_no: u64,
        line: String,
        match_start: usize,
        match_end: usize,
    }

    struct RecordingPrinter {
        matches: Vec<RecordedMatch>,
        eof_count: u32,
    }

    impl RecordingPrinter {
        fn new() -> Self {
            Self {
                matches: Vec::new(),
                eof_count: 0,
            }
        }
    }

    impl Printer for RecordingPrinter {
        fn print(&mut self, _file_path: &str, line_no: u64, line: &str, m: &regex::Match) {
            self.matches.push(RecordedMatch {
                line_no,
                line: line.to_string(),
                match_start: m.start(),
                match_end: m.end(),
            });
        }

        fn on_eof(&mut self) {
            self.eof_count += 1;
        }
    }

    fn run_reader(pattern: &str, input: &str) -> RecordingPrinter {
        let re = Regex::new(pattern).unwrap();
        let mut printer = RecordingPrinter::new();
        search_reader(&[re], "<test>", Cursor::new(input.as_bytes()), &mut printer).unwrap();
        printer
    }

    #[test]
    fn line_numbers_start_at_one_and_increment() {
        let printer = run_reader("foo", "foo\nbar\nfoo\n");
        let lines: Vec<u64> = printer.matches.iter().map(|m| m.line_no).collect();
        assert_eq!(lines, vec![1, 3]);
    }

    #[test]
    fn trims_both_lf_and_crlf_line_endings() {
        let printer = run_reader("foo", "foo\r\nfoo\n");
        assert_eq!(printer.matches.len(), 2);
        assert_eq!(printer.matches[0].line, "foo");
        assert_eq!(printer.matches[1].line, "foo");
    }

    #[test]
    fn scans_last_line_without_trailing_newline() {
        let printer = run_reader("bar", "foo\nbar");
        assert_eq!(printer.matches.len(), 1);
        assert_eq!(printer.matches[0].line_no, 2);
    }

    #[test]
    fn reports_match_start_and_end_offsets() {
        let printer = run_reader("world", "hello world\n");
        assert_eq!(printer.matches.len(), 1);
        assert_eq!(printer.matches[0].match_start, 6);
        assert_eq!(printer.matches[0].match_end, 11);
    }

    #[test]
    fn non_matching_lines_are_not_printed() {
        let printer = run_reader("zzz", "foo\nbar\n");
        assert_eq!(printer.matches.len(), 0);
    }

    #[test]
    fn eof_is_reported_exactly_once() {
        let printer = run_reader("foo", "foo\nfoo\nfoo\n");
        assert_eq!(printer.eof_count, 1);
    }

    #[test]
    fn empty_input_still_reports_eof_with_no_matches() {
        let printer = run_reader("foo", "");
        assert_eq!(printer.eof_count, 1);
        assert_eq!(printer.matches.len(), 0);
    }
}
