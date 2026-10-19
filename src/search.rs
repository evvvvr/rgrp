use regex::Regex;
use std::fs;
use std::io::BufRead;
use std::io::BufReader;

use crate::printer::Printer;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Pattern(regex::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(err) => write!(f, "{err}"),
            Error::Pattern(err) => write!(f, "invalid search pattern: {err}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<regex::Error> for Error {
    fn from(e: regex::Error) -> Self {
        Error::Pattern(e)
    }
}

pub fn search<T: Printer>(
    pattern: &str,
    path: &str,
    make_printer: &mut impl FnMut() -> T,
) -> Result<(), Error> {
    search_regex(&Regex::new(pattern)?, path, make_printer)
}

pub fn search_regex<T: Printer>(
    re: &regex::Regex,
    path: &str,
    make_printer: &mut impl FnMut() -> T,
) -> Result<(), Error> {
    // symlink_metadata (unlike metadata) reports the entry's own type instead
    // of following the symlink, so this check actually catches symlinks
    let metadata = fs::symlink_metadata(path)?;

    if metadata.is_symlink() {
        // skip for now - this could lead to infinite cycles
        return Ok(());
    }

    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let entry_path = entry.path();
            let Some(entry_path) = entry_path.to_str() else {
                // non-UTF8 path - skip rather than aborting the whole walk
                eprintln!(
                    "rgrp: skipping non-UTF8 path: {}",
                    entry_path.to_string_lossy()
                );
                continue;
            };
            search_regex(re, entry_path, make_printer)?;
        }
        return Ok(());
    }

    if !metadata.is_file() {
        // Something we don't know how to work with
        return Ok(());
    }

    return search_file(&re, path, &mut make_printer());
}

fn search_file<T: Printer>(pattern: &Regex, file_path: &str, printer: &mut T) -> Result<(), Error> {
    let file = fs::File::open(file_path)?;
    let mut reader = BufReader::new(file);

    let mut buf = String::new();
    let mut line_number = 1;

    loop {
        let res = reader.read_line(&mut buf);

        match res {
            Ok(0) => {
                // Reached EOF
                printer.on_eof();
                return Ok(());
            }
            Ok(_) => {
                let line = buf.trim_end_matches(['\r', '\n']);
                if let Some(m) = pattern.find(line) {
                    printer.print(file_path, line_number, line, &m);
                }
            }
            Err(err) => return Err(Error::Io(err)),
        }

        buf.clear();
        line_number = line_number + 1;
    }
}
