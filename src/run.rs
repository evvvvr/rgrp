use std::path::{Path, PathBuf};

use crate::{
    concurrency::{RunOutcome, SearchFailed, SpawnOptions, spawn_workers},
    printer::Printer,
    search,
    search::OwnedSearchConfig,
};

/// Entire program run error.
pub enum Error {
    /// The search ran, but at least one path reported an error along the way.
    SearchFailed,
    /// Ctrl-C (or another handled termination signal) stopped the search
    /// before every path was searched.
    Interrupted,
}

impl From<SearchFailed> for Error {
    fn from(_: SearchFailed) -> Self {
        Error::SearchFailed
    }
}

/// Everything a run needs that isn't a closure.
#[derive(Default)]
pub struct Options {
    /// What to match and how - patterns already compiled, since a
    /// caller building these from raw strings (and an `ignore_case`
    /// flag) can fail to do so, so that step happens before `Options`
    /// exists, not in `run` itself.
    pub search_config: OwnedSearchConfig,
    /// Starting paths given on the command line.
    pub files: Vec<PathBuf>,
    /// How many worker threads to use; `0` picks a number automatically.
    pub max_threads: u8,
}

/// Runs a whole search end to end: starts the worker pool and blocks
/// until every result and error has been reported.
///
/// `make_printer` and `write_output` are both shared across every
/// worker thread, so both need to be `Send + Sync`. `write_output` is
/// the caller's choice of where a worker's finished output actually
/// goes (e.g. `|s| print!("{s}")` for stdout). `on_error` only runs on
/// the calling thread, once per failed path.
pub fn run<P: Printer + 'static>(
    options: &Options,
    make_printer: impl Fn() -> P + Send + Sync + 'static,
    write_output: impl Fn(&str) + Send + Sync + 'static,
    on_error: impl FnMut(search::Error, &Path),
) -> Result<(), Error> {
    let outcome = spawn_workers(
        SpawnOptions {
            search_config: options.search_config.as_search_config(),
            files: &options.files,
            max_threads: options.max_threads as usize,
        },
        move || -> Box<dyn Printer> { Box::new(make_printer()) },
        write_output,
    )
    .consume_with(on_error)?;

    match outcome {
        RunOutcome::Completed => Ok(()),
        RunOutcome::Cancelled => Err(Error::Interrupted),
    }
}
