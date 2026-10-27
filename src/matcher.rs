use regex::bytes::{Match, Regex};

pub struct Options {
    pub invert_matches: bool,
}

// [{Line, match 1}, {Line, match 2}...] -> { Line, [match1, match2..] }
pub struct MatchingLine<'a> {
    pub line: &'a [u8],
    pub matches: Vec<(usize, usize)>,
}

pub fn match_line<'a>(
    patterns: &[Regex],
    options: &Options,
    line: &'a [u8],
) -> Option<MatchingLine<'a>> {
    let mut all_matches: Vec<Match> = Vec::new();

    // flatmap [[match 1, match 2], [match 3, match 4]...] -> [match 1, match 2, match 3, match 4...]
    for pattern in patterns {
        for m in pattern.find_iter(line) {
            all_matches.push(m);
        }
    }

    if options.invert_matches {
        // we are interested in lines with zero total matches
        if all_matches.is_empty() {
            return Option::Some(MatchingLine {
                line,
                matches: Vec::new(),
            });
        }
    } else if !all_matches.is_empty() {
        // Walk over all matches for all patterns sequentially
        // starting from the beginning of the line
        all_matches.sort_by_key(|a| a.start());

        let all_matches = all_matches.iter().map(|m| (m.start(), m.end())).collect();

        return Option::Some(MatchingLine {
            line,
            matches: all_matches,
        });
    }

    Option::None
}
