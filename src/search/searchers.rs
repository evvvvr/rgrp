use regex::bytes::Regex;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::line_reader::{ReadOptions, ReadResult, read_lines};
use crate::matcher::{Options as MatchOptions, match_line};
use crate::printer::Printer;

use super::classify::CompressionFormat;
use super::{Error, Options, Outcome, SearchResult, search_reader};

pub(super) fn search_tarball(
    patterns: &[Regex],
    path: &Path,
    options: &Options,
    printer: &mut dyn Printer,
) -> SearchResult {
    let listing = Command::new("tar").arg("-tf").arg(path).output()?;
    let entry_names: std::collections::VecDeque<String> = String::from_utf8_lossy(&listing.stdout)
        .lines()
        .map(|s| s.to_string())
        .collect();

    let mut results: Vec<SearchResult> = Vec::new();
    for entry in entry_names {
        let res = search_compressed_tarball_entry(patterns, path, &entry, options, printer);
        results.push(res);
    }

    if results.iter().any(|result| result.is_err()) {
        return SearchResult::Err(Error::GenericError);
    }

    SearchResult::Ok(Outcome::Done)
}

fn search_compressed_tarball_entry(
    patterns: &[Regex],
    path: &Path,
    entry: &str,
    options: &Options,
    printer: &mut dyn Printer,
) -> SearchResult {
    let mut cmd = Command::new("tar")
        .arg("-xOf")
        .arg(path)
        .arg(entry)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    let child_output = cmd.stdout.take().unwrap();
    let archive_prefixed_file_path = path.to_string_lossy().to_string() + ":" + entry;

    let res = search_reader(
        patterns,
        &archive_prefixed_file_path,
        options,
        child_output,
        printer,
    );

    let _ = cmd.wait();

    printer.on_eof();

    res
}

/// A single file compressed on its own (`access.log.gz`, `notes.txt.bz2`,
/// ...) rather than a compressed tarball - the latter is `search_tarball`'s
/// job, and `classify` steers those there instead of here.
pub(super) fn search_compressed(
    patterns: &[Regex],
    path: &Path,
    options: &Options,
    printer: &mut dyn Printer,
    format: CompressionFormat,
) -> SearchResult {
    let (program, flag) = format.decompressor();

    let mut cmd = Command::new(program)
        .arg(flag)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    let child_output = cmd.stdout.take().unwrap();

    let res = search_reader(
        patterns,
        &path.to_string_lossy(),
        options,
        child_output,
        printer,
    );

    let _ = cmd.wait();

    res
}

pub(super) fn search_zip(
    patterns: &[Regex],
    path: &Path,
    options: &Options,
    printer: &mut dyn Printer,
) -> SearchResult {
    //  ZIP stdout:
    //  Archive:  input/node_modules.zip
    //extracting: ./emil_claims/node_modules/
    //inflating: ./emil_claims/node_modules/jest-runtime/LICENSE

    // `unzip -bc`'s own "extracting:"/"inflating:" progress lines
    // are not a reliable source for member names: sometimes
    // the filename unzip writes into that progress line is missing
    // its first character (a quirk of unzip's own output, not of
    // how we parse it), and the "extracting:" line (used for
    // stored/uncompressed members) carries no code path to record a
    // name at all. `-Z1` gives plain member names, one per line, in
    // the same order the archive's members are extracted in, so
    // each progress line here just claims the next one instead of
    // trying to parse a name out of its own text.
    let listing = Command::new("unzip").arg("-Z1").arg(path).output()?;
    let mut member_names: std::collections::VecDeque<String> =
        String::from_utf8_lossy(&listing.stdout)
            .lines()
            .map(|s| s.to_string())
            .collect();

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
    // Whether the member currently being read looks binary - set the
    // first time one of its lines carries a null byte, and cleared
    // at the next member boundary. Scoped to the member rather than
    // the whole archive, so one binary member doesn't silence
    // matches already found in earlier text members, and checked
    // without ever stopping the read, so the child's stdout always
    // keeps draining instead of deadlocking on a full pipe.
    let mut current_member_is_binary = false;

    let mut read_line = |line: &[u8]| {
        if line.starts_with(b"Archive: ") {
            // beginning of an archive
        } else if line.starts_with(b" extracting: ") || line.starts_with(b"  inflating: ") {
            printer.on_eof(); // Prev file ended
            // beginning of file output
            current_file_path = member_names.pop_front().unwrap_or_default();
            current_file_line_no = 1;
            current_member_is_binary = false;
            archive_prefixed_file_path =
                path.to_string_lossy().to_string() + ":" + &current_file_path;
        } else {
            current_file_line_no += 1;

            if !options.search_binary && line.contains(&0) {
                current_member_is_binary = true;
            }
            if current_member_is_binary {
                return;
            }

            if let Some(m) = match_line(
                patterns,
                &MatchOptions {
                    invert_matches: options.invert_matches,
                },
                line,
            ) {
                printer.print(
                    &archive_prefixed_file_path,
                    current_file_line_no,
                    line,
                    &m.matches,
                );
            }
        }
    };

    let res = read_lines(
        &mut child_output,
        ReadOptions {
            // Binary members are skipped individually, by
            // `current_member_is_binary` above - the read itself
            // must always run to completion so the child's stdout
            // keeps draining no matter what any one member
            // contains.
            skip_on_null: false,
            crlf: options.crlf,
        },
        &mut read_line,
    );

    let _ = cmd.wait();

    match res {
        Ok(ReadResult::FileRead) => {
            printer.on_eof();
            Ok(Outcome::Done)
        }
        Ok(ReadResult::BinaryFileSkipped) => {
            // Unreachable with skip_on_null: false above, but kept
            // for exhaustiveness.
            Ok(Outcome::BinarySkipped)
        }
        Err(err) => Err(Error::LineRead(err)),
    }
}

pub(super) fn search_plain(
    patterns: &[Regex],
    path: &Path,
    options: &Options,
    printer: &mut dyn Printer,
) -> SearchResult {
    let file = fs::File::open(path)?;
    search_reader(patterns, &path.to_string_lossy(), options, file, printer)
}
