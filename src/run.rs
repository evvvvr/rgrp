use crate::{
    printer::Printer,
    search::{self, search},
};

pub struct Error;

pub fn run<'a, P: Printer>(
    pattern: &str,
    paths: impl Iterator<Item = &'a str>,
    mut make_printer: impl FnMut() -> P,
    mut on_error: impl FnMut(search::Error, &str),
) -> Result<(), Error> {
    let mut had_error = false;

    for path in paths {
        if let Err(err) = search(pattern, path, &mut make_printer) {
            had_error = true;
            on_error(err, path);
        }
    }

    if had_error { Err(Error) } else { Ok(()) }
}
