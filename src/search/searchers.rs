use regex::bytes::Regex;
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::line_reader::{ReadOptions, ReadResult, read_lines};
use crate::matcher::{Options as MatchOptions, match_line};
use crate::printer::Printer;

use super::classify::CompressionFormat;
use super::{Error, FLUSH_THRESHOLD_BYTES, Options, Outcome, SearchResult, search_reader};

/// One member of a tarball, as reported by `tar -tvf`'s verbose listing.
struct TarballEntry {
    name: String,
    /// How many content bytes this member contributes to `tar -xOf`'s
    /// single concatenated stdout stream - `0` for anything without
    /// content of its own (directories, symlinks, ...).
    size: u64,
}

/// Parses one line of `tar -tvf`'s verbose listing, e.g.:
/// `-rw-r--r--  0 user  staff  1234 Aug 27 2025 some/path.txt`
///
/// This is bsdtar's column layout (permissions, links, owner, group,
/// size, month, day, time-or-year, then the name - which may itself
/// contain spaces, so it's whatever's left after those 8 fixed fields).
/// GNU tar's verbose listing combines owner/group into a single
/// `user/group` field instead, which this does not account for.
fn parse_verbose_tar_line(line: &str) -> Option<TarballEntry> {
    let mut fields = line.split_whitespace();

    // Only a regular file ('-') ever contributes bytes to `tar -xOf`'s
    // output - directories, symlinks, and other special types always
    // produce zero, regardless of what their own size field says (e.g. a
    // symlink's "size" is the length of its target string, not content
    // `tar -xOf` will ever emit).
    let is_regular_file = fields.next()?.starts_with('-');

    let _links = fields.next()?;
    let _owner = fields.next()?;
    let _group = fields.next()?;
    let size: u64 = fields.next()?.parse().ok()?;
    let _month = fields.next()?;
    let _day = fields.next()?;
    let _time_or_year = fields.next()?;

    let name: String = fields.collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return None;
    }

    Some(TarballEntry {
        name,
        size: if is_regular_file { size } else { 0 },
    })
}

fn list_tarball_entries(path: &Path) -> Result<Vec<TarballEntry>, Error> {
    let listing = Command::new("tar").arg("-tvf").arg(path).output()?;
    Ok(String::from_utf8_lossy(&listing.stdout)
        .lines()
        .filter_map(parse_verbose_tar_line)
        .collect())
}

/// Searches every member of a tarball in one single pass: one `tar`
/// process decompresses and extracts the whole archive once, with every
/// member's content concatenated onto its stdout in archive order, and
/// this reads each member's exact byte count (from `list_tarball_entries`)
/// off that one shared stream before moving to the next.
///
/// Earlier, each member was extracted by its own separate `tar -xOf`
/// call, which - for a compressed archive - has to decompress from the
/// start every time to reach that member; across many thousands of
/// members, that's effectively quadratic in the archive's member count.
/// Reading everything out of one streaming pass instead is linear.
pub(super) fn search_tarball(
    patterns: &[Regex],
    path: &Path,
    options: &Options,
    printer: &mut dyn Printer,
    emit: &mut dyn FnMut(String),
) -> SearchResult {
    let entries = list_tarball_entries(path)?;

    let mut cmd = Command::new("tar")
        .arg("-xOf")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;

    let mut child_output = cmd.stdout.take().expect("just configured with Stdio::piped()");
    // The first error hit, if any - kept (rather than discarded in favor
    // of a generic placeholder) so the caller finds out what actually
    // went wrong, not just that something did.
    let mut first_error: Option<Error> = None;

    for entry in entries {
        if entry.size == 0 {
            continue;
        }

        let archive_prefixed_file_path = path.to_string_lossy().to_string() + ":" + &entry.name;

        // Bound to exactly this member's byte count, so `search_reader`
        // (and the `read_lines` it streams through internally) never
        // sees past this member's own content, regardless of how large
        // the member itself is - memory stays bounded by `read_lines`'s
        // own chunk size, not by the member's size.
        let mut member_reader = (&mut child_output).take(entry.size);

        // `search_reader` flushes this member's output to `emit` itself
        // on a clean finish (or discards it if the member was binary),
        // so each member's share of the archive's output is sent on as
        // soon as it's done instead of accumulating for the whole
        // archive.
        let res = search_reader(
            patterns,
            &archive_prefixed_file_path,
            options,
            &mut member_reader,
            printer,
            emit,
        );

        // `search_reader` may have stopped early (e.g. the member
        // turned out binary), leaving some of this member's bytes still
        // sitting unread on the shared stream - drain them now, in
        // small chunks rather than all at once, so the next member
        // starts reading at its own correct offset.
        if let Err(drain_err) = io::copy(&mut member_reader, &mut io::sink()) {
            first_error.get_or_insert(Error::from(drain_err));
            break;
        }

        if let Err(err) = res {
            first_error.get_or_insert(err);
        }
    }

    let _ = cmd.wait();

    if let Some(err) = first_error {
        return Err(err);
    }

    Ok(Outcome::Done)
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
    emit: &mut dyn FnMut(String),
) -> SearchResult {
    let (program, flag) = format.decompressor();

    let mut cmd = Command::new(program)
        .arg(flag)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;

    let child_output = cmd.stdout.take().expect("just configured with Stdio::piped()");

    let res = search_reader(
        patterns,
        &path.to_string_lossy(),
        options,
        child_output,
        printer,
        emit,
    );

    let _ = cmd.wait();

    res
}

pub(super) fn search_zip(
    patterns: &[Regex],
    path: &Path,
    options: &Options,
    printer: &mut dyn Printer,
    emit: &mut dyn FnMut(String),
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
        .spawn()?;

    let mut child_output = cmd.stdout.take().expect("just configured with Stdio::piped()");

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
    // Reused across every line of every member in this archive instead
    // of allocated fresh per line - `match_line` clears and refills it
    // each call.
    let mut line_matches = Vec::new();

    let mut read_line = |line: &[u8]| {
        if line.starts_with(b"Archive: ") {
            // beginning of an archive
        } else if line.starts_with(b" extracting: ") || line.starts_with(b"  inflating: ") {
            // Previous member ended - flush its output, unless it turned
            // out to be binary, in which case discard whatever partial
            // output it buffered before that was detected.
            if current_member_is_binary {
                printer.discard();
            } else {
                let output = printer.flush();
                if !output.is_empty() {
                    emit(output);
                }
            }
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

            let matched = match_line(
                patterns,
                &MatchOptions {
                    invert_matches: options.invert_matches,
                },
                line,
                &mut line_matches,
            );

            if matched {
                printer.print(
                    &archive_prefixed_file_path,
                    current_file_line_no,
                    line,
                    &line_matches,
                );

                if printer.buffered_len() >= FLUSH_THRESHOLD_BYTES {
                    let chunk = printer.drain();
                    if !chunk.is_empty() {
                        emit(chunk);
                    }
                }
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
            // The last member in the archive never hit another
            // "extracting:"/"inflating:" marker to flush it, so do that
            // here instead.
            if current_member_is_binary {
                printer.discard();
            } else {
                let output = printer.flush();
                if !output.is_empty() {
                    emit(output);
                }
            }
            Ok(Outcome::Done)
        }
        Ok(ReadResult::BinaryFileSkipped) => {
            // Unreachable with skip_on_null: false above, but kept
            // for exhaustiveness.
            printer.discard();
            Ok(Outcome::BinarySkipped)
        }
        Err(err) => {
            printer.discard();
            Err(Error::LineRead(err))
        }
    }
}

pub(super) fn search_plain(
    patterns: &[Regex],
    path: &Path,
    options: &Options,
    printer: &mut dyn Printer,
    emit: &mut dyn FnMut(String),
) -> SearchResult {
    let file = fs::File::open(path)?;
    search_reader(
        patterns,
        &path.to_string_lossy(),
        options,
        file,
        printer,
        emit,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_regular_file_line() {
        let entry =
            parse_verbose_tar_line("-rw-r--r--  0 voga   staff  786671 Aug 27  2025 node_modules/.package-lock.json")
                .unwrap();
        assert_eq!(entry.name, "node_modules/.package-lock.json");
        assert_eq!(entry.size, 786671);
    }

    #[test]
    fn parses_a_file_name_with_spaces() {
        let entry =
            parse_verbose_tar_line("-rw-r--r--  0 voga   staff     123 Aug 27  2025 some dir/a file.txt")
                .unwrap();
        assert_eq!(entry.name, "some dir/a file.txt");
        assert_eq!(entry.size, 123);
    }

    #[test]
    fn a_directory_always_has_zero_size_regardless_of_its_listed_size() {
        let entry =
            parse_verbose_tar_line("drwxr-xr-x  0 voga   staff       0 Aug 27  2025 node_modules/")
                .unwrap();
        assert_eq!(entry.name, "node_modules/");
        assert_eq!(entry.size, 0);
    }

    #[test]
    fn a_symlink_always_has_zero_size_even_though_its_listed_size_is_its_target_length() {
        let entry = parse_verbose_tar_line(
            "lrwxr-xr-x  0 voga   staff      20 Aug 26  2025 node_modules/some-link -> ../apps/some-app",
        )
        .unwrap();
        assert_eq!(entry.size, 0);
    }

    #[test]
    fn an_unparseable_line_is_skipped() {
        assert!(parse_verbose_tar_line("").is_none());
        assert!(parse_verbose_tar_line("not a tar listing line").is_none());
    }
}
