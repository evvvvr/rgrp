use clap::{CommandFactory, FromArgMatches, Parser};

use std::env;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rgrp::glob;
use rgrp::printer::{ColorPrinter, MonochromePrinter};
use rgrp::run::{self, Options, run};
use rgrp::search;

const PROGRAM_NAME: &str = "rgrp";

#[derive(Parser)]
#[command(name=PROGRAM_NAME, version)]
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

    /// Search inside zip/tar archives and gzip/bzip2/xz/etc-compressed
    /// files (by default, they're read as opaque binary data like any
    /// other file, and typically skipped as binary).
    #[arg(short = 'z', long = "search-archives")]
    search_archives: bool,

    /// Only search paths matching this glob (may be given multiple
    /// times; a leading `!` excludes instead of includes; when both
    /// --glob and --iglob are given, whichever comes later on the
    /// command line wins for a given path).
    #[arg(short = 'g', long = "glob", value_name = "GLOB")]
    glob: Vec<String>,

    /// Same as --glob, but matched case-insensitively.
    #[arg(long = "iglob", value_name = "GLOB")]
    iglob: Vec<String>,
}

fn parse_options() -> Options {
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());

    let glob_indexes = matches.indices_of("glob").into_iter().flatten();
    let iglob_indexes = matches.indices_of("iglob").into_iter().flatten();

    let mut tagged: Vec<(usize, &str, bool)> = glob_indexes
        .zip(cli.glob.iter())
        .map(|(i, p)| (i, p.as_str(), false))
        .chain(
            iglob_indexes
                .zip(cli.iglob.iter())
                .map(|(i, p)| (i, p.as_str(), true)),
        )
        .collect();
    tagged.sort_by_key(|(i, ..)| *i);

    let globs = tagged
        .into_iter()
        .map(|(_, pattern, ignore_case)| {
            glob::compile(pattern, ignore_case).unwrap_or_else(|e| {
                Cli::command()
                    .error(
                        clap::error::ErrorKind::InvalidValue,
                        format!("invalid --glob/--iglob pattern {pattern:?}: {e}"),
                    )
                    .exit()
            })
        })
        .collect();

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
        search_archives: cli.search_archives,
        globs,
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
            search::Error::LineRead(err) => {
                eprintln!("{PROGRAM_NAME}:{path} error reading lines: {err}")
            }
            search::Error::WorkerPanicked(message) => {
                eprintln!("{PROGRAM_NAME}:{path} internal error: {message}")
            }
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
        Err(run::Error::Interrupted) => {
            eprintln!("{PROGRAM_NAME}: interrupted");
            // 128 + SIGINT(2), matching the exit status a shell would
            // report if the process had been killed by the signal
            // outright instead of catching and handling it.
            ExitCode::from(130)
        }
    }
}
