use regex::bytes::Regex;
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::printer::FlushingPrinter;

use super::classify::CompressionFormat;
use super::{Error, Options, Outcome, SearchResult, search_reader};

/// One member of an archive (tarball or zip).
struct ArchiveEntry {
    name: String,
    /// Content bytes this member contributes to the extractor's stdout.
    /// `0` for directories, symlinks, etc.
    size: u64,
}

/// Parses one line of `tar -tvf`'s verbose listing. Column layout
/// differs between tar implementations, e.g.:
///
/// ```text
/// bsdtar (macOS):  -rw-r--r--  0 user  staff     1234 Aug 27 2025 some/path.txt
/// GNU tar (Linux): -rw-r--r-- user/staff          1234 2025-08-27 14:27 some/path.txt
/// ```
///
/// GNU combines owner/group into one `user/group` field (always has a
/// `/`, telling it apart from bsdtar's plain-integer `links` field) and
/// always uses a `YYYY-MM-DD HH:MM` date instead of bsdtar's `month day
/// time-or-year`. The name is whatever's left after the fixed fields,
/// and may itself contain spaces.
fn parse_verbose_tar_line(line: &str) -> Option<ArchiveEntry> {
    let mut fields = line.split_whitespace();

    // Only a regular file contributes bytes to `tar -xOf`'s output -
    // directories, symlinks etc. always produce zero regardless of
    // their own size field (a symlink's "size" is its target length).
    let is_regular_file = fields.next()?.starts_with('-');

    let second_field = fields.next()?;
    let is_gnu_format = second_field.contains('/');

    let size: u64 = if is_gnu_format {
        fields.next()?.parse().ok()?
    } else {
        let _owner = fields.next()?;
        let _group = fields.next()?;
        fields.next()?.parse().ok()?
    };

    let date_time_field_count = if is_gnu_format { 2 } else { 3 };
    for _ in 0..date_time_field_count {
        fields.next()?;
    }

    let name: String = fields.collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return None;
    }

    Some(ArchiveEntry {
        name,
        size: if is_regular_file { size } else { 0 },
    })
}

/// Parses one entry line of `unzip -v`'s verbose listing, e.g.:
///
/// ```text
///  Length   Method    Size  Cmpr    Date    Time   CRC-32   Name
/// --------  ------  ------- ---- ---------- ----- --------  ----
///       18  Stored       18   0% 10-29-2026 09:32 b8c5e2e7  sub/a.txt
/// --------          -------  ---                            -------
///       18               18   0%                            1 file
/// ```
///
/// `list_zip_entries` filters out the header, separators, and totals
/// line before calling this, so it only ever sees a real entry.
fn parse_verbose_zip_line(line: &str) -> Option<ArchiveEntry> {
    let mut fields = line.split_whitespace();

    let size: u64 = fields.next()?.parse().ok()?;
    let _method = fields.next()?;
    let _compressed_size = fields.next()?;
    let _ratio = fields.next()?;
    let _date = fields.next()?;
    let _time = fields.next()?;
    let _crc32 = fields.next()?;

    let name: String = fields.collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return None;
    }

    Some(ArchiveEntry { name, size })
}

/// Whether `line` is `unzip -v`'s trailing totals line rather than a
/// real entry - its second column is a number (a size total) there,
/// but always a method name (e.g. `Stored`) for a real entry.
fn is_zip_totals_line(line: &str) -> bool {
    let mut fields = line.split_whitespace();
    let _length = fields.next();
    matches!(fields.next(), Some(field) if field.parse::<u64>().is_ok())
}

/// Treats a failed exit status as an error, so a corrupted archive or
/// failed decompression is reported instead of silently producing less
/// output than it should.
///
/// `tolerable_warning_code`, if given, is a code the program documents
/// as a harmless warning rather than a real failure (e.g. gzip/xz exit
/// 2 for trailing garbage after the compressed stream). Opt-in per call
/// site since the same code can mean a real failure for another program
/// (bzip2's exit code 2 means a corrupt file).
fn check_exit_status(
    program: &str,
    status: std::process::ExitStatus,
    tolerable_warning_code: Option<i32>,
) -> Result<(), Error> {
    // `is_some_and` rather than `status.code() == tolerable_warning_code`:
    // a signal-terminated process's code is `None`, which must never
    // match a `None` "no warning code applies" either.
    if status.success() || tolerable_warning_code.is_some_and(|code| status.code() == Some(code))
    {
        return Ok(());
    }
    Err(Error::Io(io::Error::other(format!(
        "`{program}` {status}"
    ))))
}

/// `check_exit_status` for unzip, which documents exit code 1 as a
/// harmless warning (e.g. a Windows-made archive using backslash path
/// separators) rather than a real failure.
fn check_unzip_exit_status(status: std::process::ExitStatus) -> Result<(), Error> {
    check_exit_status("unzip", status, Some(1))
}

fn list_tarball_entries(path: &Path) -> Result<Vec<ArchiveEntry>, Error> {
    let listing = Command::new("tar").arg("-tvf").arg(path).output()?;
    check_exit_status("tar", listing.status, None)?;

    // An unparseable line can't just be skipped: `tar -xOf`'s stdout
    // still contains that member's bytes either way, so dropping its
    // entry here would desync every later member's read offset.
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

fn list_zip_entries(path: &Path) -> Result<Vec<ArchiveEntry>, Error> {
    let listing = Command::new("unzip").arg("-v").arg(path).output()?;
    check_unzip_exit_status(listing.status)?;

    // Same reasoning as `list_tarball_entries` above, but `unzip -v`
    // also wraps its entries in an `Archive:` line, column header,
    // separators, and a totals line - filtered out here since they're
    // recognizable on sight, before the "must parse" check below.
    String::from_utf8_lossy(&listing.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter(|line| !line.starts_with("Archive:"))
        .filter(|line| !line.trim_start().starts_with("Length"))
        .filter(|line| !line.trim_start().starts_with("--"))
        .filter(|line| !is_zip_totals_line(line))
        .map(|line| {
            parse_verbose_zip_line(line).ok_or_else(|| {
                Error::Io(io::Error::other(format!(
                    "couldn't parse unzip -v listing line: {line:?}"
                )))
            })
        })
        .collect()
}

/// Searches every member of a tarball in one pass: one `tar` process
/// extracts the whole archive, with every member's content concatenated
/// onto its stdout in archive order, and this reads each member's exact
/// byte count (from `list_tarball_entries`) off that stream before
/// moving to the next. Extracting each member with its own `tar` call
/// instead would re-decompress the archive from the start every time.
pub(super) fn search_tarball(
    patterns: &[Regex],
    path: &Path,
    options: &Options,
    printer: &mut FlushingPrinter<'_>,
) -> SearchResult {
    let entries = list_tarball_entries(path)?;

    let mut cmd = Command::new("tar")
        .arg("-xOf")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;

    let mut child_output = cmd.stdout.take().expect("just configured with Stdio::piped()");
    // Kept rather than discarded for a generic placeholder, so the
    // caller learns what actually went wrong.
    let mut first_error: Option<Error> = None;
    let archive_path = path.to_string_lossy();

    for entry in entries {
        if entry.size == 0 {
            continue;
        }

        let archive_prefixed_file_path = format!("{archive_path}:{}", entry.name);

        // Bounded to this member's own size so `search_reader` never
        // reads into the next member's content.
        let mut member_reader = (&mut child_output).take(entry.size);

        let res = search_reader(
            patterns,
            &archive_prefixed_file_path,
            options,
            &mut member_reader,
            printer,
        );

        // `search_reader` may have stopped early (e.g. a binary
        // member), leaving bytes unread - drain them so the next
        // member starts at the right offset.
        if let Err(drain_err) = io::copy(&mut member_reader, &mut io::sink()) {
            first_error.get_or_insert(Error::from(drain_err));
            break;
        }

        // `limit()` only reaches `0` once every byte `take` allowed
        // has actually been read; anything left means `tar`'s stdout
        // hit EOF before this member's full content arrived.
        if member_reader.limit() != 0 {
            first_error.get_or_insert(Error::Io(io::Error::other(format!(
                "`tar` wrote fewer bytes for {archive_prefixed_file_path} than its listing promised"
            ))));
            break;
        }

        if let Err(err) = res {
            first_error.get_or_insert(err);
        }
    }

    // Close our end before waiting: otherwise, if the loop broke out
    // early without draining everything, `tar` could block forever
    // writing into a pipe nobody's reading.
    drop(child_output);

    // A wait() error only matters if nothing already failed - `?`
    // here would let it bury a more informative earlier error.
    match cmd.wait() {
        Ok(status) => {
            if let Err(err) = check_exit_status("tar", status, None) {
                first_error.get_or_insert(err);
            }
        }
        Err(wait_err) => {
            first_error.get_or_insert(Error::from(wait_err));
        }
    }

    if let Some(err) = first_error {
        return Err(err);
    }

    Ok(Outcome::Done)
}

/// A single compressed file (`access.log.gz`, `notes.txt.bz2`, ...)
/// rather than a compressed tarball - `classify` sends those to
/// `search_tarball` instead.
pub(super) fn search_compressed(
    patterns: &[Regex],
    path: &Path,
    options: &Options,
    printer: &mut FlushingPrinter<'_>,
    format: CompressionFormat,
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
    );

    // `res`'s own error takes priority over a mere wait() failure,
    // same reasoning as `search_tarball` - checked only once `res` is
    // already `Ok`, inside the closure below.
    let wait_result = cmd.wait();

    res.and_then(|outcome| {
        let status = wait_result?;
        check_exit_status(program, status, format.tolerable_warning_exit_code())?;
        Ok(outcome)
    })
}

/// Searches every member of a zip in one pass, the same way
/// `search_tarball` does: one `unzip -p` process extracts the whole
/// archive, with every member's content concatenated onto its stdout
/// in archive order, and this reads each member's exact byte count
/// (from `list_zip_entries`) off that stream before moving to the next.
pub(super) fn search_zip(
    patterns: &[Regex],
    path: &Path,
    options: &Options,
    printer: &mut FlushingPrinter<'_>,
) -> SearchResult {
    let entries = list_zip_entries(path)?;

    let mut cmd = Command::new("unzip")
        .arg("-p")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;

    let mut child_output = cmd.stdout.take().expect("just configured with Stdio::piped()");
    let mut first_error: Option<Error> = None;
    let archive_path = path.to_string_lossy();

    for entry in entries {
        if entry.size == 0 {
            continue;
        }

        let archive_prefixed_file_path = format!("{archive_path}:{}", entry.name);
        let mut member_reader = (&mut child_output).take(entry.size);

        let res = search_reader(
            patterns,
            &archive_prefixed_file_path,
            options,
            &mut member_reader,
            printer,
        );

        if let Err(drain_err) = io::copy(&mut member_reader, &mut io::sink()) {
            first_error.get_or_insert(Error::from(drain_err));
            break;
        }

        // Same reasoning as `search_tarball`'s drain check above.
        if member_reader.limit() != 0 {
            first_error.get_or_insert(Error::Io(io::Error::other(format!(
                "`unzip` wrote fewer bytes for {archive_prefixed_file_path} than its listing promised"
            ))));
            break;
        }

        if let Err(err) = res {
            first_error.get_or_insert(err);
        }
    }

    // Same reasoning as `search_tarball` above.
    drop(child_output);

    match cmd.wait() {
        Ok(status) => {
            if let Err(err) = check_unzip_exit_status(status) {
                first_error.get_or_insert(err);
            }
        }
        Err(wait_err) => {
            first_error.get_or_insert(Error::from(wait_err));
        }
    }

    if let Some(err) = first_error {
        return Err(err);
    }

    Ok(Outcome::Done)
}

pub(super) fn search_plain(
    patterns: &[Regex],
    path: &Path,
    options: &Options,
    printer: &mut FlushingPrinter<'_>,
) -> SearchResult {
    let file = fs::File::open(path)?;
    search_reader(patterns, &path.to_string_lossy(), options, file, printer)
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
