mod common;

use std::cell::RefCell;
use std::fs;

use common::SharedRecorder;
use rgrp::run::{Options, run};
use tempfile::tempdir;

#[test]
fn reports_matches_from_good_paths_and_errors_for_bad_ones() {
    let dir = tempdir().unwrap();
    let good = dir.path().join("good.txt");
    fs::write(&good, "a needle here\n").unwrap();
    let missing = dir.path().join("does-not-exist");

    let recorder = SharedRecorder::new();
    let errors = RefCell::new(Vec::new());

    let options = Options {
        regexps: vec!["needle".to_string()],
        files: vec![
            good.to_str().unwrap().to_string(),
            missing.to_str().unwrap().to_string(),
        ],
        ignore_case: false,
        invert_matches: false,
    };

    let result = run(
        &options,
        || recorder.printer(),
        |_err, path| errors.borrow_mut().push(path.to_string()),
    );

    assert!(result.is_err());
    assert_eq!(recorder.matches().len(), 1);
    assert_eq!(errors.borrow().as_slice(), [missing.to_str().unwrap()]);
}

#[test]
fn succeeds_when_every_path_is_valid() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("only.txt");
    fs::write(&file, "a needle here\n").unwrap();

    let recorder = SharedRecorder::new();
    let errors = RefCell::new(Vec::new());

    let options = Options {
        regexps: vec!["needle".to_string()],
        files: vec![file.to_str().unwrap().to_string()],
        ignore_case: false,
        invert_matches: false,
    };

    let result = run(
        &options,
        || recorder.printer(),
        |_err, path| errors.borrow_mut().push(path.to_string()),
    );

    assert!(result.is_ok());
    assert!(errors.borrow().is_empty());
}

#[test]
fn ignore_case_matches_regardless_of_letter_case() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("mixed_case.txt");
    fs::write(&file, "NEEDLE\nhaystack\nNeEdLe\n").unwrap();

    let recorder = SharedRecorder::new();

    let options = Options {
        regexps: vec!["needle".to_string()],
        files: vec![file.to_str().unwrap().to_string()],
        ignore_case: true,
        invert_matches: false,
    };

    let result = run(&options, || recorder.printer(), |_err, _path| {});

    assert!(result.is_ok());
    let lines: Vec<String> = recorder.matches().into_iter().map(|m| m.line).collect();
    assert_eq!(lines, vec!["NEEDLE", "NeEdLe"]);
}

#[test]
fn without_ignore_case_only_exact_case_matches() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("mixed_case.txt");
    fs::write(&file, "NEEDLE\nneedle\n").unwrap();

    let recorder = SharedRecorder::new();

    let options = Options {
        regexps: vec!["needle".to_string()],
        files: vec![file.to_str().unwrap().to_string()],
        ignore_case: false,
        invert_matches: false,
    };

    let result = run(&options, || recorder.printer(), |_err, _path| {});

    assert!(result.is_ok());
    let lines: Vec<String> = recorder.matches().into_iter().map(|m| m.line).collect();
    assert_eq!(lines, vec!["needle"]);
}

#[test]
fn invert_match_reports_lines_that_do_not_match() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("mixed.txt");
    fs::write(&file, "needle\nhaystack\nneedle in a haystack\nplain\n").unwrap();

    let recorder = SharedRecorder::new();

    let options = Options {
        regexps: vec!["needle".to_string()],
        files: vec![file.to_str().unwrap().to_string()],
        ignore_case: false,
        invert_matches: true,
    };

    let result = run(&options, || recorder.printer(), |_err, _path| {});

    assert!(result.is_ok());
    let lines: Vec<String> = recorder.matches().into_iter().map(|m| m.line).collect();
    assert_eq!(lines, vec!["haystack", "plain"]);
}
