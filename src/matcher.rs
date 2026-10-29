use regex::bytes::{Match, Regex};

pub struct Options {
    pub invert_matches: bool,
}

/// Matches one line against every pattern, writing the result into
/// `matches` (cleared first) and reporting whether the line should be
/// reported at all.
///
/// `matches` is owned by the caller and reused across calls instead of
/// allocated fresh each time, avoiding a small heap allocation per
/// line for a file where most lines match.
pub fn match_line(
    patterns: &[Regex],
    options: &Options,
    line: &[u8],
    matches: &mut Vec<(usize, usize)>,
) -> bool {
    matches.clear();

    let mut all_matches: Vec<Match> = Vec::new();

    for pattern in patterns {
        all_matches.extend(pattern.find_iter(line));
    }

    if options.invert_matches {
        if all_matches.is_empty() {
            matches.push((0, line.len()));
            return true;
        }
        return false;
    }

    if all_matches.is_empty() {
        return false;
    }

    all_matches.sort_by_key(|m| m.start());
    matches.extend(all_matches.iter().map(|m| (m.start(), m.end())));

    true
}
