# rgrp (Rust grep)

  Yet another `grep` rewrite in Rust.
Just a hobby 100% artisanal hand-coded learning project.

Should support:
- regex matching
- parallel/concurrent search
- searching inside tars/compressed files
- ignoring/specifying files
- file read optimization with mmap

Basically roughly matching ripgrep abilities.

# CLI syntax

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
