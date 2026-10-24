mod common;

use std::fs;
use std::path::Path;

use common::SharedRecorder;
use regex::bytes::Regex;
use rgrp::concurrency::SharedWorkState;
use rgrp::search::{self, search};
use tempfile::tempdir;

// `search` only expands one level of a directory into the shared queue and
// returns; draining the queue (single-threaded, so there's no race to
// worry about) is what actually walks the whole tree.
fn walk(
    patterns: &[Regex],
    root: &Path,
    options: &search::Options,
    mut make_printer: impl FnMut() -> common::RecordingPrinter,
) -> Result<(), search::Error> {
    let shared_state = SharedWorkState::new(&[root.to_path_buf()]);
    while let Some(path) = shared_state.work_queue.pop() {
        let mut printer = make_printer();
        search(patterns, &path, options, &mut printer, &shared_state)?;
    }
    Ok(())
}

#[test]
fn finds_matches_in_every_file_of_a_flat_directory() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "hello world\n").unwrap();
    fs::write(dir.path().join("b.txt"), "say hello\n").unwrap();
    fs::write(dir.path().join("c.txt"), "no match here\n").unwrap();

    let recorder = SharedRecorder::new();
    let re = Regex::new("hello").unwrap();
    walk(
        &[re],
        dir.path(),
        &search::Options {
            invert_matches: false,
            search_binary: false,
        },
        &mut || recorder.printer(),
    )
    .unwrap();

    let mut lines: Vec<String> = recorder.matches().into_iter().map(|m| m.line).collect();
    lines.sort();
    assert_eq!(lines, vec!["hello world", "say hello"]);
}

#[test]
fn recurses_into_nested_subdirectories() {
    let dir = tempdir().unwrap();
    let nested = dir.path().join("a").join("b");
    fs::create_dir_all(&nested).unwrap();
    fs::write(dir.path().join("top.txt"), "needle at top\n").unwrap();
    fs::write(nested.join("deep.txt"), "needle deep inside\n").unwrap();

    let recorder = SharedRecorder::new();
    let re = Regex::new("needle").unwrap();
    walk(
        &[re],
        dir.path(),
        &search::Options {
            invert_matches: false,
            search_binary: false,
        },
        &mut || recorder.printer(),
    )
    .unwrap();

    assert_eq!(recorder.matches().len(), 2);
}

#[test]
fn empty_directory_produces_no_matches_and_no_error() {
    let dir = tempdir().unwrap();

    let recorder = SharedRecorder::new();
    let re = Regex::new("anything").unwrap();
    walk(
        &[re],
        dir.path(),
        &search::Options {
            invert_matches: false,
            search_binary: false,
        },
        &mut || recorder.printer(),
    )
    .unwrap();

    assert_eq!(recorder.matches().len(), 0);
    assert_eq!(recorder.eof_count(), 0);
}

#[test]
fn single_file_path_is_searched_directly() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("only.txt");
    fs::write(&file, "a needle in a file\n").unwrap();

    let recorder = SharedRecorder::new();
    let re = Regex::new("needle").unwrap();
    walk(
        &[re],
        &file,
        &search::Options {
            invert_matches: false,
            search_binary: false,
        },
        &mut || recorder.printer(),
    )
    .unwrap();

    assert_eq!(recorder.matches().len(), 1);
}

#[test]
fn nonexistent_path_returns_io_error() {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("does-not-exist");

    let recorder = SharedRecorder::new();
    let re = Regex::new("x").unwrap();
    let result = walk(
        &[re],
        &missing,
        &search::Options {
            invert_matches: false,
            search_binary: false,
        },
        &mut || recorder.printer(),
    );

    assert!(matches!(result, Err(search::Error::Io(_))));
}

#[cfg(unix)]
#[test]
fn symlinks_are_skipped_rather_than_followed() {
    use std::os::unix::fs::symlink;

    let dir = tempdir().unwrap();
    fs::write(dir.path().join("real.txt"), "a needle here\n").unwrap();
    symlink(dir.path().join("real.txt"), dir.path().join("link.txt")).unwrap();

    let recorder = SharedRecorder::new();
    let re = Regex::new("needle").unwrap();
    walk(
        &[re],
        dir.path(),
        &search::Options {
            invert_matches: false,
            search_binary: false,
        },
        &mut || recorder.printer(),
    )
    .unwrap();

    // Only the real file is visited; the symlink doesn't duplicate the match.
    assert_eq!(recorder.matches().len(), 1);
}

// macOS (APFS/HFS+) rejects non-UTF8 filenames outright, so this case can
// only be exercised on filesystems that actually allow arbitrary bytes.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_path_is_searched_like_any_other_file() {
    use std::os::unix::ffi::OsStrExt;

    let dir = tempdir().unwrap();
    fs::write(dir.path().join("good.txt"), "a needle here\n").unwrap();

    let bad_name = std::ffi::OsStr::from_bytes(&[0x66, 0x6f, 0x80, 0x6f]); // "fo\x80o"
    fs::write(dir.path().join(bad_name), "needle too\n").unwrap();

    let recorder = SharedRecorder::new();
    let re = Regex::new("needle").unwrap();
    walk(
        &[re],
        dir.path(),
        &search::Options {
            invert_matches: false,
            search_binary: false,
        },
        &mut || recorder.printer(),
    )
    .unwrap();

    // `Path`/`PathBuf` carry a non-UTF8 filename just fine, so both files
    // are found and searched - only display formatting is lossy, not the
    // walk itself.
    assert_eq!(recorder.matches().len(), 2);
}
