# rgrp (Rust grep)

Yet another `grep` rewrite in Rust.
Just a hobby 100% artisanal hand-coded learning project.

Allows to search inside tarballs and compressed files.

## To Do:
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

**OPTIONS**

- `-e, --regexp <PATTERN>`
  A pattern to search for. May be given multiple times to search for several patterns at once. When used, every positional argument is treated as a `PATH` instead of the first one being `PATTERN`.

- `-i, --ignore-case`
  Match patterns without regard to letter case.

- `-v, --invert-match`
  Print lines that *don't* match instead of lines that do.

- `-j, --threads <N>`
  Number of worker threads to use. Defaults to `0`, which picks a count automatically based on available parallelism.

- `-a, --text`
  Search binary files as if they were text, instead of skipping them (the default).

- `--crlf`
  Treat `\r\n` as the line terminator instead of just `\n`. By default, a trailing `\r` is kept as part of the line's content rather than stripped as part of the terminator.

- `-z, --search-archives`
  Search inside zip/tar archives and gzip/bzip2/xz/zstd/etc-compressed files, instead of reading them as opaque binary data (the default, under which they're typically skipped as binary - mirrors ripgrep's `-z`/`--search-zip`). Requires the corresponding external tool (`tar`, `unzip`, `gzip`, `bzip2`, `xz`, `zstd`, `lzip`, `lzma`, `lzop`, `uncompress`) to be installed for whichever archive/compression format is encountered.


# How it works

- Read file line-by-line into a buffer.
- Run regex vs extracted lines.
- Repeat.

Only single-line pattern expressions bc. of this.

For parallelism/concurrency:
- launch separate traversal and matching jobs capped by max jobs limit.
- collect output

## Performance

Benchmarks against `ripgrep` (`rg`), searching for `"function"` over a
real-world `node_modules` directory (~1.2GB, ~58k files) and an equivalent
`node_modules.tar.gz` built from that same tree (292MB compressed, 64,513
entries), warm filesystem cache, single run each. `rg` is always run with
`-uu` (`--no-ignore --hidden`), since it respects `.gitignore`/hidden-file
rules by default and `rgrp` has no such concept at all; otherwise both
tools are given matching flags on each row.

### Directory

| setting | tool | time | peak RSS | lines matched |
|---|---|---|---|---|
| default (skip binary) | `rgrp` | 2.31s | 87.9MB | 365,844 |
| default (skip binary) | `rg` | 1.74s | 75.8MB | 365,844 |
| `-a` (binary as text) | `rgrp` | 1.97s | 85.0MB | 368,669 |
| `-a` (binary as text) | `rg` | 1.69s | 72.8MB | 368,669 |

With matching flags, `rgrp` and `rg` agree exactly on every line matched in
both configurations. `rg` is consistently faster here by ~15-20%; with ~58k
mostly-small files, directory traversal and per-file `open`/`read`/`close`
syscalls dominate over actual regex matching, so this workload is I/O-bound
rather than CPU-bound, and neither tool comes close to saturating available
cores.

### Inside an archive (`-z`)

`-z` unpacks zip/tar archives and gzip/bzip2/xz/etc-compressed files instead
of reading them as opaque binary data:

| setting | tool | time | peak RSS | lines matched |
|---|---|---|---|---|
| default (no `-z`) | `rgrp` | 0.00s | 2.8MB | 0 |
| default (no `-z`) | `rg` | 0.13s | 3.8MB | 0 |
| `-z` | `rgrp` | 3.17s | 60.0MB | **365,844** |
| `-z` | `rg` | 0.00s | 4.0MB | 1 † |
| `-a -z` | `rgrp` | 3.11s | 55.7MB | 368,566 ‡ |
| `-a -z` | `rg` | 3.43s | 21.8MB | 369,164 § |

† `rg -z` only decompresses a single gzip stream - it has no concept of a
tar member. The decompressed tar bytes (headers and padding) look binary to
it, so without `-a` it just prints its own `binary file matches` notice and
finds nothing real inside. `rgrp -z` fully unpacks the tarball's structure
and finds every match, each correctly attributed to its own member path.

‡ A few lines short of the clean directory figure (368,669) for reasons not
yet root-caused - worth investigating further.

§ `rg -a -z` is scanning the raw concatenated tar bytes (headers, padding,
and every member's content all together) as one flat blob with no file
boundaries at all, so this number isn't a clean per-member comparison the
way the directory figures are.

Earlier, `rgrp`'s tar support extracted each member with its own separate
`tar -xOf` call, which - for a compressed archive - has to decompress from
the start every time to reach that member: quadratic in the member count.
For this archive's 64,513 entries, that was on the order of hours,
estimated from timing a single late-archive extraction rather than run to
completion. It's a single streaming pass now (`tar -xOf` once, member
boundaries read from `tar -tvf`'s size column), which is linear - the whole
`-z` run above, archive included, takes 3.17s.

Peak memory for `-z`/`-a -z` dropped from an initial working version's
476MB/629MB (which buffered a whole archive's worth of output before
printing any of it) to the 60MB/56MB above, via two changes: flushing a
printer's output per archive *member* instead of per whole archive, and
bounding how much of a single member is ever read into memory at once
(regardless of how large that member itself is) rather than buffering it
whole.

### Allocation overhead on dense matches

Both workloads above have modest match density per file, so they don't
exercise two smaller allocation fixes made along the way: reusing one
match-offset buffer across every line of a file instead of allocating fresh
per matching line, and removing a per-match intermediate `String`
allocation in the colored printer. Isolated on a synthetic 2,000,000-line
file where every line matches, warm cache, 3 runs each, piped output:

| | before | after |
|---|---|---|
| user CPU time | 0.20s (×3) | 0.17-0.18s |
| peak RSS | ~4.3MB | ~4.3MB (flat) |

A consistent ~10-15% CPU-time reduction on this kind of dense-match file,
output confirmed byte-identical between both versions. Memory is flat, as
expected - this is about per-line allocation churn, not peak memory.

## License

MIT - see [LICENSE](LICENSE).
