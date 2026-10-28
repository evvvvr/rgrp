use std::io;
use std::io::prelude::*;

const BUFFER_SIZE_BYTES: usize = 65536; // 64 KB

#[derive(Debug, PartialEq)]
pub enum ReadResult {
    FileRead,
    BinaryFileSkipped,
}

#[derive(Debug)]
pub enum Error {
    ReadFailed(io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::ReadFailed(err) => write!(f, "error reading file: {err}"),
        }
    }
}

impl std::error::Error for Error {}

/// Knobs that change how `read_lines` splits and reports lines, independent
/// of where the bytes themselves come from.
#[derive(Clone, Copy, Default)]
pub struct ReadOptions {
    /// When true, a chunk containing a null byte stops the read early with
    /// [`ReadResult::BinaryFileSkipped`] instead of being split into lines.
    pub skip_on_null: bool,
    /// When true, a trailing `\r` right before a `\n` is treated as part of
    /// the line terminator and stripped, rather than as part of the line.
    pub crlf: bool,
}

pub fn read_lines(
    reader: &mut impl Read,
    options: ReadOptions,
    on_line_read: &mut impl FnMut(&[u8]),
) -> Result<ReadResult, Error> {
    let mut buffer = [0; BUFFER_SIZE_BYTES];

    // Bytes of the line currently being assembled. Lives outside the loop
    // so a line split across two reads survives into the next chunk -
    // ranges into `buffer` alone wouldn't, since `buffer` gets overwritten
    // by the next `read` call.
    let mut line: Vec<u8> = Vec::new();

    loop {
        let res = reader.read(&mut buffer);
        match res {
            Ok(0) => {
                // Done reading. Whatever's left in `line` is the last line,
                // if the file didn't end with a trailing '\n'.
                if !line.is_empty() {
                    if options.crlf && line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    on_line_read(&line);
                }

                return Ok(ReadResult::FileRead);
            }
            Ok(bytes_read) => {
                if options.skip_on_null && buffer[..bytes_read].contains(&0) {
                    // Check the whole chunk for a null byte before handing
                    // any of its lines to `on_line_read` - otherwise a null
                    // byte later in the chunk would still let lines earlier
                    // in the same chunk reach the caller first.
                    return Ok(ReadResult::BinaryFileSkipped);
                }

                let mut line_start = 0;

                for current_byte_index in 0..bytes_read {
                    let b = buffer[current_byte_index];

                    if b == b'\n' {
                        if line.is_empty() {
                            // Fast path: nothing carried over from a
                            // previous read, so this line lives entirely in
                            // `buffer` - hand the closure a slice straight
                            // into it instead of paying for a copy through
                            // `line` that the slow path below needs only
                            // for a line split across two reads.
                            let mut end = current_byte_index;
                            if options.crlf && end > line_start && buffer[end - 1] == b'\r' {
                                end -= 1;
                            }
                            on_line_read(&buffer[line_start..end]);
                        } else {
                            // everything since the last boundary, up to (not
                            // including) this newline, completes the line
                            line.extend_from_slice(&buffer[line_start..current_byte_index]);
                            if options.crlf && line.last() == Some(&b'\r') {
                                // `line` holds the whole assembled line by
                                // this point regardless of how many reads
                                // it took to get here, so this also covers
                                // a '\r' left over from the end of a
                                // previous chunk.
                                line.pop();
                            }
                            on_line_read(&line);
                            line.clear();
                        }
                        line_start = current_byte_index + 1;
                    }
                }

                // leftover bytes after the last newline in this chunk (or
                // the whole chunk, if it had no newline at all) carry over
                // into `line` to be completed by the next read
                line.extend_from_slice(&buffer[line_start..bytes_read]);
            }
            Err(err) => {
                return Err(Error::ReadFailed(err));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn can_read_lines_correctly() {
        let mut lines: Vec<Vec<u8>> = Vec::new();
        let mut input = Cursor::new(b"aa 11\n123 foo\n\nvoga");
        let res = read_lines(
            &mut input,
            ReadOptions {
                skip_on_null: true,
                crlf: false,
            },
            &mut |line| lines.push(line.to_vec()),
        );

        assert!(res.is_ok());
        assert_eq!(res.unwrap(), ReadResult::FileRead);
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn a_line_longer_than_one_buffer_is_assembled_across_reads() {
        // bigger than BUFFER_SIZE_BYTES, so reading it takes more than one
        // `read` call - the line only comes out whole if bytes left over
        // from one chunk are correctly carried into the next
        let long_line_len = BUFFER_SIZE_BYTES * 2 + 100;
        let mut input_bytes = vec![b'x'; long_line_len];
        input_bytes.push(b'\n');
        input_bytes.extend_from_slice(b"second line");

        let mut lines: Vec<Vec<u8>> = Vec::new();
        let mut input = Cursor::new(input_bytes);
        let res = read_lines(
            &mut input,
            ReadOptions {
                skip_on_null: false,
                crlf: false,
            },
            &mut |line| lines.push(line.to_vec()),
        );

        assert!(res.is_ok());
        assert_eq!(res.unwrap(), ReadResult::FileRead);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), long_line_len);
        assert!(lines[0].iter().all(|&b| b == b'x'));
        assert_eq!(lines[1], b"second line");
    }

    #[test]
    fn skips_rest_of_file_once_a_null_byte_is_seen() {
        // the null byte lands before the first newline, so no line has
        // completed yet when it's hit - reporting nothing is the correct
        // "this looks binary, don't search it" outcome
        let mut lines: Vec<Vec<u8>> = Vec::new();
        let mut input = Cursor::new(b"aa 11\x00 bc\nmore\n");
        let res = read_lines(
            &mut input,
            ReadOptions {
                skip_on_null: true,
                crlf: false,
            },
            &mut |line| lines.push(line.to_vec()),
        );

        assert!(res.is_ok());
        assert_eq!(res.unwrap(), ReadResult::BinaryFileSkipped);
        assert!(lines.is_empty());
    }

    #[test]
    fn reads_an_empty_file() {
        let mut lines: Vec<Vec<u8>> = Vec::new();
        let mut input = Cursor::new(b"");

        let res = read_lines(
            &mut input,
            ReadOptions {
                skip_on_null: true,
                crlf: false,
            },
            &mut |line| lines.push(line.to_vec()),
        );
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), ReadResult::FileRead);
        assert!(lines.is_empty());
    }

    struct AlwaysErrors;

    impl Read for AlwaysErrors {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("boom"))
        }
    }

    #[test]
    fn stops_after_a_read_error_instead_of_looping_forever() {
        let mut lines: Vec<Vec<u8>> = Vec::new();

        let res = read_lines(
            &mut AlwaysErrors,
            ReadOptions {
                skip_on_null: true,
                crlf: false,
            },
            &mut |line| lines.push(line.to_vec()),
        );

        let Err(Error::ReadFailed(err)) = res else {
            panic!("expected a ReadFailed error, got {res:?}");
        };
        assert_eq!(err.to_string(), "boom");
        assert!(lines.is_empty());
    }

    #[test]
    fn reads_a_single_line_file_withot_newlines() {
        let long_line_len = BUFFER_SIZE_BYTES * 2 + 100;
        let input_bytes = vec![b'x'; long_line_len];

        let mut lines: Vec<Vec<u8>> = Vec::new();
        let mut input = Cursor::new(input_bytes);
        let res = read_lines(
            &mut input,
            ReadOptions {
                skip_on_null: false,
                crlf: false,
            },
            &mut |line| lines.push(line.to_vec()),
        );

        assert!(res.is_ok());
        assert_eq!(res.unwrap(), ReadResult::FileRead);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), long_line_len);
        assert!(lines[0].iter().all(|&b| b == b'x'));
    }

    #[test]
    fn without_crlf_a_trailing_cr_is_kept_as_part_of_the_line() {
        let mut lines: Vec<Vec<u8>> = Vec::new();
        let mut input = Cursor::new(b"foo\r\nbar");
        let res = read_lines(
            &mut input,
            ReadOptions {
                skip_on_null: false,
                crlf: false,
            },
            &mut |line| lines.push(line.to_vec()),
        );

        assert!(res.is_ok());
        assert_eq!(lines, vec![b"foo\r".to_vec(), b"bar".to_vec()]);
    }

    #[test]
    fn crlf_strips_the_trailing_cr_even_when_split_across_a_read_boundary() {
        // The '\r' lands as the very last byte of the first `read()` call
        // (the buffer fills exactly), so the '\n' that completes the line
        // only shows up in the next chunk - the strip must apply to the
        // carried-over '\r', not just one still sitting in the same chunk
        // as the '\n'.
        let mut input_bytes = vec![b'x'; BUFFER_SIZE_BYTES - 1];
        input_bytes.push(b'\r');
        input_bytes.push(b'\n');
        input_bytes.extend_from_slice(b"second");

        let mut lines: Vec<Vec<u8>> = Vec::new();
        let mut input = Cursor::new(input_bytes);
        let res = read_lines(
            &mut input,
            ReadOptions {
                skip_on_null: false,
                crlf: true,
            },
            &mut |line| lines.push(line.to_vec()),
        );

        assert!(res.is_ok());
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), BUFFER_SIZE_BYTES - 1);
        assert!(lines[0].iter().all(|&b| b == b'x'));
        assert_eq!(lines[1], b"second");
    }
}
