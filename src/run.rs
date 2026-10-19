use crate::{
    printer::Printer,
    search::{self, search},
};

pub struct Error;

pub struct Options {
    pub regexp: String,
    pub files: Vec<String>,
}

pub fn run<P: Printer>(
    options: &Options,
    mut make_printer: impl FnMut() -> P,
    mut on_error: impl FnMut(search::Error, &str),
) -> Result<(), Error> {
    let mut had_error = false;

    for path in &options.files {
        if let Err(err) = search(&options.regexp, path, &mut make_printer) {
            had_error = true;
            on_error(err, path);
        }
    }

    if had_error { Err(Error) } else { Ok(()) }
}
