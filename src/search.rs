use crate::line_reader::{ReadOptions, ReadResult, read_lines};
use crate::printer::{BufferedOutput, ColorPrinter};
use regex::bytes::Regex;
use std::fs;
use std::io::prelude::*;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::printer::Printer;

/// Something that went wrong while walking or reading a single path.
#[derive(Debug)]
pub enum Error {
    /// A filesystem operation failed (missing path, permissions, etc.).
    Io(std::io::Error),
    /// One of the search patterns failed to compile.
    Pattern(regex::Error),
    /// Reading a file's lines failed partway through.
    LineRead(crate::line_reader::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(err) => write!(f, "{err}"),
            Error::Pattern(err) => write!(f, "invalid search pattern: {err}"),
            Error::LineRead(err) => write!(f, "{err}"),
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

/// What happened when searching a single path - lets the caller decide
/// whether to keep whatever output a printer buffered for it.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Finished normally (including a directory, symlink, or anything
    /// else that never reached a printer); any buffered output is
    /// complete and should be kept.
    Done,
    /// The file looked binary partway through, so whatever a printer
    /// already buffered for it only covers lines read before that point -
    /// incomplete, so it should be discarded rather than shown.
    BinarySkipped,
}

/// Knobs that change how a search behaves, independent of the pattern(s)
/// used or which path is being searched.
#[derive(Clone, Copy, Default)]
pub struct Options {
    /// When true, report lines that *don't* match any pattern instead of
    /// lines that do.
    pub invert_matches: bool,
    /// When true, a file that looks binary is searched as text instead of
    /// being skipped. Mirrors ripgrep's `-a`/`--text`.
    pub search_binary: bool,
    /// When true, a trailing `\r` before the `\n` is treated as part of the
    /// line terminator and stripped, rather than as part of the line
    /// content. Mirrors ripgrep's `--crlf`.
    pub crlf: bool,
}

/// Somewhere a directory's entries can be handed off for someone else to
/// pick up later - the thing that makes a directory walk parallel. `search`
/// only needs to be able to hand work off; it doesn't need to know how
/// that work gets distributed or picked up.
pub trait WorkQueue {
    /// Hands off a batch of newly discovered paths at once (rather than
    /// one at a time), so an implementation that needs to wake up other
    /// workers can do that once per directory instead of once per entry.
    fn enqueue(&self, paths: impl Iterator<Item = PathBuf>);
}

/// Searches one path against `patterns`.
///
/// A file is read line by line and matched directly. A directory isn't
/// recursed into here - its entries are handed to `WorkQueue` for someone to pick up.
pub fn search<T: Printer>(
    patterns: &[Regex],
    path: &Path,
    options: &Options,
    printer: &mut T,
    queue: &impl WorkQueue,
) -> Result<Outcome, Error> {
    // symlink_metadata (unlike metadata) reports the entry's own type instead
    // of following the symlink, so this check actually catches symlinks
    let metadata = fs::symlink_metadata(path)?;

    if metadata.is_symlink() {
        // skip for now - this could lead to infinite cycles
        return Ok(Outcome::Done);
    }

    if metadata.is_dir() {
        let mut entries = Vec::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            entries.push(entry.path());
        }
        queue.enqueue(entries.into_iter());
        return Ok(Outcome::Done);
    }

    if !metadata.is_file() {
        // Something we don't know how to work with
        return Ok(Outcome::Done);
    }

    if let Option::Some(extension) = path.extension() {
        if extension == "zip" {
            let mut cmd = Command::new("unzip")
                .arg("-bc")
                .arg(path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();

            let mut child_output = cmd.stdout.take().unwrap();

            let mut current_file_path = String::new();
            let mut archive_prefixed_file_path = String::new();
            let mut current_file_line_no = 1;
            let mut printer = ColorPrinter::new();

            let mut read_line = |line: &[u8]| {
                if line.starts_with(b"Archive: ") {
                    // beginning of an archive
                    // println!("Archive {}", path.to_string_lossy())
                } else if line.starts_with(b" extracting: ") {
                    // println!("Extracting: {}",  String::from_utf8_lossy(&line[13..]));
                } else if line.starts_with(b"  inflating: ") {
                    let finished_printer = std::mem::replace(&mut printer, ColorPrinter::new());
                    println!("{}", finished_printer.into_output());
                    // beginning of file output
                    current_file_path = String::from_utf8_lossy(&line[13..]).to_string();
                    current_file_line_no = 1;
                    archive_prefixed_file_path =
                        path.to_string_lossy().to_string() + ":" + &current_file_path;
                    // println!("Inflating: {}",  current_file_path);
                } else {
                    current_file_line_no += 1;
                    let mut all_matches: Vec<regex::bytes::Match> = Vec::new();

                    for pattern in patterns {
                        for m in pattern.find_iter(line) {
                            all_matches.push(m);
                        }
                    }

                    if options.invert_matches {
                        // we are interested in lines with zero total matches
                        if all_matches.is_empty() {
                            printer.print_2(
                                &archive_prefixed_file_path,
                                current_file_line_no,
                                line,
                                &all_matches,
                            );
                        }
                    } else if !all_matches.is_empty() {
                        // This allows to walk over all matches for all patterns sequentially
                        all_matches.sort_by_key(|a| a.start());

                        printer.print_2(
                            &archive_prefixed_file_path,
                            current_file_line_no,
                            line,
                            &all_matches,
                        );
                        // println!(
                        //     "{}:{}: {}",
                        //     path.to_string_lossy(),
                        //     current_file_path,
                        //     String::from_utf8_lossy(line)
                        // )
                    }
                }
            };
            // Tarballs/ ZIP is a different problem - rougly same dir tree walking applies
            //  ZIP stdout:
            //  Archive:  input/node_modules.zip
            //extracting: ./emil_claims/node_modules/
            //inflating: ./emil_claims/node_modules/jest-runtime/LICENSE
            let _ = read_lines(
                &mut child_output,
                ReadOptions {
                    skip_on_null: !options.search_binary,
                    crlf: options.crlf,
                },
                &mut read_line,
            );

            let _ = cmd.wait();
        } else if extension == "gz" {
            // additional check if it's a compressed tarball
            if let Option::Some(filename) = path.file_stem().unwrap().to_str() {
                if filename.ends_with(".tar") {
                    // taraballs are harder to work with:
                    // tar doesn't  usually outputs filename before file content in stdout
                    // ssntead it can show entry name in stderr then somehpow it is your job to match
                    // read stderr
                    // if it is "x filename" - set current filename
                    // everuthing in stdout is file ouput until next "x filename"
                    println!("TAR GZ")
                } else {
                    println!("GZ")
                }
            }
        }
    }

    search_file(patterns, path, options, printer)
}

fn search_file<T: Printer>(
    patterns: &[Regex],
    file_path: &Path,
    options: &Options,
    printer: &mut T,
) -> Result<Outcome, Error> {
    let file = fs::File::open(file_path)?;
    search_reader(patterns, file_path, options, file, printer)
}

fn search_reader<T: Printer>(
    patterns: &[Regex],
    file_path: &Path,
    options: &Options,
    mut reader: impl Read,
    printer: &mut T,
) -> Result<Outcome, Error> {
    let mut line_number: u64 = 1;

    let mut on_read_line = |line: &[u8]| {
        let mut all_matches: Vec<regex::bytes::Match> = Vec::new();

        for pattern in patterns {
            for m in pattern.find_iter(line) {
                all_matches.push(m);
            }
        }

        if options.invert_matches {
            // we are interested in lines with zero total matches
            if all_matches.is_empty() {
                printer.print(file_path, line_number, line, &all_matches);
            }
        } else if !all_matches.is_empty() {
            // This allows to walk over all matches for all patterns sequentially
            all_matches.sort_by_key(|a| a.start());

            printer.print(file_path, line_number, line, &all_matches);
        }

        line_number += 1;
    };
    let res = read_lines(
        &mut reader,
        ReadOptions {
            skip_on_null: !options.search_binary,
            crlf: options.crlf,
        },
        &mut on_read_line,
    );

    match res {
        Ok(ReadResult::FileRead) => {
            printer.on_eof();
            Ok(Outcome::Done)
        }
        Ok(ReadResult::BinaryFileSkipped) => Ok(Outcome::BinarySkipped),
        Err(err) => Err(Error::LineRead(err)),
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
        // every line passed to `print`, regardless of how many (if any)
        // matches it carried - needed to observe invert-match behavior,
        // where `print` is called with zero matches
        printed_lines: Vec<String>,
        // how many times `print` itself was called, as opposed to how many
        // matches were reported - a line with several matches must still
        // be reported through a single `print` call
        print_call_count: u32,
        eof_count: u32,
    }

    impl RecordingPrinter {
        fn new() -> Self {
            Self {
                matches: Vec::new(),
                printed_lines: Vec::new(),
                print_call_count: 0,
                eof_count: 0,
            }
        }
    }

    impl Printer for RecordingPrinter {
        fn print(
            &mut self,
            _file_path: &Path,
            line_no: u64,
            line: &[u8],
            matches: &[regex::bytes::Match],
        ) {
            self.print_call_count += 1;
            let line = String::from_utf8_lossy(line).into_owned();
            self.printed_lines.push(line.clone());
            for m in matches {
                self.matches.push(RecordedMatch {
                    line_no,
                    line: line.clone(),
                    match_start: m.start(),
                    match_end: m.end(),
                });
            }
        }

        fn on_eof(&mut self) {
            self.eof_count += 1;
        }
    }

    fn run_reader(pattern: &str, input: &str) -> RecordingPrinter {
        run_reader_multi(&[pattern], input)
    }

    fn run_reader_multi(patterns: &[&str], input: &str) -> RecordingPrinter {
        run_reader_with_options(
            patterns,
            input,
            &Options {
                invert_matches: false,
                ..Default::default()
            },
        )
    }

    fn run_reader_with_options(
        patterns: &[&str],
        input: &str,
        options: &Options,
    ) -> RecordingPrinter {
        let patterns: Vec<Regex> = patterns.iter().map(|p| Regex::new(p).unwrap()).collect();
        let mut printer = RecordingPrinter::new();
        search_reader(
            &patterns,
            Path::new("<test>"),
            options,
            Cursor::new(input.as_bytes()),
            &mut printer,
        )
        .unwrap();
        printer
    }

    #[test]
    fn line_numbers_start_at_one_and_increment() {
        let printer = run_reader("foo", "foo\nbar\nfoo\n");
        let lines: Vec<u64> = printer.matches.iter().map(|m| m.line_no).collect();
        assert_eq!(lines, vec![1, 3]);
    }

    #[test]
    fn by_default_a_trailing_cr_is_kept_as_part_of_the_line() {
        let printer = run_reader("foo", "foo\r\nfoo\n");
        assert_eq!(printer.matches.len(), 2);
        assert_eq!(printer.matches[0].line, "foo\r");
        assert_eq!(printer.matches[1].line, "foo");
    }

    #[test]
    fn crlf_option_trims_the_trailing_cr_from_both_line_endings() {
        let printer = run_reader_with_options(
            &["foo"],
            "foo\r\nfoo\n",
            &Options {
                invert_matches: false,
                search_binary: false,
                crlf: true,
            },
        );
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

    #[test]
    fn multiple_matches_on_one_line_are_reported_in_a_single_call() {
        let printer = run_reader("foo", "foo bar foo\n");
        assert_eq!(printer.print_call_count, 1);

        let offsets: Vec<(usize, usize)> = printer
            .matches
            .iter()
            .map(|m| (m.match_start, m.match_end))
            .collect();
        assert_eq!(offsets, vec![(0, 3), (8, 11)]);
    }

    #[test]
    fn matches_from_different_patterns_on_one_line_are_merged_in_order() {
        let printer = run_reader_multi(&["foo", "bar"], "foo bar\n");
        assert_eq!(printer.print_call_count, 1);

        let offsets: Vec<(usize, usize)> = printer
            .matches
            .iter()
            .map(|m| (m.match_start, m.match_end))
            .collect();
        assert_eq!(offsets, vec![(0, 3), (4, 7)]);
    }

    #[test]
    fn matches_are_sorted_by_start_even_when_patterns_find_them_out_of_order() {
        // "bar" is searched for before "foo" but appears later in the line,
        // so the merged matches must still come out in line order
        let printer = run_reader_multi(&["bar", "foo"], "foo bar\n");

        let offsets: Vec<(usize, usize)> = printer
            .matches
            .iter()
            .map(|m| (m.match_start, m.match_end))
            .collect();
        assert_eq!(offsets, vec![(0, 3), (4, 7)]);
    }

    #[test]
    fn print_is_not_called_once_per_match() {
        let printer = run_reader("foo", "foo foo foo\nfoo\n");
        // 2 lines with matches -> 2 calls, regardless of 4 total matches
        assert_eq!(printer.print_call_count, 2);
    }

    #[test]
    fn invert_matches_reports_only_lines_with_no_match() {
        let printer = run_reader_with_options(
            &["foo"],
            "foo\nbar\nfoo bar\nbaz\n",
            &Options {
                invert_matches: true,
                ..Default::default()
            },
        );

        assert_eq!(printer.printed_lines, vec!["bar", "baz"]);
    }

    #[test]
    fn invert_matches_with_multiple_patterns_excludes_lines_matching_any_pattern() {
        let printer = run_reader_with_options(
            &["foo", "bar"],
            "foo\nbar\nfoo bar\nbaz\n",
            &Options {
                invert_matches: true,
                ..Default::default()
            },
        );

        assert_eq!(printer.printed_lines, vec!["baz"]);
    }

    #[test]
    fn invert_matches_reports_no_match_offsets() {
        let printer = run_reader_with_options(
            &["foo"],
            "bar\n",
            &Options {
                invert_matches: true,
                ..Default::default()
            },
        );

        assert_eq!(printer.matches.len(), 0);
        assert_eq!(printer.print_call_count, 1);
    }
}
