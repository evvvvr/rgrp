use clap::{CommandFactory, Parser};

use std::env;
use std::io::IsTerminal;
use std::process::ExitCode;

use rgrp::printer::{ColorPrinter, MonochromePrinter};
use rgrp::run::{self, Options, run};
use rgrp::search;

const PROGRAM_NAME: &str = "rgrp";

#[derive(Parser)]
#[command(name=PROGRAM_NAME)]
struct Cli {
    // CLI args:
    // -i, --ignore-case
    // -v, --invert-match
    /// A pattern to search for (may be given multiple times).
    #[arg(short = 'e', long = "regexp", value_name = "PATTERN")]
    regexp: Vec<String>,

    /// Regular expression pattern to search for, followed by paths to search in
    /// (or, if -e/--regexp is given, only paths to search in).
    args: Vec<String>,

    /// When this flag is provided, all patterns will be searched case insensitively.
    #[arg(short = 'i', long = "ignore_case")]
    ignore_case: bool,
}

fn parse_options() -> Options {
    let cli = Cli::parse();

    let (patterns, path) = if cli.regexp.is_empty() {
        let mut args = cli.args.into_iter();
        let Some(pattern) = args.next() else {
            Cli::command()
                .error(
                    clap::error::ErrorKind::MissingRequiredArgument,
                    "the following required arguments were not provided:\n  <PATTERN>",
                )
                .exit();
        };
        (vec![pattern], args.collect())
    } else {
        (cli.regexp, cli.args)
    };

    Options {
        regexps: patterns,
        files: path,
        ignore_case: cli.ignore_case,
    }
}

fn main() -> ExitCode {
    let options = parse_options();

    let no_terminal = !std::io::stdout().is_terminal() || env::var("NO_COLOR").is_ok();

    let on_error = |e: search::Error, path: &str| match e {
        search::Error::Io(err) => {
            eprintln!("{PROGRAM_NAME}:{path} IO error for operation on path {path}: {err}")
        }
        search::Error::Pattern(err) => eprintln!("{PROGRAM_NAME}: pattern error: {err}"),
        search::Error::NonUtf8Path => {
            eprintln!("{PROGRAM_NAME}: skipping non-UTF8 path: {path}")
        }
    };

    let result = if no_terminal {
        run(&options, || MonochromePrinter {}, on_error)
    } else {
        run(&options, ColorPrinter::new, on_error)
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(run::Error::Pattern(err)) => {
            eprintln!("{PROGRAM_NAME}: invalid search pattern: {err}");
            ExitCode::FAILURE
        }
        Err(run::Error::SearchFailed) => ExitCode::FAILURE,
    }
}
