use regex::bytes::{Match, Regex};

pub struct Options {
    pub invert_matches: bool,
}

/// Matches one line against every pattern, writing the result into
/// `matches` (cleared first, so it holds exactly this line's matches on
/// return) and reporting whether the line should be reported at all.
///
/// `matches` is owned by the caller and reused across many calls
/// (typically one per line of a file) instead of being allocated fresh
/// here each time - for a file where most lines match, that's otherwise
/// a small heap allocation per line for no reason, since the previous
/// line's buffer is already the right shape to hold the next line's.
/// The intermediate per-pattern matches (which borrow from `line`, so
/// can't be reused the same way across lines with a different
/// lifetime each time) are still collected fresh each call.
pub fn match_line(
    patterns: &[Regex],
    options: &Options,
    line: &[u8],
    matches: &mut Vec<(usize, usize)>,
) -> bool {
    matches.clear();

    let mut all_matches: Vec<Match> = Vec::new();

    // flatmap [[match 1, match 2], [match 3, match 4]...] -> [match 1, match 2, match 3, match 4...]
    for pattern in patterns {
        all_matches.extend(pattern.find_iter(line));
    }

    if options.invert_matches {
        // we are interested in lines with zero total matches
        if all_matches.is_empty() {
            // match is the entire line
            matches.push((0, line.len()));
            return true;
        }
        return false;
    }

    if all_matches.is_empty() {
        return false;
    }

    // Walk over all matches for all patterns sequentially starting from
    // the beginning of the line
    all_matches.sort_by_key(|m| m.start());
    matches.extend(all_matches.iter().map(|m| (m.start(), m.end())));

    true
}
