use std::fmt::Write;

use owo_colors::OwoColorize;

/// Turns one matched line into whatever a printer's output format looks
/// like - purely a rendering concern. Buffering, flushing and deciding
/// when output actually gets sent anywhere live in `FlushingPrinter`
/// instead, built on top of this rather than part of it.
///
/// One printer instance is created per file, so implementations can hold
/// per-file state (e.g. "have I already printed this file's name?") -
/// `reset` is called once that file (or archive member) is done, so the
/// next one starts fresh.
pub trait Printer: Send + Sync {
    /// Renders a single line that had at least one match, as the text
    /// that should be appended to this file's output. `matches` holds
    /// every match on the line, sorted by start offset, so implementations
    /// can render the whole line in one shot instead of being called once
    /// per match.
    fn render_line(
        &mut self,
        file_path: &str,
        line_no: u64,
        line: &[u8],
        matches: &[(usize, usize)],
    ) -> String;

    /// Clears any per-file state (e.g. "have I already printed this
    /// file's name?"), since whatever comes next - another file, or
    /// more output for this same file after a discarded binary member -
    /// starts fresh.
    fn reset(&mut self);
}

/// How large a buffered-but-not-yet-emitted chunk is allowed to grow
/// before `FlushingPrinter` sends it on early, mid-file - otherwise a
/// single file (or archive member) with a huge number of matches would
/// hold all of them in memory until it finally ends, regardless of how
/// the rest of a search is already bounded.
const FLUSH_THRESHOLD_BYTES: usize = 1 << 20; // 1 MiB

/// Wraps a `Printer` with the policy of when its rendered lines actually
/// get sent to `emit`.
///
/// One instance covers one *path* (which may itself contain many
/// files/members, e.g. an archive) - `finish` marks the end of each
/// individual file/member within it, not the end of the whole thing.
pub struct FlushingPrinter<'a> {
    printer: &'a mut dyn Printer,
    emit: &'a mut dyn FnMut(String),
    buffer: String,
    // Once a size-triggered flush has actually sent something for the
    // *current* file/member, there's no taking it back - a later binary
    // detection can no longer discard everything for it, only what's
    // accumulated since. Reset by `finish`/`discard`, which both mark a
    // file/member boundary.
    already_emitted: bool,
}

impl<'a> FlushingPrinter<'a> {
    pub fn new(printer: &'a mut dyn Printer, emit: &'a mut dyn FnMut(String)) -> Self {
        Self {
            printer,
            emit,
            buffer: String::new(),
            already_emitted: false,
        }
    }

    /// Renders and buffers one matched line, sending the buffer on early
    /// if that pushes it past the size threshold.
    pub fn print_line(&mut self, file_path: &str, line_no: u64, line: &[u8], matches: &[(usize, usize)]) {
        let rendered = self.printer.render_line(file_path, line_no, line, matches);
        self.buffer.push_str(&rendered);

        if self.buffer.len() >= FLUSH_THRESHOLD_BYTES {
            let chunk = std::mem::take(&mut self.buffer);
            if !chunk.is_empty() {
                self.already_emitted = true;
                (self.emit)(chunk);
            }
        }
    }

    /// Marks the end of one file/member. If it turned out binary and
    /// nothing had already been sent for it, discards whatever's
    /// buffered, since it's incomplete and shouldn't be shown; otherwise
    /// flushes and emits whatever's left - once some of a file's output
    /// has already gone out, there's no retracting it, so this finishes
    /// with the rest instead of discarding on top of what can't be
    /// undone anyway.
    pub fn finish(&mut self, is_binary: bool) {
        if is_binary && !self.already_emitted {
            self.buffer.clear();
        } else {
            let chunk = std::mem::take(&mut self.buffer);
            if !chunk.is_empty() {
                (self.emit)(chunk);
            }
        }

        self.already_emitted = false;
        self.printer.reset();
    }

    /// Unconditionally discards whatever's buffered for the current
    /// file/member, regardless of whether anything was already emitted -
    /// for a genuine read error, where there's no "finish with what's
    /// left" to fall back to, only "stop".
    pub fn discard(&mut self) {
        self.buffer.clear();
        self.already_emitted = false;
        self.printer.reset();
    }
}

/// Renders matches with ANSI colors (filename in purple/bold, matched text
/// in red), for output going to an actual terminal.
pub struct ColorPrinter {
    // assuming one printer per file
    has_printed_filename: bool,
}

impl ColorPrinter {
    /// Creates a printer for a single file, with nothing printed yet.
    pub fn new() -> Self {
        Self {
            has_printed_filename: false,
        }
    }
}

impl Default for ColorPrinter {
    fn default() -> Self {
        Self::new()
    }
}

impl Printer for ColorPrinter {
    fn render_line(
        &mut self,
        file_path: &str,
        line_no: u64,
        line: &[u8],
        matches: &[(usize, usize)],
    ) -> String {
        let mut output = String::new();

        if !self.has_printed_filename {
            writeln!(output).expect("Error writing output");
            writeln!(output, "{}", file_path.bold().purple()).expect("Error writing output");
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

        let _ = writeln!(output, "{}: {}", line_no.green(), highlighted);
        output
    }

    fn reset(&mut self) {
        self.has_printed_filename = false;
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
#[derive(Default)]
pub struct MonochromePrinter;

impl MonochromePrinter {
    /// Creates a printer for a single file.
    pub fn new() -> Self {
        Self
    }
}

impl Printer for MonochromePrinter {
    fn render_line(
        &mut self,
        file_path: &str,
        _line_no: u64,
        line: &[u8],
        _matches: &[(usize, usize)],
    ) -> String {
        let mut output = String::new();
        writeln!(output, "{}: {}", file_path, String::from_utf8_lossy(line))
            .expect("Error writing output");
        output
    }

    fn reset(&mut self) {
        // nop - no per-file state to clear
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

        let output = strip_ansi(&printer.render_line("f", 1, line, &matches));

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

        let output = strip_ansi(&printer.render_line("f", 1, &line, &matches));

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

        let output = strip_ansi(&printer.render_line("f", 1, &line, &matches));

        // The whole character must survive, highlighted as one unit,
        // instead of the match being silently dropped by an end boundary
        // that snapped backward past the start.
        assert_eq!(output.matches('\u{FFFD}').count(), 0);
        assert!(output.contains("a€"));
    }

    #[test]
    fn render_line_only_prints_the_filename_header_once_per_file() {
        let mut printer = ColorPrinter::new();
        let first = strip_ansi(&printer.render_line("myfile", 1, b"foo", &[(0, 3)]));
        let second = strip_ansi(&printer.render_line("myfile", 2, b"bar", &[(0, 3)]));

        assert!(first.contains("myfile"));
        assert!(!second.contains("myfile"));
    }

    #[test]
    fn reset_makes_the_next_render_line_print_its_own_filename_header() {
        let mut printer = ColorPrinter::new();
        printer.render_line("first", 1, b"foo", &[(0, 3)]);
        printer.reset();

        let output = strip_ansi(&printer.render_line("second", 1, b"foo", &[(0, 3)]));
        assert!(output.contains("second"));
    }

    fn collect_emitted(f: impl FnOnce(&mut dyn FnMut(String))) -> Vec<String> {
        let mut emitted = Vec::new();
        let mut emit = |chunk: String| emitted.push(chunk);
        f(&mut emit);
        emitted
    }

    #[test]
    fn flushing_printer_sends_nothing_until_finish_for_small_output() {
        let mut printer = ColorPrinter::new();
        let emitted = collect_emitted(|emit| {
            let mut flushing = FlushingPrinter::new(&mut printer, emit);
            flushing.print_line("f", 1, b"foo", &[(0, 3)]);
            flushing.finish(false);
        });

        assert_eq!(emitted.len(), 1);
        assert!(strip_ansi(&emitted[0]).contains("foo"));
    }

    #[test]
    fn flushing_printer_discards_binary_output_that_was_never_emitted() {
        let mut printer = ColorPrinter::new();
        let emitted = collect_emitted(|emit| {
            let mut flushing = FlushingPrinter::new(&mut printer, emit);
            flushing.print_line("f", 1, b"foo", &[(0, 3)]);
            flushing.finish(true);
        });

        assert!(emitted.is_empty());
    }

    #[test]
    fn flushing_printer_cannot_retract_an_early_flush_so_finishes_with_the_rest_instead() {
        let mut printer = ColorPrinter::new();
        let emitted = collect_emitted(|emit| {
            let mut flushing = FlushingPrinter::new(&mut printer, emit);
            // Force an early flush by pushing a single line past the
            // threshold, rather than depending on the real constant.
            let huge = vec![b'x'; FLUSH_THRESHOLD_BYTES];
            flushing.print_line("f", 1, &huge, &[]);
            flushing.print_line("f", 2, b"more", &[(0, 4)]);
            // Binary found only after that early flush already went out -
            // nothing left to retract, so this should still emit the rest.
            flushing.finish(true);
        });

        assert_eq!(emitted.len(), 2);
        assert!(strip_ansi(&emitted[1]).contains("more"));
    }

    #[test]
    fn flushing_printer_discard_sends_nothing_regardless_of_prior_emits() {
        let mut printer = ColorPrinter::new();
        let emitted = collect_emitted(|emit| {
            let mut flushing = FlushingPrinter::new(&mut printer, emit);
            flushing.print_line("f", 1, b"foo", &[(0, 3)]);
            flushing.discard();
        });

        assert!(emitted.is_empty());
    }
}
