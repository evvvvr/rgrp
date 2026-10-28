use regex::bytes::Regex;
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::line_reader::{ReadOptions, ReadResult, read_lines};
use crate::matcher::{Options as MatchOptions, match_line};
use crate::printer::Printer;

use super::classify::CompressionFormat;
use super::{
    Error, Options, Outcome, SearchResult, finish_output, print_and_maybe_flush, search_reader,
};

/// One member of a tarball, as reported by `tar -tvf`'s verbose listing.
struct TarballEntry {
    name: String,
    /// How many content bytes this member contributes to `tar -xOf`'s
    /// single concatenated stdout stream - `0` for anything without
    /// content of its own (directories, symlinks, ...).
    size: u64,
}

/// Parses one line of `tar -tvf`'s verbose listing. The column layout
/// differs between the two major `tar` implementations, e.g. for the
/// same file:
///
/// ```text
/// bsdtar (macOS):  -rw-r--r--  0 user  staff     1234 Aug 27 2025 some/path.txt
/// GNU tar (Linux): -rw-r--r-- user/staff          1234 2025-08-27 14:27 some/path.txt
/// ```
///
/// bsdtar has separate `links`/`owner`/`group` fields and a
/// `month day time-or-year` date; GNU tar combines owner/group into one
/// `user/group` field (always containing a `/`, which `links` - always
/// a plain integer - never does, so that's what distinguishes the two)
/// and always uses a `YYYY-MM-DD HH:MM` date, never falling back to a
/// bare year the way bsdtar's `ls`-style date does. Either way, the name
/// is whatever's left after those fixed fields, and may itself contain
/// spaces.
fn parse_verbose_tar_line(line: &str) -> Option<TarballEntry> {
    let mut fields = line.split_whitespace();

    // Only a regular file ('-') ever contributes bytes to `tar -xOf`'s
    // output - directories, symlinks, and other special types always
    // produce zero, regardless of what their own size field says (e.g. a
    // symlink's "size" is the length of its target string, not content
    // `tar -xOf` will ever emit).
    let is_regular_file = fields.next()?.starts_with('-');

    let second_field = fields.next()?;
    let is_gnu_format = second_field.contains('/');

    let size: u64 = if is_gnu_format {
        // second_field was already the combined owner/group
        fields.next()?.parse().ok()?
    } else {
        // second_field was `links`; `owner` and `group` are still ahead
        let _owner = fields.next()?;
        let _group = fields.next()?;
        fields.next()?.parse().ok()?
    };

    // GNU: one `YYYY-MM-DD HH:MM` date field pair. bsdtar: `month day
    // time-or-year`, three fields.
    let date_time_field_count = if is_gnu_format { 2 } else { 3 };
    for _ in 0..date_time_field_count {
        fields.next()?;
    }

    let name: String = fields.collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return None;
    }

    Some(TarballEntry {
        name,
        size: if is_regular_file { size } else { 0 },
    })
}

/// Treats a failed exit status as an error - easy to silently ignore
/// otherwise (as it was, everywhere, before this), which would mean a
/// corrupted archive or a decompression failure gets reported as if
/// nothing had gone wrong, just with less (or no) output found than
/// there should have been.
///
/// `tolerable_warning_code`, if given, is a code the program documents
/// as "a warning occurred, but the output is still correct" rather than
/// a real failure - e.g. gzip and xz both exit 2 for harmless trailing
/// garbage after the compressed stream. Not every program has one of
/// these, and the code that means "warning" for one program can mean
/// something else entirely for another (bzip2's own exit code 2 means
/// a corrupt file - a real failure), so this is opt-in per call site
/// rather than a single blanket rule.
fn check_exit_status(
    program: &str,
    status: std::process::ExitStatus,
    tolerable_warning_code: Option<i32>,
) -> Result<(), Error> {
    // `tolerable_warning_code.is_some_and(...)` rather than a plain
    // `status.code() == tolerable_warning_code`: a signal-terminated
    // process has no code at all (`status.code()` is `None`), and that
    // must never match a `None` "no warning code applies here" either.
    if status.success() || tolerable_warning_code.is_some_and(|code| status.code() == Some(code))
    {
        return Ok(());
    }
    Err(Error::Io(io::Error::other(format!(
        "`{program}` {status}"
    ))))
}

/// `check_exit_status` for unzip specifically, which documents exit code
/// 1 as "one or more warning errors were found, but processing completed
/// successfully" - a perfectly fine zip occasionally triggers it for
/// cosmetic reasons (e.g. a Windows-made archive using backslash path
/// separators) without anything actually having gone wrong.
fn check_unzip_exit_status(status: std::process::ExitStatus) -> Result<(), Error> {
    if status.success() || status.code() == Some(1) {
        return Ok(());
    }
    Err(Error::Io(io::Error::other(format!("`unzip` {status}"))))
}

fn list_tarball_entries(path: &Path) -> Result<Vec<TarballEntry>, Error> {
    let listing = Command::new("tar").arg("-tvf").arg(path).output()?;
    check_exit_status("tar", listing.status, None)?;

    // A line this parser can't make sense of isn't safe to just skip:
    // `tar -xOf`'s single shared stdout stream still contains that
    // member's actual bytes regardless of whether its listing line
    // parsed, so silently dropping it would desync every subsequent
    // member's read offset - corrupting the rest of the archive's
    // output instead of just that one entry's.
    String::from_utf8_lossy(&listing.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            parse_verbose_tar_line(line).ok_or_else(|| {
                Error::Io(io::Error::other(format!(
                    "couldn't parse tar listing line: {line:?}"
                )))
            })
        })
        .collect()
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
    // Computed once rather than on every entry - the archive's own
    // path never changes across the loop, only each entry's name does.
    let archive_path = path.to_string_lossy();

    for entry in entries {
        if entry.size == 0 {
            continue;
        }

        let archive_prefixed_file_path = format!("{archive_path}:{}", entry.name);

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

    // Close our end of the pipe before waiting on the child, regardless
    // of whether the loop above actually consumed everything it wrote -
    // if it didn't (e.g. the drain above hit a real error and broke
    // early), `tar` could otherwise block forever trying to write more
    // into a pipe nobody's draining, and this wait() would then block
    // forever right along with it.
    drop(child_output);

    let status = cmd.wait()?;
    if let Err(err) = check_exit_status("tar", status, None) {
        first_error.get_or_insert(err);
    }

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

    let status = cmd.wait()?;

    res.and_then(|outcome| {
        check_exit_status(program, status, format.tolerable_warning_exit_code())?;
        Ok(outcome)
    })
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
    check_unzip_exit_status(listing.status)?;
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

    // Computed once rather than on every member boundary - the
    // archive's own path never changes, only each member's name does.
    let archive_path = path.to_string_lossy();
    let mut current_file_path = String::new();
    let mut archive_prefixed_file_path = String::new();
    // Incremented *before* use on every content line (see below), so
    // this starts at 0 rather than 1 - otherwise the first line of
    // every member would be reported as line 2, not line 1.
    let mut current_file_line_no = 0;
    // Whether the member currently being read looks binary - set the
    // first time one of its lines carries a null byte, and cleared
    // at the next member boundary. Scoped to the member rather than
    // the whole archive, so one binary member doesn't silence
    // matches already found in earlier text members, and checked
    // without ever stopping the read, so the child's stdout always
    // keeps draining instead of deadlocking on a full pipe.
    let mut current_member_is_binary = false;
    // Once a size-triggered `drain` has actually sent something for the
    // *current* member, there's no taking it back - binary detection
    // later in that same member can no longer discard everything for
    // it, only what's accumulated since. Reset alongside
    // `current_member_is_binary` at each member boundary.
    let mut current_member_already_flushed = false;
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
            // output it buffered before that was detected - unless some
            // of it was already sent on early, in which case finish
            // with what's left instead of discarding on top of what
            // can't be undone anyway.
            finish_output(
                printer,
                emit,
                current_member_is_binary,
                current_member_already_flushed,
            );
            // beginning of file output
            current_file_path = member_names.pop_front().unwrap_or_default();
            current_file_line_no = 0;
            current_member_is_binary = false;
            current_member_already_flushed = false;
            archive_prefixed_file_path = format!("{archive_path}:{current_file_path}");
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

            if matched
                && print_and_maybe_flush(
                    printer,
                    emit,
                    &archive_prefixed_file_path,
                    current_file_line_no,
                    line,
                    &line_matches,
                )
            {
                current_member_already_flushed = true;
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

    let status = cmd.wait()?;

    let outcome = match res {
        Ok(ReadResult::FileRead) => {
            // The last member in the archive never hit another
            // "extracting:"/"inflating:" marker to flush it, so do that
            // here instead.
            finish_output(
                printer,
                emit,
                current_member_is_binary,
                current_member_already_flushed,
            );
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
    };

    outcome.and_then(|outcome| {
        check_unzip_exit_status(status)?;
        Ok(outcome)
    })
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
    fn bsdtar_parses_a_regular_file_line() {
        let entry =
            parse_verbose_tar_line("-rw-r--r--  0 voga   staff  786671 Aug 27  2025 node_modules/.package-lock.json")
                .unwrap();
        assert_eq!(entry.name, "node_modules/.package-lock.json");
        assert_eq!(entry.size, 786671);
    }

    #[test]
    fn bsdtar_parses_a_file_name_with_spaces() {
        let entry =
            parse_verbose_tar_line("-rw-r--r--  0 voga   staff     123 Aug 27  2025 some dir/a file.txt")
                .unwrap();
        assert_eq!(entry.name, "some dir/a file.txt");
        assert_eq!(entry.size, 123);
    }

    #[test]
    fn bsdtar_directory_always_has_zero_size_regardless_of_its_listed_size() {
        let entry =
            parse_verbose_tar_line("drwxr-xr-x  0 voga   staff       0 Aug 27  2025 node_modules/")
                .unwrap();
        assert_eq!(entry.name, "node_modules/");
        assert_eq!(entry.size, 0);
    }

    #[test]
    fn bsdtar_symlink_always_has_zero_size_even_though_its_listed_size_is_its_target_length() {
        let entry = parse_verbose_tar_line(
            "lrwxr-xr-x  0 voga   staff      20 Aug 26  2025 node_modules/some-link -> ../apps/some-app",
        )
        .unwrap();
        assert_eq!(entry.size, 0);
    }

    #[test]
    fn gnu_tar_parses_a_regular_file_line() {
        // GNU tar combines owner/group into one field and uses an ISO
        // date instead of bsdtar's `month day time-or-year`.
        let entry = parse_verbose_tar_line(
            "-rw-r--r-- voga/staff      786671 2025-08-27 14:27 node_modules/.package-lock.json",
        )
        .unwrap();
        assert_eq!(entry.name, "node_modules/.package-lock.json");
        assert_eq!(entry.size, 786671);
    }

    #[test]
    fn gnu_tar_parses_a_file_name_with_spaces() {
        let entry = parse_verbose_tar_line(
            "-rw-r--r-- voga/staff         123 2025-08-27 14:27 some dir/a file.txt",
        )
        .unwrap();
        assert_eq!(entry.name, "some dir/a file.txt");
        assert_eq!(entry.size, 123);
    }

    #[test]
    fn gnu_tar_directory_always_has_zero_size_regardless_of_its_listed_size() {
        let entry = parse_verbose_tar_line(
            "drwxr-xr-x voga/staff           0 2025-08-27 14:27 node_modules/",
        )
        .unwrap();
        assert_eq!(entry.name, "node_modules/");
        assert_eq!(entry.size, 0);
    }

    #[test]
    fn gnu_tar_symlink_always_has_zero_size_even_though_its_listed_size_is_its_target_length() {
        let entry = parse_verbose_tar_line(
            "lrwxr-xr-x voga/staff          20 2025-08-26 14:27 node_modules/some-link -> ../apps/some-app",
        )
        .unwrap();
        assert_eq!(entry.size, 0);
    }

    #[test]
    fn gnu_tar_date_never_falls_back_to_a_bare_year_the_way_bsdtar_does() {
        // Confirmed empirically against real GNU tar: even a file from
        // years ago still gets a full `YYYY-MM-DD HH:MM`, unlike
        // bsdtar/`ls`, which swaps the time for a bare year once a file
        // is old enough.
        let entry = parse_verbose_tar_line(
            "-rw-r--r-- voga/wheel           6 2020-01-01 00:00 subdir/plain.txt",
        )
        .unwrap();
        assert_eq!(entry.name, "subdir/plain.txt");
        assert_eq!(entry.size, 6);
    }

    #[test]
    fn an_unparseable_line_is_skipped() {
        assert!(parse_verbose_tar_line("").is_none());
        assert!(parse_verbose_tar_line("not a tar listing line").is_none());
    }
}
