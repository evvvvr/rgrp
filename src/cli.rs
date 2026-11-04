use clap::{CommandFactory, FromArgMatches, Parser};
use regex::bytes::RegexBuilder;

use std::ffi::OsString;
use std::path::PathBuf;

use rgrp::glob;
use rgrp::run::Options;
use rgrp::search::{MatchBehavior, OwnedSearchConfig};

pub const PROGRAM_NAME: &str = "rgrp";

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

/// Parses `rgrp`'s options from an explicit argument list (its first
/// element conventionally being the program name, same as
/// `std::env::args_os()`), returning a [`clap::Error`] on failure
/// instead of printing it and exiting the process directly - that's
/// `parse_options`'s job, so this can be driven from tests too.
fn parse_options_from<I, T>(args: I) -> Result<Options, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let matches = Cli::command().try_get_matches_from(args)?;
    let cli = Cli::from_arg_matches(&matches)?;

    // `indices_of` reports each value's position in the original
    // argument list, comparable across different args - zipping each
    // one's values up with its own indices and sorting the merged set
    // by index recovers the relative order `--glob`/`--iglob` were
    // actually given in, which two separate `Vec<String>` fields on
    // `Cli` can't preserve on their own.
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
            glob::compile(pattern, ignore_case).map_err(|e| {
                Cli::command().error(
                    clap::error::ErrorKind::InvalidValue,
                    format!("invalid --glob/--iglob pattern {pattern:?}: {e}"),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let (regexps, paths) = if cli.regexp.is_empty() {
        let mut args = cli.args.into_iter();
        let Some(pattern) = args.next() else {
            return Err(Cli::command().error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "the following required arguments were not provided:\n  <PATTERN>",
            ));
        };
        (vec![pattern], args.collect::<Vec<String>>())
    } else {
        (cli.regexp, cli.args)
    };

    let patterns = regexps
        .into_iter()
        .map(|regexp| {
            RegexBuilder::new(&regexp)
                .case_insensitive(cli.ignore_case)
                .build()
                .map_err(|e| {
                    Cli::command().error(
                        clap::error::ErrorKind::InvalidValue,
                        format!("invalid search pattern {regexp:?}: {e}"),
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Options {
        search_config: OwnedSearchConfig {
            patterns,
            globs,
            behavior: MatchBehavior {
                invert_matches: cli.invert_match,
                search_binary: cli.text,
                crlf: cli.crlf,
                search_archives: cli.search_archives,
            },
        },
        files: paths.into_iter().map(PathBuf::from).collect(),
        max_threads: cli.max_threads,
    })
}

/// Parses options from the real process arguments, printing clap's own
/// formatted usage/error message and exiting the process on failure.
pub fn parse_options() -> Options {
    parse_options_from(std::env::args_os()).unwrap_or_else(|e| e.exit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn parse(args: &[&str]) -> Options {
        let mut full_args = vec!["rgrp"];
        full_args.extend_from_slice(args);
        parse_options_from(full_args).unwrap()
    }

    fn try_parse(args: &[&str]) -> Result<Options, clap::Error> {
        let mut full_args = vec!["rgrp"];
        full_args.extend_from_slice(args);
        parse_options_from(full_args)
    }

    #[test]
    fn first_positional_is_the_pattern_and_the_rest_are_paths() {
        let options = parse(&["needle", "a", "b"]);
        assert_eq!(options.search_config.patterns.len(), 1);
        assert!(options.search_config.patterns[0].is_match(b"needle"));
        assert_eq!(options.files, vec![PathBuf::from("a"), PathBuf::from("b")]);
    }

    #[test]
    fn regexp_flag_treats_every_positional_as_a_path() {
        let options = parse(&["-e", "needle", "a", "b"]);
        assert_eq!(options.search_config.patterns.len(), 1);
        assert!(options.search_config.patterns[0].is_match(b"needle"));
        assert_eq!(options.files, vec![PathBuf::from("a"), PathBuf::from("b")]);
    }

    #[test]
    fn regexp_flag_may_be_given_multiple_times() {
        let options = parse(&["-e", "foo", "-e", "bar", "a"]);
        assert_eq!(options.search_config.patterns.len(), 2);
        assert!(options.search_config.patterns[0].is_match(b"foo"));
        assert!(options.search_config.patterns[1].is_match(b"bar"));
        assert_eq!(options.files, vec![PathBuf::from("a")]);
    }

    #[test]
    fn no_pattern_at_all_is_an_error() {
        assert!(try_parse(&[]).is_err());
    }

    #[test]
    fn an_invalid_regex_pattern_is_an_error() {
        assert!(try_parse(&["("]).is_err());
    }

    #[test]
    fn ignore_case_flag_makes_the_compiled_pattern_match_regardless_of_letter_case() {
        let options = parse(&["needle"]);
        assert!(!options.search_config.patterns[0].is_match(b"NEEDLE"));

        let options = parse(&["-i", "needle"]);
        assert!(options.search_config.patterns[0].is_match(b"NEEDLE"));
    }

    #[test]
    fn boolean_flags_are_off_by_default_and_on_when_given() {
        let defaults = parse(&["needle"]);
        assert!(!defaults.search_config.behavior.invert_matches);
        assert!(!defaults.search_config.behavior.search_binary);
        assert!(!defaults.search_config.behavior.crlf);
        assert!(!defaults.search_config.behavior.search_archives);
        assert_eq!(defaults.max_threads, 0);

        let set = parse(&["-i", "-v", "-a", "--crlf", "-z", "-j", "4", "needle"]);
        assert!(set.search_config.behavior.invert_matches);
        assert!(set.search_config.behavior.search_binary);
        assert!(set.search_config.behavior.crlf);
        assert!(set.search_config.behavior.search_archives);
        assert_eq!(set.max_threads, 4);
    }

    #[test]
    fn glob_and_iglob_are_merged_in_original_command_line_order() {
        // If the merge didn't preserve relative order, `!secret.txt`
        // wouldn't necessarily be the last matching glob, and
        // `secret.txt` would wrongly stay included.
        let options = parse(&["-g", "*", "-g", "!secret.txt", "needle"]);
        assert!(!glob::is_file_included(
            Path::new("secret.txt"),
            &options.search_config.globs
        ));
        assert!(glob::is_file_included(
            Path::new("other.txt"),
            &options.search_config.globs
        ));
    }

    #[test]
    fn glob_and_iglob_interleave_by_their_actual_argv_position() {
        // `-g`/`--iglob` interleaved three times - the merge must
        // reconstruct this exact order, not group all `-g`s before
        // all `--iglob`s (or vice versa).
        let options = parse(&[
            "-g", "!*.rs", "--iglob", "MAIN.RS", "-g", "!main.rs", "needle",
        ]);
        // `!*.rs` excludes everything .rs, `MAIN.RS` (case-insensitive)
        // re-includes main.rs, then `!main.rs` excludes it again -
        // only this exact order produces "excluded" as the result.
        assert!(!glob::is_file_included(
            Path::new("main.rs"),
            &options.search_config.globs
        ));
        assert!(!glob::is_file_included(
            Path::new("other.rs"),
            &options.search_config.globs
        ));
    }

    #[test]
    fn an_invalid_glob_pattern_is_an_error() {
        assert!(try_parse(&["-g", "{unclosed", "needle"]).is_err());
    }
}
