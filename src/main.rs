use std::env;
use std::process::ExitCode;

use rgrp::search::search;

const USAGE_HINT: &str = "usage: rgrp [OPTIONS] PATTERN PATH";

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let pattern = args.next();
    let path = args.next();

    let (Some(pattern), Some(path)) = (pattern, path) else {
        eprintln!("{USAGE_HINT}");
        return ExitCode::FAILURE;
    };

    let (Some(pattern), Some(path)) = (pattern.to_str(), path.to_str()) else {
        eprintln!("arguments must be valid UTF-8");
        return ExitCode::FAILURE;
    };

    if let Err(e) = search(pattern, path) {
        eprintln!("{e}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
