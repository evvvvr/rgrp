use clap::Parser;

use std::env;
use std::io::IsTerminal;
use std::process::ExitCode;

use rgrp::printer::{ColorPrinter, MonochromePrinter};
use rgrp::run::run;
use rgrp::search;

const PROGRAM_NAME: &str = "rgrp";

#[derive(Parser)]
#[command(name=PROGRAM_NAME)]
struct Cli {
    /// Regular expression pattern to search for
    pattern: String,

    /// File path to search in
    path: Vec<String>,
}

fn main() -> ExitCode {
    // CLI args:
    // -e PATTERN, --regexp=PATTERN multiple
    // if used all positional args are considerex to be pathes
    // -i, --ignore-case
    // -v, --invert-match

    let cli = Cli::parse();
    let pattern = &cli.pattern;
    let path = cli.path;

    let paths = path.iter().map(String::as_str);

    let no_terminal = !std::io::stdout().is_terminal() || env::var("NO_COLOR").is_ok();

    let on_error = |e: search::Error, path: &str| match e {
        search::Error::Io(err) => {
            eprintln!("{PROGRAM_NAME}:{path} IO error for operation on path {path}: {err}")
        }
        search::Error::Pattern(err) => eprintln!("{PROGRAM_NAME}: pattern error: {err}"),
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
