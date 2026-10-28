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

    /// How many bytes are currently buffered, waiting for the next
    /// `flush`/`discard`/`drain` - lets a caller decide when a single
    /// file's own output has grown large enough to send on early via
    /// `drain`, rather than letting it grow unboundedly until the file
    /// (or archive member) finally ends.
    fn buffered_len(&self) -> usize;

    /// Takes and returns whatever is currently buffered, same as `flush`,
    /// but - unlike `flush` - leaves per-file state (e.g. "have I already
    /// printed this file's name?") untouched, since the file isn't done:
    /// this is a size-triggered early flush mid-file, not an end-of-file
    /// one, so the next chunk is still the same file's continuation
    /// rather than a new one that needs its own header.
    fn drain(&mut self) -> String;
}

/// A plain, growable text buffer that accumulates a printer's rendered
/// output until something decides it's time to let it go. `ColorPrinter`
/// and `MonochromePrinter` both delegate their `Printer::flush`/`discard`/
/// `buffered_len`/`drain` to this instead of each hand-rolling the same
/// `std::mem::take`/`clear`/`len` on a `String` - the only thing that
/// actually differs between the two printers is how a line gets rendered
/// in the first place, not how the rendered text gets accumulated or
/// evicted.
#[derive(Default)]
struct OutputBuffer(String);

impl OutputBuffer {
    fn len(&self) -> usize {
        self.0.len()
    }

    /// Takes and returns everything accumulated so far.
    fn drain(&mut self) -> String {
        std::mem::take(&mut self.0)
    }

    /// Clears everything accumulated so far without returning it.
    fn discard(&mut self) {
        self.0.clear();
    }
}

impl Write for OutputBuffer {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.write_str(s)
    }
}

/// Renders matches with ANSI colors (filename in purple/bold, matched text
/// in red), for output going to an actual terminal.
pub struct ColorPrinter {
    // assuming one printer per file
    has_printed_filename: bool,
    output: OutputBuffer,
}

impl ColorPrinter {
    /// Creates a printer for a single file, with nothing buffered yet.
    pub fn new() -> Self {
        Self {
            has_printed_filename: false,
            output: OutputBuffer::default(),
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
            // `write!` renders the colored wrapper's `Display` impl
            // straight into `highlighted`, rather than first rendering
            // it into its own throwaway `String` via `.to_string()`.
            let _ = write!(
                highlighted,
                "{}",
                String::from_utf8_lossy(&line[start..end]).red()
            );
            printed_byte_index = end;
        }

        if printed_byte_index < line.len() {
            highlighted.push_str(&String::from_utf8_lossy(&line[printed_byte_index..]));
        }

        let _ = writeln!(self.output, "{}: {}", line_no.green(), highlighted);
    }

    fn flush(&mut self) -> String {
        self.has_printed_filename = false;
        self.output.drain()
    }

    fn discard(&mut self) {
        self.has_printed_filename = false;
        self.output.discard();
    }

    fn buffered_len(&self) -> usize {
        self.output.len()
    }

    fn drain(&mut self) -> String {
        self.output.drain()
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
    output: OutputBuffer,
}

impl MonochromePrinter {
    /// Creates a printer for a single file, with nothing buffered yet.
    pub fn new() -> Self {
        Self {
            output: OutputBuffer::default(),
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
        self.output.drain()
    }

    fn discard(&mut self) {
        self.output.discard();
    }

    fn buffered_len(&self) -> usize {
        self.output.len()
    }

    fn drain(&mut self) -> String {
        self.output.drain()
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

    #[test]
    fn drain_keeps_per_file_state_so_the_next_chunk_skips_the_header() {
        let mut printer = ColorPrinter::new();
        printer.print("myfile", 1, b"foo", &[(0, 3)]);
        let first = strip_ansi(&printer.drain());
        assert!(first.contains("myfile"));

        // Same file, next line - drain shouldn't have reset
        // `has_printed_filename`, so this must not reprint the header.
        printer.print("myfile", 2, b"bar", &[(0, 3)]);
        let second = strip_ansi(&printer.flush());
        assert!(!second.contains("myfile"));
    }

    #[test]
    fn buffered_len_reflects_what_was_just_printed_and_drops_to_zero_after_draining() {
        let mut printer = ColorPrinter::new();
        assert_eq!(printer.buffered_len(), 0);

        printer.print("f", 1, b"foo", &[(0, 3)]);
        assert!(printer.buffered_len() > 0);

        printer.drain();
        assert_eq!(printer.buffered_len(), 0);
    }
}
