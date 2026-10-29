use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};

use regex::bytes::Regex;

use crate::printer::FlushingPrinter;

use super::super::{Error, Options, Outcome, SearchResult, search_reader};
use super::{ArchiveEntry, check_exit_status};

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

/// `check_exit_status` for unzip, which documents exit code 1 as a
/// harmless warning (e.g. a Windows-made archive using backslash path
/// separators) rather than a real failure.
fn check_unzip_exit_status(status: std::process::ExitStatus) -> Result<(), Error> {
    check_exit_status("unzip", status, Some(1))
}

fn list_zip_entries(path: &Path) -> Result<Vec<ArchiveEntry>, Error> {
    let listing = Command::new("unzip").arg("-v").arg(path).output()?;
    check_unzip_exit_status(listing.status)?;

    // Same reasoning as `list_tarball_entries` (in `tar.rs`), but
    // `unzip -v` also wraps its entries in an `Archive:` line, column
    // header, separators, and a totals line - filtered out here since
    // they're recognizable on sight, before the "must parse" check
    // below.
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

/// Searches every member of a zip in one pass, the same way
/// `search_tarball` (in `tar.rs`) does: one `unzip -p` process extracts
/// the whole archive, with every member's content concatenated onto
/// its stdout in archive order, and this reads each member's exact
/// byte count (from `list_zip_entries`) off that stream before moving
/// to the next.
pub(in crate::search) fn search_zip(
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

        // Same reasoning as `search_tarball`'s drain check (in `tar.rs`).
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

    // Same reasoning as `search_tarball` (in `tar.rs`).
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
