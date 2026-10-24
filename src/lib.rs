//! A small, parallel grep-like line search tool.
//!
//! The pieces split by concern: [`search`] walks one path (file or
//! directory) and matches its lines against patterns, [`printer`] turns a
//! matched line into printable text, [`concurrency`] spawns and coordinates
//! the worker thread pool that drives the walk, and [`run`] wires all three
//! together behind the one function the CLI actually calls.

/// Spawns and coordinates the worker thread pool that walks paths and
/// funnels their results back to the caller.
pub mod concurrency;
/// Turns a matched line into printable text, in plain or colored form.
pub mod printer;
/// The public entry point: builds the search config, starts the workers,
/// and reports output and errors as they arrive.
pub mod run;
/// Walks a single path - searching a file line by line, or queuing a
/// directory's entries for later - against a set of patterns.
pub mod search;

pub mod line_reader;
