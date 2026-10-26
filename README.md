# rgrp (Rust grep)

Yet another `grep` rewrite in Rust.
Just a hobby 100% artisanal hand-coded learning project.

## To Do:
- searching inside tars/compressed files
- follow symlinks (avoid infinite loops)
- ignoring/specifying search paths/file patterns 
- multiline matches
- file read optimization with mmap

Basically roughly matching ripgrep abilities.

## CLI syntax

**SYNOPSIS**

`rgrp [OPTIONS] [PATTERN] [PATH]`

**ARGUMENTS**

- `PATTERN`
  A regular expression used for searching. To match a pattern beginning with a dash, use the `-e`, `--regexp` option.

- `PATH`
  A file or directory to search. Directories are searched recursively. File paths specified explicitly on the command line override glob and ignore rules.


# How it works

- Read file line-by-line into a buffer.
- Run regex vs extracted lines.
- Repeat.

Only single-line pattern expressions bc. of this.

For parallelism/concurrency:
- launch separate traversal and matching jobs capped by max jobs limit.
- collect output

## Performance

Rough benchmark against `ripgrep` (`rg`), searching for `"function"` with both
tools set to ignore `.gitignore` and hidden-file rules and to treat binary
files as text (`rg --no-ignore --hidden -uu -a`), over a real-world
`node_modules` directory (~1.2GB, ~58k files), warm filesystem cache:

| | time | CPU usage | lines matched |
|---|---|---|---|
| `rgrp` | 2.41s | ~176% (under 2 cores) | 365,844 |
| `rg` | 1.78s | ~141% (under 1.5 cores) | 368,669 |

`rg` is roughly 35% faster here, and neither tool comes close to saturating
8 available cores - with this many small files, directory traversal and
per-file `open`/`read`/`close` syscalls dominate over actual regex matching,
so this workload is I/O-bound rather than CPU-bound.

The remaining ~3k match gap comes down to a small set of binary files
that only `rg` found matches in. `rgrp` reads each line into a `String` via
`BufRead::read_line`, which requires valid UTF-8; the moment it hits an
invalid byte sequence (near the start of these binaries, before any valid
line has been read), it gives up on that file entirely rather than skipping
just the bad line. `ripgrep`, by contrast, never requires valid UTF-8 in the
first place - it reads raw bytes split on `\n` and matches with a
byte-oriented regex engine, lossily decoding only when it needs to print.
Beyond those 35 files, the two tools agree almost exactly on which lines
match, out of ~29,600 files searched in common.

## License

MIT - see [LICENSE](LICENSE).
