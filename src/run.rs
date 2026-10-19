use regex::Regex;

use crate::{
    printer::Printer,
    search::{self, search},
};

pub enum Error {
    Pattern(regex::Error),
    SearchFailed,
}

impl From<regex::Error> for Error {
    fn from(e: regex::Error) -> Self {
        Error::Pattern(e)
    }
}

pub struct Options {
    pub regexp: String,
    pub files: Vec<String>,
}

pub fn run<P: Printer>(
    options: &Options,
    mut make_printer: impl FnMut() -> P,
    mut on_error: impl FnMut(search::Error, &str),
) -> Result<(), Error> {
    let reg = Regex::new(&options.regexp)?;

    let mut had_error = false;

    for path in &options.files {
        if let Err(err) = search(&reg, path, &mut make_printer, &mut on_error) {
            had_error = true;
            on_error(err, path);
        }
    }

    if had_error {
        Err(Error::SearchFailed)
    } else {
        Ok(())
    }
}
