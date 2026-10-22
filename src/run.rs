use regex::RegexBuilder;

use crate::{
    concurrency::{SearchFailed, SpawnOptions, spawn_workers},
    printer::{BufferedOutput, Printer},
    search,
};

/// Entire program run error.
pub enum Error {
    /// One of the given patterns failed to compile; nothing was searched.
    Pattern(regex::Error),
    /// The search ran, but at least one path reported an error along the way.
    SearchFailed,
}

impl From<regex::Error> for Error {
    fn from(e: regex::Error) -> Self {
        Error::Pattern(e)
    }
}

impl From<SearchFailed> for Error {
    fn from(_: SearchFailed) -> Self {
        Error::SearchFailed
    }
}

/// Everything a run needs that isn't a closure.
pub struct Options {
    /// Patterns to search for, as raw regex strings (compiled by `run`).
    pub regexps: Vec<String>,
    /// Starting paths given on the command line.
    pub files: Vec<String>,
    /// Match patterns without regard to letter case.
    pub ignore_case: bool,
    /// Report lines that *don't* match instead of lines that do.
    pub invert_matches: bool,
    /// How many worker threads to use; `0` picks a number automatically.
    pub max_threads: u8,
}

/// Runs a whole search end to end: compiles the patterns, starts the
/// worker pool, and blocks until every result and error has been reported.
///
/// `make_printer` is cloned into every worker thread and called once per
/// path it picks up, so it needs to be `Send`. `on_error` only ever runs on
/// the calling thread, once per path that failed, with that path.
pub fn run<P: Printer + BufferedOutput>(
    options: &Options,
    make_printer: impl FnMut() -> P + Send + Clone + 'static,
    on_error: impl FnMut(search::Error, &str),
) -> Result<(), Error> {
    // Compile search patterns and fail fast if one of them is invalid
    let patterns = options
        .regexps
        .iter()
        .map(|regexp| {
            RegexBuilder::new(regexp)
                .case_insensitive(options.ignore_case)
                .build()
        })
        .collect::<Result<Vec<_>, _>>()?;

    let search_options = search::Options {
        invert_matches: options.invert_matches,
    };

    let handle = spawn_workers(
        SpawnOptions {
            patterns: &patterns,
            files: &options.files,
            search_options: &search_options,
            max_threads: options.max_threads as usize,
        },
        make_printer,
    );

    handle.consume_with(|output| print!("{output}"), on_error)?;

    Ok(())
}
