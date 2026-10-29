use std::fs;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use regex::bytes::Regex;

use crate::printer::FlushingPrinter;

use super::classify::CompressionFormat;
use super::{Error, Options, SearchResult, search_reader};

mod tar;
mod zip;

pub(super) use tar::search_tarball;
pub(super) use zip::search_zip;

/// One member of an archive (tarball or zip).
struct ArchiveEntry {
    name: String,
    /// Content bytes this member contributes to the extractor's stdout.
    /// `0` for directories, symlinks, etc.
    size: u64,
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

pub(super) fn search_plain(
    patterns: &[Regex],
    path: &Path,
    options: &Options,
    printer: &mut FlushingPrinter<'_>,
) -> SearchResult {
    let file = fs::File::open(path)?;
    search_reader(patterns, &path.to_string_lossy(), options, file, printer)
}
