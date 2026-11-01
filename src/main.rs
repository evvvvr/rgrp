use std::env;
use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;

use rgrp::printer::{ColorPrinter, MonochromePrinter};
use rgrp::run::{self, run};
use rgrp::search;

mod cli;
use cli::{PROGRAM_NAME, parse_options};

fn main() -> ExitCode {
    let options = parse_options();

    let no_terminal = !std::io::stdout().is_terminal() || env::var("NO_COLOR").is_ok();

    let on_error = |e: search::Error, path: &Path| {
        let path = path.display();
        match e {
            search::Error::Io(err) => {
                eprintln!("{PROGRAM_NAME}:{path} IO error for operation on path {path}: {err}")
            }
            search::Error::LineRead(err) => {
                eprintln!("{PROGRAM_NAME}:{path} error reading lines: {err}")
            }
            search::Error::WorkerPanicked(message) => {
                eprintln!("{PROGRAM_NAME}:{path} internal error: {message}")
            }
        }
    };

    let write_output = |s: &str| print!("{s}");

    let result = if no_terminal {
        run(&options, MonochromePrinter::new, write_output, on_error)
    } else {
        run(&options, ColorPrinter::new, write_output, on_error)
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(run::Error::Pattern(err)) => {
            eprintln!("{PROGRAM_NAME}: invalid search pattern: {err}");
            ExitCode::FAILURE
        }
        Err(run::Error::SearchFailed) => ExitCode::FAILURE,
        Err(run::Error::Interrupted) => {
            eprintln!("{PROGRAM_NAME}: interrupted");
            // 128 + SIGINT(2), matching the exit status a shell would
            // report if the process had been killed by the signal
            // outright instead of catching and handling it.
            ExitCode::from(130)
        }
    }
}
