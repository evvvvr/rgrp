# rgrp (Rust grep)

Yet another `grep` rewrite in Rust.
Allows to search inside zip/tar archives and compressed files.
Just a hobby 80% artisanal hand-coded learning project.

The core of it is written by hand.
AI is used to find and fix bugs (especially concurrency/mem usage related); generate tests; generate docs; performance testing.

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
  Number of worker threads to use. Defaults to `0`, which picks a count automatically based on available parallelism. With more than one thread (the default on any multi-core machine), output across different files/paths is **not** guaranteed to come out in the same order as the input paths, or even in a consistent order between runs - whichever worker finishes a file first gets to print it first. Pass `-j 1` if you need deterministic, strictly sequential output, at the cost of giving up parallelism.

- `-a, --text`
  Search binary files as if they were text, instead of skipping them (the default).

- `--crlf`
  Treat `\r\n` as the line terminator instead of just `\n`. By default, a trailing `\r` is kept as part of the line's content rather than stripped as part of the terminator.

- `-z, --search-archives`
  Search inside zip/tar archives and gzip/bzip2/xz/zstd/etc-compressed files, instead of reading them as opaque binary data (the default, under which they're typically skipped as binary - mirrors ripgrep's `-z`/`--search-zip`). Requires the corresponding external tool (`tar`, `unzip`, `gzip`, `bzip2`, `xz`, `zstd`, `lzip`, `lzma`, `lzop`, `uncompress`) to be installed for whichever archive/compression format is encountered.


# How it works

- Read a file line-by-line into a buffer.
- Run every pattern against each extracted line.
- Repeat.

Only single-line pattern expressions, because of this.

A zip/tar archive or a compressed file is unpacked through the matching
external tool (`tar`, `unzip`, `gzip`, ...) instead of a bundled decoder,
and streamed through the same line-by-line matching in one pass, member
by member, rather than loading the whole archive into memory first.

For parallelism/concurrency:
- A fixed-size pool of worker threads (sized by `-j`/`--threads`, or
  available parallelism by default) all pull from one shared queue of
  paths.
- Popping a directory expands it back into that same queue instead of
  recursing directly, so any worker can pick up its entries next.
  Popping a file searches it line-by-line as above.
- Each file/archive member's output streams back to the main thread as
  it's found, rather than after the whole path finishes, and output
  from one path can't be interleaved with another's mid-stream.

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
| default (skip binary) | `rgrp` | 2.32s | 90.3MB | 365,844 |
| default (skip binary) | `rg` | 1.78s | 73.1MB | 365,844 |
| `-a` (binary as text) | `rgrp` | 1.94s | 85.7MB | 368,669 |
| `-a` (binary as text) | `rg` | 1.80s | 72.9MB | 368,669 |

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
| default (no `-z`) | `rgrp` | 0.00s | 2.1MB | 0 |
| default (no `-z`) | `rg` | 0.14s | 1.9MB | 0 |
| `-z` | `rgrp` | 3.14s | 71.1MB ¶ | **365,844** |
| `-z` | `rg` | 0.03s | 1.9MB | 1 † |
| `-a -z` | `rgrp` | 3.11s | 74.0MB ¶ | 368,566 ‡ |
| `-a -z` | `rg` | 3.69s | 19.6MB | 369,164 § |

¶ Median of several runs, not a single sample - peak RSS for `rgrp -z`
swings by ~15-20MB run to run on this machine even with nothing in the code
changed between runs, so a single-sample reading can look like a
regression (or an improvement) that isn't really there.

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
`-z` run above, archive included, takes 3.14s. Zip archives are handled
the same way (`unzip -p` once, member boundaries read from `unzip -v`'s
size column), replacing an earlier version that found member boundaries by
matching `unzip`'s own progress-line text against the extracted content
stream - which a member whose content happened to contain a matching line
could throw off.

Peak memory for `-z`/`-a -z` dropped from an initial working version's
476MB/629MB (which buffered a whole archive's worth of output before
printing any of it) to the 71.1MB/74.0MB above, via two changes: flushing a
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
