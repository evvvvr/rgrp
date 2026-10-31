use std::path::Path;

use globset::{GlobBuilder, GlobMatcher};

/// One compiled `--glob`/`--iglob` pattern.
#[derive(Clone)]
pub struct GlobSpec {
    pub matcher: GlobMatcher,
    pub negate: bool,
}

/// Compiles one `--glob`/`--iglob` argument into a [`GlobSpec`].
///
/// A leading `!` means "exclude" rather than "include", and is
/// stripped before the rest of the pattern is compiled. A pattern
/// with no `/` matches the basename at any depth (e.g. `*.rs` finds
/// `src/nested/deep.rs`), mirroring ripgrep's own `-g`/`.gitignore`
/// behavior - one with a `/` is left as-is, anchored wherever the
/// caller wrote it.
pub fn compile(pattern: &str, ignore_case: bool) -> Result<GlobSpec, globset::Error> {
    let (negate, pattern) = match pattern.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, pattern),
    };
    let pattern = if pattern.contains('/') {
        pattern.to_string()
    } else {
        format!("**/{pattern}")
    };

    let matcher = GlobBuilder::new(&pattern)
        .case_insensitive(ignore_case)
        .literal_separator(true)
        .build()?
        .compile_matcher();

    Ok(GlobSpec { matcher, negate })
}

/// Whether a *file* at `path` should be searched, given every
/// `--glob`/`--iglob` in their original command-line order. Mirrors
/// ripgrep: if every glob is a negation, the baseline is "include
/// everything" and negations carve out exceptions; the moment any
/// glob is a plain (non-negated) pattern, the baseline flips to
/// "exclude everything" and only a matching glob can include a path.
/// Whichever glob in the list matches *last* decides the outcome,
/// overriding any earlier match.
pub fn is_file_included(path: &Path, globs: &[GlobSpec]) -> bool {
    let mut included = globs.iter().all(|glob| glob.negate);

    for glob in globs {
        if glob.matcher.is_match(path) {
            included = !glob.negate;
        }
    }

    included
}

/// Whether a *directory* at `path` should be pruned - never walked at
/// all - given every `--glob`/`--iglob`. Unlike a file, a directory is
/// walked by default regardless of whether any positive glob matches
/// its own name (`*.rs` never needs `src/` itself to look like a
/// `.rs` file for `src/lib.rs` to still be reachable) - only an
/// explicit negation (`!target`) can prune it. As with files, the
/// last glob in command-line order that matches wins.
pub fn is_dir_pruned(path: &Path, globs: &[GlobSpec]) -> bool {
    let mut pruned = false;

    for glob in globs {
        if glob.matcher.is_match(path) {
            pruned = glob.negate;
        }
    }

    pruned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_pattern_matches_and_is_not_negated() {
        let glob = compile("*.rs", false).unwrap();
        assert!(!glob.negate);
        assert!(glob.matcher.is_match("main.rs"));
        assert!(!glob.matcher.is_match("main.txt"));
    }

    #[test]
    fn leading_bang_is_stripped_and_sets_negate() {
        let glob = compile("!*.rs", false).unwrap();
        assert!(glob.negate);
        assert!(glob.matcher.is_match("main.rs"));
    }

    #[test]
    fn slash_less_pattern_matches_at_any_depth() {
        let glob = compile("*.rs", false).unwrap();
        assert!(glob.matcher.is_match("main.rs"));
        assert!(glob.matcher.is_match("src/lib.rs"));
        assert!(glob.matcher.is_match("src/nested/deep.rs"));
    }

    #[test]
    fn pattern_with_a_slash_is_left_anchored_as_written() {
        let glob = compile("src/*.rs", false).unwrap();
        assert!(glob.matcher.is_match("src/lib.rs"));
        // Not auto-promoted to match at any depth, unlike a
        // slash-less pattern - nothing was prepended to it.
        assert!(!glob.matcher.is_match("other/src/lib.rs"));
    }

    #[test]
    fn case_insensitive_flag_only_applies_when_requested() {
        let sensitive = compile("*.RS", false).unwrap();
        assert!(!sensitive.matcher.is_match("main.rs"));

        let insensitive = compile("*.RS", true).unwrap();
        assert!(insensitive.matcher.is_match("main.rs"));
    }

    #[test]
    fn an_invalid_pattern_is_an_error() {
        assert!(compile("{unclosed", false).is_err());
    }

    #[test]
    fn no_globs_includes_every_file() {
        assert!(is_file_included(Path::new("anything.txt"), &[]));
    }

    #[test]
    fn a_single_positive_glob_acts_as_an_allow_list() {
        let globs = [compile("*.rs", false).unwrap()];
        assert!(is_file_included(Path::new("main.rs"), &globs));
        assert!(!is_file_included(Path::new("main.txt"), &globs));
    }

    #[test]
    fn negation_only_keeps_everything_except_what_it_excludes() {
        let globs = [compile("!*.txt", false).unwrap()];
        assert!(is_file_included(Path::new("main.rs"), &globs));
        assert!(!is_file_included(Path::new("notes.txt"), &globs));
    }

    #[test]
    fn the_last_matching_glob_in_command_line_order_wins_for_files() {
        let exclude_then_reinclude = [
            compile("!*.rs", false).unwrap(),
            compile("main.rs", false).unwrap(),
        ];
        assert!(is_file_included(
            Path::new("main.rs"),
            &exclude_then_reinclude
        ));
        assert!(!is_file_included(
            Path::new("other.rs"),
            &exclude_then_reinclude
        ));

        let include_then_exclude = [
            compile("*.rs", false).unwrap(),
            compile("!main.rs", false).unwrap(),
        ];
        assert!(!is_file_included(
            Path::new("main.rs"),
            &include_then_exclude
        ));
        assert!(is_file_included(
            Path::new("other.rs"),
            &include_then_exclude
        ));
    }

    #[test]
    fn no_globs_prunes_no_directory() {
        assert!(!is_dir_pruned(Path::new("anything"), &[]));
    }

    #[test]
    fn a_positive_glob_never_prunes_a_directory_on_its_own() {
        // `*.rs` matching no directory name must never stop the walk
        // from reaching `src/lib.rs` - only an explicit negation can.
        let globs = [compile("*.rs", false).unwrap()];
        assert!(!is_dir_pruned(Path::new("src"), &globs));
    }

    #[test]
    fn a_negated_glob_prunes_a_matching_directory() {
        let globs = [compile("!target", false).unwrap()];
        assert!(is_dir_pruned(Path::new("target"), &globs));
        assert!(!is_dir_pruned(Path::new("src"), &globs));
    }

    #[test]
    fn the_last_matching_glob_in_command_line_order_wins_for_directories() {
        let globs = [
            compile("!target", false).unwrap(),
            compile("target", false).unwrap(),
        ];
        assert!(!is_dir_pruned(Path::new("target"), &globs));
    }
}
