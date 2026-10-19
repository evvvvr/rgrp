use std::env;
use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;

use rgrp::printer::{ColorPrinter, MonochromePrinter};
use rgrp::run::run;
use rgrp::search;

const USAGE_HINT: &str = "usage: rgrp [OPTIONS] PATTERN PATH";

fn main() -> ExitCode {
    let mut args = env::args_os();
    let program_name = args
        .next()
        .and_then(|a| {
            Path::new(&a)
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "rgrp".to_owned());
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

    let on_error = |e: search::Error, path: &str| match e {
        search::Error::Io(err) => {
            eprintln!("{program_name}:{path} IO error for operation on path {path}: {err}")
        }
        search::Error::Pattern(err) => eprintln!("{program_name}: pattern error: {err}"),
    };

    let result = if no_terminal {
        run(pattern, paths, || MonochromePrinter {}, on_error)
    } else {
        run(pattern, paths, ColorPrinter::new, on_error)
    };

    if result.is_err() {
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
