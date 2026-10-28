use clap::{CommandFactory, Parser};

use std::env;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rgrp::printer::{ColorPrinter, MonochromePrinter};
use rgrp::run::{self, Options, run};
use rgrp::search;

const PROGRAM_NAME: &str = "rgrp";

#[derive(Parser)]
#[command(name=PROGRAM_NAME)]
struct Cli {
    /// A pattern to search for (may be given multiple times).
    #[arg(short = 'e', long = "regexp", value_name = "PATTERN")]
    regexp: Vec<String>,

    /// Regular expression pattern to search for, followed by paths to search in
    /// (or, if -e/--regexp is given, only paths to search in).
    args: Vec<String>,

    /// When this flag is provided, all patterns will be searched case insensitively.
    #[arg(short = 'i', long = "ignore-case")]
    ignore_case: bool,

    /// This flag inverts matching.
    /// That is, instead of printing lines that match, rgrp will print lines that don't match.
    #[arg(short = 'v', long = "invert-match")]
    invert_match: bool,

    /// Set the number of threads to use.
    /// A value of 0 (which is the default) causes rgrp to choose the thread count using heuristics.
    #[arg(short = 'j', long = "threads", default_value_t = 0)]
    max_threads: u8,

    /// Search binary files as if they were text (by default, a file that
    /// looks binary is skipped).
    #[arg(short = 'a', long = "text")]
    text: bool,

    /// Treat CRLF ("\r\n") as the line terminator instead of just "\n"
    /// (by default, a trailing "\r" is kept as part of the line).
    #[arg(long = "crlf")]
    crlf: bool,
}

fn parse_options() -> Options {
    let cli = Cli::parse();

    let (patterns, paths) = if cli.regexp.is_empty() {
        let mut args = cli.args.into_iter();
        let Some(pattern) = args.next() else {
            Cli::command()
                .error(
                    clap::error::ErrorKind::MissingRequiredArgument,
                    "the following required arguments were not provided:\n  <PATTERN>",
                )
                .exit();
        };
        (vec![pattern], args.collect::<Vec<String>>())
    } else {
        (cli.regexp, cli.args)
    };

    Options {
        regexps: patterns,
        files: paths.into_iter().map(PathBuf::from).collect(),
        ignore_case: cli.ignore_case,
        invert_matches: cli.invert_match,
        max_threads: cli.max_threads,
        search_binary: cli.text,
        crlf: cli.crlf,
    }
}

fn main() -> ExitCode {
    let options = parse_options();

    let no_terminal = !std::io::stdout().is_terminal() || env::var("NO_COLOR").is_ok();

    let on_error = |e: search::Error, path: &Path| {
        let path = path.display();
        match e {
            search::Error::Io(err) => {
                eprintln!("{PROGRAM_NAME}:{path} IO error for operation on path {path}: {err}")
            }
            search::Error::Pattern(err) => eprintln!("{PROGRAM_NAME}: pattern error: {err}"),
            search::Error::LineRead(err) => {
                eprintln!("{PROGRAM_NAME}:{path} error reading lines: {err}")
            }
            search::Error::GenericError => todo!("fix"),
        }
    };

    let result = if no_terminal {
        run(&options, MonochromePrinter::new, on_error)
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
