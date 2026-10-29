use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};

use regex::bytes::Regex;

use crate::printer::FlushingPrinter;

use super::super::{Error, Options, Outcome, SearchResult, search_reader};
use super::{ArchiveEntry, check_exit_status};

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

/// Searches every member of a tarball in one pass: one `tar` process
/// extracts the whole archive, with every member's content concatenated
/// onto its stdout in archive order, and this reads each member's exact
/// byte count (from `list_tarball_entries`) off that stream before
/// moving to the next. Extracting each member with its own `tar` call
/// instead would re-decompress the archive from the start every time.
pub(in crate::search) fn search_tarball(
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
