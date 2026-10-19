use clap::{CommandFactory, Parser};

use std::env;
use std::io::IsTerminal;
use std::process::ExitCode;

use rgrp::printer::{ColorPrinter, MonochromePrinter};
use rgrp::run::{Options, run};
use rgrp::search;

const PROGRAM_NAME: &str = "rgrp";

#[derive(Parser)]
#[command(name=PROGRAM_NAME)]
struct Cli {
    /// A pattern to search for
    #[arg(short = 'e', long = "regexp", value_name = "PATTERN")]
    regexp: Option<String>,

    /// Regular expression pattern to search for, followed by paths to search in
    /// (or, if -e/--regexp is given, only paths to search in)
    args: Vec<String>,
}

fn parse_options() -> Options {
    let cli = Cli::parse();

    let (pattern, path) = match cli.regexp {
        Some(pattern) => (pattern, cli.args),
        None => {
            let mut args = cli.args.into_iter();
            let Some(pattern) = args.next() else {
                Cli::command()
                    .error(
                        clap::error::ErrorKind::MissingRequiredArgument,
                        "the following required arguments were not provided:\n  <PATTERN>",
                    )
                    .exit();
            };
            (pattern, args.collect())
        }
    };

    Options {
        regexp: pattern,
        files: path,
    }
}

fn main() -> ExitCode {
    // CLI args:
    // -i, --ignore-case
    // -v, --invert-match

    let options = parse_options();

    let no_terminal = !std::io::stdout().is_terminal() || env::var("NO_COLOR").is_ok();

    let on_error = |e: search::Error, path: &str| match e {
        search::Error::Io(err) => {
            eprintln!("{PROGRAM_NAME}:{path} IO error for operation on path {path}: {err}")
        }
        search::Error::Pattern(err) => eprintln!("{PROGRAM_NAME}: pattern error: {err}"),
    };

    let result = if no_terminal {
        run(&options, || MonochromePrinter {}, on_error)
    } else {
        run(&options, ColorPrinter::new, on_error)
    };

    if result.is_err() {
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
