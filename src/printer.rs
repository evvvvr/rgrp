use owo_colors::OwoColorize;

pub fn print(file_path: &str, line_no: u64, line: &str, _m: &regex::Match) {
    println!("{file_path}:{line_no} {line}")
}

pub fn print_in_color(file_path: &str, line_no: u64, line: &str, m: &regex::Match) {
    let before_match = &line[0..m.start()];
    let match_ = m.as_str();
    let after_match = &line[m.end()..];

    println!(
        "{}:{} {}{}{}",
        file_path.bold().purple(),
        line_no.green(),
        before_match,
        match_.red(),
        after_match
    )
}
