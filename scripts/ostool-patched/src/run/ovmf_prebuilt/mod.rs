#![warn(missing_docs)]
#![allow(unused)]

mod error;
mod fetch;
mod source_constants;

use fetch::update_cache;
use std::path::{Path, PathBuf};

pub use error::Error;

/// Which prebuilt to download.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Source {
    /// Release tag name, e.g. "edk2-stable202408-r1".
    pub tag: &'static str,

    /// SHA-256 hash of the compressed tarball.
    pub sha256: &'static str,
}

/// UEFI architecture.
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Arch {
    Aarch64,
    Ia32,
    LoongArch64,
    Riscv64,
    X64,
}

impl Arch {
    /// Convert to a string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Aarch64 => "aarch64",
            Self::Ia32 => "ia32",
            Self::LoongArch64 => "loongarch64",
            Self::Riscv64 => "riscv64",
            Self::X64 => "x64",
        }
    }
}

/// Type of file within the prebuilt archive.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[allow(missing_docs)]
pub enum FileType {
    Code,
    Vars,
    Shell,
}

impl FileType {
    /// Convert to a string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code.fd",
            Self::Vars => "vars.fd",
            Self::Shell => "shell.efi",
        }
    }
}

/// Cached prebuilt.
pub struct Prebuilt {
    dir: PathBuf,
}

impl Prebuilt {
    /// Fetch a prebuilt from a local cache. If the cache is out of
    /// date, the prebuilt is downloaded and the cache is updated.
    ///
    /// The SHA-256 hash of the original prebuilt is stored in
    /// `<prebuilt_dir>/sha256`. This is used to determine whether the
    /// cache is up-to-date. Note that if some external process modifies
    /// the cached files but leaves the `sha256` file unmodified, this
    /// code will not detect that the cache is invalid.
    ///
    /// If the cache is updated, the downloaded prebuilt's hash will be
    /// checked against [`source.sha256`]. An error will be
    /// returned if the hash does not match, and the filesystem will not
    /// be modified. This ensures that if you pin this library in
    /// `Cargo.lock`, and use one of the [`Source`] associated
    /// constants, the library will never unpack unverified files. This
    /// provides some protection against a malicious attack modifying
    /// the release tarballs on Github.
    ///
    /// [`source.sha256`]: Source::sha256
    pub fn fetch<P: AsRef<Path>>(source: Source, prebuilt_dir: P) -> Result<Self, Error> {
        let prebuilt_dir = prebuilt_dir.as_ref();

        update_cache(source, prebuilt_dir)?;

        Ok(Self {
            dir: prebuilt_dir.to_owned(),
        })
    }

    /// Get the path of a specific file within the cache.
    pub fn get_file(&self, arch: Arch, file_type: FileType) -> PathBuf {
        self.dir.join(arch.as_str()).join(file_type.as_str())
    }
}
