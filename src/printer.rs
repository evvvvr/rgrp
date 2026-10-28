use std::fmt::Write;

use owo_colors::OwoColorize;

/// Turns matched lines into whatever a printer's output format looks like.
///
/// One printer instance is created per file, so implementations can hold
/// per-file state (e.g. "have I already printed this file's name?").
pub trait Printer: Send + Sync {
    /// Reports a single line that had at least one match. `matches` holds
    /// every match on the line, sorted by start offset, so implementations
    /// can render the whole line in one shot instead of being called once
    /// per match.
    fn print(&mut self, file_path: &str, line_no: u64, line: &[u8], matches: &[(usize, usize)]);

    /// Called once a file (or one member of an archive) has been read to
    /// the end cleanly - takes and returns everything buffered for it
    /// since the last `flush`/`discard`, so the caller can emit that
    /// chunk right away instead of holding it until an entire archive of
    /// many members is done.
    fn flush(&mut self) -> String;

    /// Called when a file (or archive member) turned out to be binary
    /// partway through, or failed to read - clears whatever was buffered
    /// for it instead of returning it, since it's incomplete and
    /// shouldn't be shown.
    fn discard(&mut self);
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

impl Default for ColorPrinter {
    fn default() -> Self {
        Self::new()
    }
}

impl Printer for ColorPrinter {
    fn print(&mut self, file_path: &str, line_no: u64, line: &[u8], matches: &[(usize, usize)]) {
        if !self.has_printed_filename {
            writeln!(self.output).expect("Error writing output");
            writeln!(self.output, "{}", file_path.bold().purple()).expect("Error writing output");
            self.has_printed_filename = true;
        }

        let mut highlighted = String::new();
        let mut printed_byte_index = 0;

        for m in matches {
            // Widen the match to whichever whole characters it touches,
            // rather than decoding each half of a split multi-byte
            // character on its own - two independent `from_utf8_lossy`
            // calls on either side of such a split would each see an
            // incomplete sequence and print a replacement character for
            // it, turning one character into two mangled ones.
            let (start, end) = *m;
            let start = snap_start_to_char_boundary(line, start.max(printed_byte_index));
            let end = snap_end_to_char_boundary(line, end);

            if start >= end {
                // Fully covered by a previous match already printed -
                // patterns can overlap, and printing this one too would
                // duplicate that stretch of the line.
                continue;
            }

            if start > printed_byte_index {
                highlighted.push_str(&String::from_utf8_lossy(&line[printed_byte_index..start]));
            }
            highlighted.push_str(&String::from_utf8_lossy(&line[start..end]).red().to_string());
            printed_byte_index = end;
        }

        if printed_byte_index < line.len() {
            highlighted.push_str(&String::from_utf8_lossy(&line[printed_byte_index..]));
        }

        let _ = writeln!(self.output, "{}: {}", line_no.green(), highlighted);
    }

    fn flush(&mut self) -> String {
        self.has_printed_filename = false;
        std::mem::take(&mut self.output)
    }

    fn discard(&mut self) {
        self.has_printed_filename = false;
        self.output.clear();
    }
}

/// A continuation byte of a multi-byte UTF-8 sequence (`10xxxxxx`) - never
/// the first byte of a character.
fn is_continuation_byte(b: u8) -> bool {
    b & 0xC0 == 0x80
}

/// Walks a match-start offset back to the start of whatever UTF-8 sequence
/// it's inside of (if any), so a boundary landing mid-character widens
/// outward to the whole character instead of splitting it.
fn snap_start_to_char_boundary(line: &[u8], mut idx: usize) -> usize {
    while idx > 0 && idx < line.len() && is_continuation_byte(line[idx]) {
        idx -= 1;
    }
    idx
}

/// The end-offset counterpart of [`snap_start_to_char_boundary`] - widens
/// forward, past the sequence's last continuation byte, since an end
/// offset that lands mid-character means the match stops partway through
/// it rather than before it.
fn snap_end_to_char_boundary(line: &[u8], mut idx: usize) -> usize {
    while idx < line.len() && is_continuation_byte(line[idx]) {
        idx += 1;
    }
    idx
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

impl Default for MonochromePrinter {
    fn default() -> Self {
        Self::new()
    }
}

impl Printer for MonochromePrinter {
    fn print(&mut self, file_path: &str, _line_no: u64, line: &[u8], _matches: &[(usize, usize)]) {
        writeln!(
            self.output,
            "{}: {}",
            file_path,
            String::from_utf8_lossy(line)
        )
        .expect("Error writing output");
    }

    fn flush(&mut self) -> String {
        std::mem::take(&mut self.output)
    }

    fn discard(&mut self) {
        self.output.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::bytes::RegexBuilder;

    /// Strips ANSI escape sequences so assertions can check the plain text
    /// `ColorPrinter` wraps them around.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for c2 in chars.by_ref() {
                    if c2.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn overlapping_matches_do_not_duplicate_text() {
        let mut printer = ColorPrinter::new();
        let line = b"foo bar";

        let foo = RegexBuilder::new("foo").build().unwrap();
        let oo = RegexBuilder::new("oo").build().unwrap();
        let matches: Vec<(usize, usize)> = foo
            .find_iter(line)
            .chain(oo.find_iter(line))
            .map(|m| (m.start(), m.end()))
            .collect();

        printer.print("f", 1, line, &matches);
        let output = strip_ansi(&printer.flush());

        assert!(output.contains("foo bar"));
        assert!(!output.contains("foooo"));
    }

    #[test]
    fn match_that_splits_a_multibyte_char_does_not_show_two_replacement_chars() {
        let mut printer = ColorPrinter::new();
        // "aéb" as UTF-8: 'a' (0x61), é (0xC3 0xA9), 'b' (0x62). The pattern
        // below (Unicode mode off) matches the lone continuation byte of é,
        // landing the match squarely inside that character.
        let line = [0x61, 0xC3, 0xA9, 0x62];
        let pattern = RegexBuilder::new(r"\xA9").unicode(false).build().unwrap();
        let matches: Vec<(usize, usize)> = pattern
            .find_iter(&line)
            .map(|m| (m.start(), m.end()))
            .collect();
        assert_eq!(matches[0], (2, 3));

        printer.print("f", 1, &line, &matches);
        let output = strip_ansi(&printer.flush());

        assert_eq!(output.matches('\u{FFFD}').count(), 0);
        assert!(output.contains("aéb"));
    }

    #[test]
    fn match_ending_mid_character_widens_forward_instead_of_being_dropped() {
        let mut printer = ColorPrinter::new();
        // "a€" as UTF-8: 'a' (0x61), € (0xE2 0x82 0xAC). The pattern matches
        // only the lead byte of €, so the match's end (2) lands on the
        // second byte of that 3-byte sequence - squarely mid-character.
        let line = [0x61, 0xE2, 0x82, 0xAC];
        let pattern = RegexBuilder::new(r"\xE2").unicode(false).build().unwrap();
        let matches: Vec<(usize, usize)> = pattern
            .find_iter(&line)
            .map(|m| (m.start(), m.end()))
            .collect();
        assert_eq!(matches[0], (1, 2));

        printer.print("f", 1, &line, &matches);
        let output = strip_ansi(&printer.flush());

        // The whole character must survive, highlighted as one unit,
        // instead of the match being silently dropped by an end boundary
        // that snapped backward past the start.
        assert_eq!(output.matches('\u{FFFD}').count(), 0);
        assert!(output.contains("a€"));
    }

    #[test]
    fn discard_clears_buffered_output_without_returning_it() {
        let mut printer = ColorPrinter::new();
        printer.print("f", 1, b"foo", &[(0, 3)]);

        printer.discard();

        assert_eq!(printer.flush(), "");
    }

    #[test]
    fn flush_resets_state_so_the_next_file_prints_its_own_filename_header() {
        let mut printer = ColorPrinter::new();
        printer.print("first", 1, b"foo", &[(0, 3)]);
        printer.flush();

        printer.print("second", 1, b"foo", &[(0, 3)]);
        let output = strip_ansi(&printer.flush());

        assert!(output.contains("second"));
    }
}
