use std::path::Path;

/// A compression format applied to a single file - never an archive on its
/// own (that's `FileType::Tar`/`FileType::Zip`), just something a plain
/// decompressor can turn back into one file's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CompressionFormat {
    Gzip,
    Bzip2,
    Xz,
    Zstd,
    Lzip,
    Lzma,
    Lzop,
    Compress,
}

impl CompressionFormat {
    fn from_extension(extension: &str) -> Option<Self> {
        Some(match extension {
            "gz" => Self::Gzip,
            "bz2" | "bz" => Self::Bzip2,
            "xz" => Self::Xz,
            "zst" => Self::Zstd,
            "lz" => Self::Lzip,
            "lzma" => Self::Lzma,
            "lzo" => Self::Lzop,
            "Z" => Self::Compress,
            _ => return None,
        })
    }

    /// The external decompressor (program, flag) that writes this format's
    /// decompressed content to stdout.
    pub(super) fn decompressor(self) -> (&'static str, &'static str) {
        match self {
            Self::Gzip => ("gzip", "-dc"),
            Self::Bzip2 => ("bzip2", "-dc"),
            Self::Xz => ("xz", "-dc"),
            Self::Zstd => ("zstd", "-dc"),
            Self::Lzip => ("lzip", "-dc"),
            Self::Lzma => ("lzma", "-dc"),
            Self::Lzop => ("lzop", "-dc"),
            Self::Compress => ("uncompress", "-c"),
        }
    }
}

/// What kind of file a path looks like, decided from its extension alone.
/// This is the one place that knows about every extension `search`
/// recognizes, so each reader downstream just handles its own kind instead
/// of also having to decide whether it applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FileType {
    /// A tar archive, compressed or not - `tar` itself auto-detects
    /// whichever compression (if any) is actually in use, so this carries
    /// no further detail.
    Tar,
    Zip,
    /// A single file compressed on its own, not an archive.
    Compressed(CompressionFormat),
    /// Anything else - read as plain text (or skipped if it looks binary).
    Plain,
}

pub(super) fn classify(path: &Path) -> FileType {
    let Some(extension) = path.extension().and_then(|e| e.to_str()) else {
        return FileType::Plain;
    };

    // Short forms that fold "tar" and the compression into one extension
    // (tgz == tar.gz, tbz2 == tar.bz2, etc.) - `tar` auto-detects the
    // compression either way, so these don't need a `CompressionFormat`.
    const SHORT_TAR_FORMS: &[&str] = &["tgz", "taz", "tbz", "tbz2", "tz2", "tlz", "txz", "tzst"];

    if extension == "tar" || SHORT_TAR_FORMS.contains(&extension) {
        return FileType::Tar;
    }

    if extension.eq_ignore_ascii_case("zip") {
        return FileType::Zip;
    }

    if let Some(format) = CompressionFormat::from_extension(extension) {
        // A compressed tarball (foo.tar.gz) is still a tar archive - `tar`
        // needs to unpack its members, not a plain decompressor that would
        // just hand back the tarball's raw bytes.
        let is_tarball = path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|stem| stem.ends_with(".tar"));

        return if is_tarball {
            FileType::Tar
        } else {
            FileType::Compressed(format)
        };
    }

    FileType::Plain
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn plain_uncompressed_tarball_is_tar() {
        assert_eq!(classify(Path::new("archive.tar")), FileType::Tar);
    }

    #[test]
    fn long_form_compressed_tarballs_are_tar() {
        for ext in ["gz", "bz2", "bz", "xz", "zst", "lz", "lzma", "lzo", "Z"] {
            let path = PathBuf::from(format!("archive.tar.{ext}"));
            assert_eq!(classify(&path), FileType::Tar, "archive.tar.{ext}");
        }
    }

    #[test]
    fn short_form_compressed_tarballs_are_tar() {
        for ext in ["tgz", "taz", "tbz", "tbz2", "tz2", "tlz", "txz", "tzst"] {
            let path = PathBuf::from(format!("archive.{ext}"));
            assert_eq!(classify(&path), FileType::Tar, "archive.{ext}");
        }
    }

    #[test]
    fn zip_extension_is_zip_regardless_of_case() {
        assert_eq!(classify(Path::new("archive.zip")), FileType::Zip);
        assert_eq!(classify(Path::new("archive.ZIP")), FileType::Zip);
    }

    #[test]
    fn standalone_compressed_files_are_compressed() {
        assert_eq!(
            classify(Path::new("access.log.gz")),
            FileType::Compressed(CompressionFormat::Gzip)
        );
        assert_eq!(
            classify(Path::new("notes.txt.bz2")),
            FileType::Compressed(CompressionFormat::Bzip2)
        );
        assert_eq!(
            classify(Path::new("notes.txt.bz")),
            FileType::Compressed(CompressionFormat::Bzip2)
        );
        assert_eq!(
            classify(Path::new("notes.txt.xz")),
            FileType::Compressed(CompressionFormat::Xz)
        );
        assert_eq!(
            classify(Path::new("notes.txt.zst")),
            FileType::Compressed(CompressionFormat::Zstd)
        );
        assert_eq!(
            classify(Path::new("notes.txt.lz")),
            FileType::Compressed(CompressionFormat::Lzip)
        );
        assert_eq!(
            classify(Path::new("notes.txt.lzma")),
            FileType::Compressed(CompressionFormat::Lzma)
        );
        assert_eq!(
            classify(Path::new("notes.txt.lzo")),
            FileType::Compressed(CompressionFormat::Lzop)
        );
        assert_eq!(
            classify(Path::new("notes.txt.Z")),
            FileType::Compressed(CompressionFormat::Compress)
        );
    }

    #[test]
    fn anything_else_is_plain() {
        assert_eq!(classify(Path::new("plain.txt")), FileType::Plain);
        assert_eq!(classify(Path::new("no_extension")), FileType::Plain);
    }
}
