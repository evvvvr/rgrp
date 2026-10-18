use regex::Regex;
use std::fs;
use std::io::BufRead;
use std::io::BufReader;
use std::io::ErrorKind;

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

pub fn search<T: Printer>(pattern: &str, path: &str, printer: &mut T) -> Result<(), Error> {
    let binding = fs::canonicalize(path)?;
    let Some(path) = binding.to_str() else {
        return Err(Error::Io(std::io::Error::new(
            ErrorKind::InvalidFilename,
            "Invalid path",
        )));
    };

    let re = Regex::new(pattern)?;
    let file = fs::File::open(path)?;
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
                if let Some(m) = re.find(line) {
                    printer.print(path, line_number, line, &m);
                }
            }
            Err(err) => return Err(Error::Io(err)),
        }

        buf.clear();
        line_number = line_number + 1;
    }
}
