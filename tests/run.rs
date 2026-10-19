mod common;

use std::cell::RefCell;
use std::fs;

use common::SharedRecorder;
use rgrp::run::run;
use tempfile::tempdir;

#[test]
fn reports_matches_from_good_paths_and_errors_for_bad_ones() {
    let dir = tempdir().unwrap();
    let good = dir.path().join("good.txt");
    fs::write(&good, "a needle here\n").unwrap();
    let missing = dir.path().join("does-not-exist");

    let recorder = SharedRecorder::new();
    let errors = RefCell::new(Vec::new());

    let result = run(
        "needle",
        [good.to_str().unwrap(), missing.to_str().unwrap()].into_iter(),
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

    let result = run(
        "needle",
        [file.to_str().unwrap()].into_iter(),
        || recorder.printer(),
        |_err, path| errors.borrow_mut().push(path.to_string()),
    );

    assert!(result.is_ok());
    assert!(errors.borrow().is_empty());
}
