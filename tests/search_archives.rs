mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use common::{SharedRecorder, walk};
use regex::bytes::Regex;
use rgrp::glob;
use rgrp::search::MatchConfig;
use tempfile::tempdir;

fn make_tarball(src_dir: &Path, archive_path: &Path) {
    let status = Command::new("tar")
        .arg("-czf")
        .arg(archive_path)
        .arg("-C")
        .arg(src_dir)
        .arg(".")
        .status()
        .unwrap();
    assert!(status.success());
}

fn make_zip(src_dir: &Path, archive_path: &Path) {
    let status = Command::new("zip")
        .arg("-qr")
        .arg(archive_path)
        .arg(".")
        .current_dir(src_dir)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn finds_matches_inside_a_tarball() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("a.txt"), "needle one\n").unwrap();

    let archive = dir.path().join("archive.tar.gz");
    make_tarball(&src, &archive);

    let recorder = SharedRecorder::new();
    let re = Regex::new("needle").unwrap();
    walk(
        &MatchConfig {
            patterns: &[re],
            search_archives: true,
            ..Default::default()
        },
        &archive,
        &mut || recorder.printer(),
    )
    .unwrap();

    assert_eq!(recorder.matches().len(), 1);
}

#[test]
fn finds_matches_inside_a_zip() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("a.txt"), "needle one\n").unwrap();

    let archive = dir.path().join("archive.zip");
    make_zip(&src, &archive);

    let recorder = SharedRecorder::new();
    let re = Regex::new("needle").unwrap();
    walk(
        &MatchConfig {
            patterns: &[re],
            search_archives: true,
            ..Default::default()
        },
        &archive,
        &mut || recorder.printer(),
    )
    .unwrap();

    assert_eq!(recorder.matches().len(), 1);
}

#[test]
fn a_glob_filters_which_tarball_members_get_searched() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("main.rs"), "needle one\n").unwrap();
    fs::write(src.join("notes.txt"), "needle two\n").unwrap();

    let archive = dir.path().join("archive.tar.gz");
    make_tarball(&src, &archive);

    let recorder = SharedRecorder::new();
    let re = Regex::new("needle").unwrap();
    let globs = [glob::compile("*.rs", false).unwrap()];
    walk(
        &MatchConfig {
            patterns: &[re],
            globs: &globs,
            search_archives: true,
            ..Default::default()
        },
        &archive,
        &mut || recorder.printer(),
    )
    .unwrap();

    let matches = recorder.matches();
    assert_eq!(matches.len(), 1);
    assert!(matches[0].file_path.ends_with("main.rs"));
}

#[test]
fn a_glob_filters_which_zip_members_get_searched() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("main.rs"), "needle one\n").unwrap();
    fs::write(src.join("notes.txt"), "needle two\n").unwrap();

    let archive = dir.path().join("archive.zip");
    make_zip(&src, &archive);

    let recorder = SharedRecorder::new();
    let re = Regex::new("needle").unwrap();
    let globs = [glob::compile("*.rs", false).unwrap()];
    walk(
        &MatchConfig {
            patterns: &[re],
            globs: &globs,
            search_archives: true,
            ..Default::default()
        },
        &archive,
        &mut || recorder.printer(),
    )
    .unwrap();

    let matches = recorder.matches();
    assert_eq!(matches.len(), 1);
    assert!(matches[0].file_path.ends_with("main.rs"));
}
