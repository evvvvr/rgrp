use std::env;
use std::fs;
use std::io::BufRead;
use std::io::BufReader;
use std::process::ExitCode;
use regex::Regex;

const USAGE_HINT: &str = "usage: rgrp [OPTIONS] PATTERN PATH";

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let pattern = args.next();
    let path = args.next();

    let (Some(pattern), Some(path)) = (pattern, path) else {
        eprintln!("{USAGE_HINT}");
        return ExitCode::FAILURE;
    };

    let (Some(pattern), Some(path)) = (pattern.to_str(), path.to_str()) else {
        eprintln!("arguments must be valid UTF-8");
        return ExitCode::FAILURE;
    };

    if let Err(e) = search(pattern, path) {
        eprintln!("{e}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

fn search(pattern: &str, path: &str) -> Result<(), std::io::Error> {
    let re = Regex::new(pattern).unwrap(); // TODO: fix error handling
    let file = fs::File::open(path)?;
    let mut reader = BufReader::new(file);

    let mut buf = String::new();
    let mut line_number = 1;

    loop {
        let res = reader.read_line(&mut buf);

        match res {
            Ok(0) => return Ok(()), // Reached EOF
            Ok(_) => {
                let line = buf.trim_end_matches(['\r', '\n']);
                if re.is_match(line) {
                    println!("{line}");
                }
            },
            Err(err) => return Err(err),
        }

        buf.clear();
        line_number = line_number + 1;
    }    
}