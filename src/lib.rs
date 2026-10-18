pub mod search {
    use std::fs;
    use std::io::BufRead;
    use std::io::BufReader;
    use regex::Regex;

    #[derive(Debug)]
    pub enum Error {
        Io(std::io::Error),
        Pattern(regex::Error),
    }

    impl std::fmt::Display for Error {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Error::Io(err) => write!(f, "{err}"),
                Error::Pattern(err) => write!(f, "invalid search pattern: {err}")
            }
        }
    }

    impl std::error::Error for Error {}

    impl From<std::io::Error> for Error {
        fn from(e: std::io::Error) -> Self { Error::Io(e) }
    }

    impl From<regex::Error> for Error {
        fn from(e: regex::Error) -> Self { Error::Pattern(e) }
    }

    pub fn search(pattern: &str, path: &str) -> Result<(), Error> {
        let re = Regex::new(pattern)?; 
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
                Err(err) => return Err(Error::Io(err)),
            }

            buf.clear();
            line_number = line_number + 1;
        }    
    }
}