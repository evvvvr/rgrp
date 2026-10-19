mod common;

use std::fs;

use common::SharedRecorder;
use rgrp::search::{self, search};
use tempfile::tempdir;

#[test]
fn finds_matches_in_every_file_of_a_flat_directory() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "hello world\n").unwrap();
    fs::write(dir.path().join("b.txt"), "say hello\n").unwrap();
    fs::write(dir.path().join("c.txt"), "no match here\n").unwrap();

    let recorder = SharedRecorder::new();
    search("hello", dir.path().to_str().unwrap(), &mut || {
        recorder.printer()
    })
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
    search("needle", dir.path().to_str().unwrap(), &mut || {
        recorder.printer()
    })
    .unwrap();

    assert_eq!(recorder.matches().len(), 2);
}

#[test]
fn empty_directory_produces_no_matches_and_no_error() {
    let dir = tempdir().unwrap();

    let recorder = SharedRecorder::new();
    search("anything", dir.path().to_str().unwrap(), &mut || {
        recorder.printer()
    })
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
    search("needle", file.to_str().unwrap(), &mut || recorder.printer()).unwrap();

    assert_eq!(recorder.matches().len(), 1);
}

#[test]
fn nonexistent_path_returns_io_error() {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("does-not-exist");

    let recorder = SharedRecorder::new();
    let result = search("x", missing.to_str().unwrap(), &mut || recorder.printer());

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
    search("needle", dir.path().to_str().unwrap(), &mut || {
        recorder.printer()
    })
    .unwrap();

    // Only the real file is visited; the symlink doesn't duplicate the match.
    assert_eq!(recorder.matches().len(), 1);
}

// macOS (APFS/HFS+) rejects non-UTF8 filenames outright, so this case can
// only be exercised on filesystems that actually allow arbitrary bytes.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_path_is_skipped_without_aborting_the_walk() {
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    let dir = tempdir().unwrap();
    fs::write(dir.path().join("good.txt"), "a needle here\n").unwrap();

    let bad_name = std::ffi::OsStr::from_bytes(&[0x66, 0x6f, 0x80, 0x6f]); // "fo\x80o"
    fs::write(Path::new(dir.path()).join(bad_name), "needle too\n").unwrap();

    let recorder = SharedRecorder::new();
    search("needle", dir.path().to_str().unwrap(), &mut || {
        recorder.printer()
    })
    .unwrap();

    // The non-UTF8 entry is skipped; the walk still finds the valid file.
    assert_eq!(recorder.matches().len(), 1);
}
