use crate::{
    printer::Printer,
    search::{Error, search},
};

pub fn run<'a, P: Printer>(
    pattern: &str,
    paths: impl Iterator<Item = &'a str>,
    mut make_printer: impl FnMut() -> P,
) -> Option<Error> {
    paths
        .map(|path| search(pattern, path, &mut make_printer()))
        .find_map(Result::err)
}
