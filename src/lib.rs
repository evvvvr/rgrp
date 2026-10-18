pub mod search {
    use std::fs;
    use std::io::BufRead;
    use std::io::BufReader;
    use regex::Regex;

    pub fn search(pattern: &str, path: &str) -> Result<(), std::io::Error> {
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
}