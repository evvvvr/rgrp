use std::env;
use std::io::IsTerminal;
use std::process::ExitCode;

use rgrp::printer::{ColorPrinter, MonochromePrinter};
use rgrp::run::run;

const USAGE_HINT: &str = "usage: rgrp [OPTIONS] PATTERN PATH";

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let pattern = args.next();
    let path = args.next();

    let (Some(pattern), Some(path)) = (pattern, path) else {
        eprintln!("{USAGE_HINT}");
        return ExitCode::FAILURE;
    };

    // collect rest of the paths
    let rest_args: Vec<_> = args.collect();

    let (Some(pattern), Some(path)) = (pattern.to_str(), path.to_str()) else {
        eprintln!("arguments must be valid UTF-8");
        return ExitCode::FAILURE;
    };

    let Some(rest_paths): Option<Vec<&str>> = rest_args.iter().map(|arg| arg.to_str()).collect()
    else {
        eprintln!("arguments must be valid UTF-8");
        return ExitCode::FAILURE;
    };

    let paths = std::iter::once(path).chain(rest_paths);

    let no_terminal = !std::io::stdout().is_terminal() || env::var("NO_COLOR").is_ok();

    let error = if no_terminal {
        run(pattern, paths, || MonochromePrinter {})
    } else {
        run(pattern, paths, ColorPrinter::new)
    };

    if let Some(e) = error {
        eprintln!("{e}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
