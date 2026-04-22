use bitflags::bitflags;

use crate::{
    api::OpenFile,
    blockdev::{BlockDevice, Jbd2Dev},
    dir::split_paren_child_and_tranlatevalid,
    error::{Errno, Ext4Error, Ext4Result},
    ext4::Ext4FileSystem,
    file::{mkfile, truncate},
    loopfile::get_file_inode,
};

/// Linux UAPI `O_ACCMODE` mask (see `include/uapi/asm-generic/fcntl.h`).
pub const O_ACCMODE_MASK: u32 = 0o00000003;
/// Linux `S_IALLUGO` mode mask (`setuid/setgid/sticky + rwx bits`).
pub const S_IALLUGO_MASK: u16 = 0o7777;
/// Linux `O_PATH`-allowed companion flags (`O_PATH_FLAGS` in `fs/open.c`).
pub const O_PATH_ALLOWED_MASK: OpenFlags =
    OpenFlags::from_bits_retain(0o13200000);

/// Linux default creation mode for regular files before umask is applied.
///
/// Note: Linux kernel applies `mode & ~umask` in VFS before the filesystem
/// sees inode creation. This crate currently has no process umask context, so
/// this value is only a semantic placeholder for open-like APIs.
pub const DEFAULT_CREATE_MODE: u16 = 0o666;

bitflags! {
    /// Linux-style `O_*` flags excluding the access-mode low bits.
    ///
    /// Values are aligned with Linux 6.6 UAPI (`asm-generic/fcntl.h`) so the
    /// upper layer can pass syscall-like flags without value translation.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct OpenFlags: u32 {
        /// `O_CREAT`: create file if it does not exist.
        const CREAT     = 0o00000100;
        /// `O_EXCL`: with `O_CREAT`, fail if target already exists.
        const EXCL      = 0o00000200;
        /// `O_NOCTTY`: do not assign controlling terminal (mostly tty-specific).
        const NOCTTY    = 0o00000400;
        /// `O_TRUNC`: truncate regular file to zero length on open.
        const TRUNC     = 0o00001000;
        /// `O_APPEND`: all writes append at end-of-file.
        const APPEND    = 0o00002000;
        /// `O_NONBLOCK`: non-blocking open/IO hint.
        const NONBLOCK  = 0o00004000;
        /// `O_DSYNC`: data-integrity writes (metadata may be partially deferred).
        const DSYNC     = 0o00010000;
        /// `O_DIRECT`: minimize page-cache effects; alignment constrained by backend.
        const DIRECT    = 0o00040000;
        /// `O_LARGEFILE`: allow large file opens on 32-bit ABIs.
        const LARGEFILE = 0o00100000;
        /// `O_DIRECTORY`: require opened object to be a directory.
        const DIRECTORY = 0o00200000;
        /// `O_NOFOLLOW`: refuse to follow final symlink component.
        const NOFOLLOW  = 0o00400000;
        /// `O_NOATIME`: suppress atime update when permitted.
        const NOATIME   = 0o01000000;
        /// `O_CLOEXEC`: set close-on-exec flag on resulting descriptor.
        const CLOEXEC   = 0o02000000;
        /// `O_SYNC`: full data+metadata sync (Linux encodes as `__O_SYNC|O_DSYNC`).
        const SYNC      = 0o04010000;
        /// `O_PATH`: path-only descriptor, minimal file operation rights.
        const PATH      = 0o10000000;
        /// `O_TMPFILE`: unnamed inode creation in a directory.
        const TMPFILE   = 0o20200000;
    }
}

bitflags! {
    /// Linux `openat2(2)` `RESOLVE_*` flags (`include/uapi/linux/openat2.h`).
    ///
    /// These are path-resolution constraints enforced by VFS, not ext4-only
    /// semantics. Keep them at API boundary so OS layer can pass through.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ResolveFlags: u64 {
        /// `RESOLVE_NO_XDEV`: block crossing mount/bind mount boundaries.
        const NO_XDEV      = 0x01;
        /// `RESOLVE_NO_MAGICLINKS`: block procfs-style magic links.
        const NO_MAGICLINKS= 0x02;
        /// `RESOLVE_NO_SYMLINKS`: block all symlink traversal during lookup.
        const NO_SYMLINKS  = 0x04;
        /// `RESOLVE_BENEATH`: prevent path escapes above dirfd (`..`, absolute jumps).
        const BENEATH      = 0x08;
        /// `RESOLVE_IN_ROOT`: scope resolution as if dirfd were root.
        const IN_ROOT      = 0x10;
        /// `RESOLVE_CACHED`: require fully cached lookup (otherwise may return `EAGAIN`).
        const CACHED       = 0x20;
    }
}

/// Access mode encoded by Linux low two `O_*` bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAccessMode {
    /// `O_RDONLY` (`0`).
    ReadOnly,
    /// `O_WRONLY` (`1`).
    WriteOnly,
    /// `O_RDWR` (`2`).
    ReadWrite,
}

impl TryFrom<u32> for OpenAccessMode {
    type Error = Ext4Error;

    fn try_from(raw_flags: u32) -> Result<Self, Self::Error> {
        match raw_flags & O_ACCMODE_MASK {
            0 => Ok(Self::ReadOnly),
            1 => Ok(Self::WriteOnly),
            2 => Ok(Self::ReadWrite),
            _ => Err(Ext4Error::from(Errno::EINVAL)),
        }
    }
}

/// Open request fields aligned with Linux `open_how`.
///
/// Keep this in API layer (not on-disk ext4 structures): these fields describe
/// VFS/openat2 contract, while ext4 proper only consumes resolved operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenHow {
    /// Access mode from `O_ACCMODE`.
    pub access: OpenAccessMode,
    /// Non-access `O_*` flags.
    pub flags: OpenFlags,
    /// Raw creation mode bits (only meaningful with `O_CREAT`/`O_TMPFILE`).
    pub mode: u16,
    /// `openat2` resolve constraints.
    pub resolve: ResolveFlags,
}

impl OpenHow {
    /// Builds `OpenHow` from Linux-style raw `flags/mode/resolve`.
    ///
    /// The parser follows openat2-style strictness: unknown flag bits return
    /// `EINVAL` instead of being silently dropped.
    pub fn from_raw(raw_flags: u32, mode: u16, raw_resolve: u64) -> Ext4Result<Self> {
        let access = OpenAccessMode::try_from(raw_flags)?;
        let flags_only = raw_flags & !O_ACCMODE_MASK;
        let flags = OpenFlags::from_bits(flags_only).ok_or_else(|| Ext4Error::from(Errno::EINVAL))?;
        let resolve =
            ResolveFlags::from_bits(raw_resolve).ok_or_else(|| Ext4Error::from(Errno::EINVAL))?;
        Ok(Self {
            access,
            flags,
            mode,
            resolve,
        })
    }
}

/// Splits a normalized absolute path into `(parent, leaf)` parts.
fn split_parent_leaf(norm_path: &str) -> Ext4Result<(&str, &str)> {
    let idx = norm_path.rfind('/').ok_or_else(Ext4Error::invalid_input)?;
    let leaf = &norm_path[idx + 1..];
    if leaf.is_empty() {
        return Err(Ext4Error::invalid_input());
    }
    let parent = if idx == 0 { "/" } else { &norm_path[..idx] };
    Ok((parent, leaf))
}

/// Validates `OpenHow` against Linux `build_open_flags()`-visible rules.
///
/// Source mapping:
/// - `fs/open.c::build_open_flags`
/// - `fs/open.c::build_open_how`
fn validate_open_how_against_linux(how: OpenHow) -> Ext4Result<()> {
    // Linux forbids combining `RESOLVE_BENEATH` and `RESOLVE_IN_ROOT`.
    if how
        .resolve
        .contains(ResolveFlags::BENEATH | ResolveFlags::IN_ROOT)
    {
        return Err(Ext4Error::from(Errno::EINVAL));
    }

    // Mode is only meaningful for create-like opens and must be permission bits.
    let will_create = how.flags.intersects(OpenFlags::CREAT | OpenFlags::TMPFILE);
    if will_create {
        if how.mode & !S_IALLUGO_MASK != 0 {
            return Err(Ext4Error::from(Errno::EINVAL));
        }
    } else if how.mode != 0 {
        return Err(Ext4Error::from(Errno::EINVAL));
    }

    // Linux blocks O_DIRECTORY|O_CREAT early to avoid creating regular files.
    if how.flags.contains(OpenFlags::DIRECTORY | OpenFlags::CREAT) {
        return Err(Ext4Error::from(Errno::EINVAL));
    }

    // O_TMPFILE requires write intent; in Linux it also requires O_DIRECTORY.
    if how.flags.contains(OpenFlags::TMPFILE) && how.access == OpenAccessMode::ReadOnly {
        return Err(Ext4Error::from(Errno::EINVAL));
    }

    // O_PATH only permits a strict flag subset.
    if how.flags.contains(OpenFlags::PATH) {
        let forbidden = how.flags & !O_PATH_ALLOWED_MASK;
        if !forbidden.is_empty() {
            return Err(Ext4Error::from(Errno::EINVAL));
        }
    }

    // Linux returns EAGAIN for RESOLVE_CACHED with create/truncate/tmpfile.
    if how.resolve.contains(ResolveFlags::CACHED)
        && how
            .flags
            .intersects(OpenFlags::TRUNC | OpenFlags::CREAT | OpenFlags::TMPFILE)
    {
        return Err(Ext4Error::from(Errno::EAGAIN));
    }

    Ok(())
}

/// Linux-oriented open entry.
///
/// This API exposes Linux-style open contract at the interface level:
/// - flag/mode semantics are described by [`OpenHow`];
/// - create and exclusivity come from `O_CREAT/O_EXCL`;
/// - parent directories are **not** auto-created.
///
/// Note: only a subset of Linux behavior is implemented today. Unsupported
/// features return `EOPNOTSUPP` explicitly instead of silently degrading.
pub fn open<B: BlockDevice>(
    dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    how: OpenHow,
) -> Ext4Result<OpenFile> {
    validate_open_how_against_linux(how)?;

    // This API currently cannot carry O_PATH descriptor restrictions through
    // later read/write calls (`OpenFile` lacks per-open operation masks).
    if how.flags.contains(OpenFlags::PATH) {
        return Err(Ext4Error::unsupported());
    }
    // openat2 resolve constraints require VFS-level pathname walk controls,
    // which are not implemented in this ext4-only library yet.
    if !how.resolve.is_empty() {
        return Err(Ext4Error::unsupported());
    }
    if how.flags.contains(OpenFlags::TMPFILE) {
        return Err(Ext4Error::unsupported());
    }

    let norm_path = split_paren_child_and_tranlatevalid(path);
    if norm_path.is_empty() {
        return Err(Ext4Error::invalid_input());
    }

    if let Some((inode_num, inode)) = get_file_inode(fs, dev, &norm_path)? {
        if how
            .flags
            .contains(OpenFlags::CREAT | OpenFlags::EXCL)
        {
            return Err(Ext4Error::from(Errno::EEXIST));
        }
        if inode.is_symlink() {
            // Linux with O_NOFOLLOW on final symlink returns ELOOP.
            if how.flags.contains(OpenFlags::NOFOLLOW) {
                return Err(Ext4Error::from(Errno::ELOOP));
            }
            // Following symlinks at open-time is a VFS path-walk behavior.
            return Err(Ext4Error::unsupported());
        }
        if how.flags.contains(OpenFlags::DIRECTORY) && !inode.is_dir() {
            return Err(Ext4Error::from(Errno::ENOTDIR));
        }
        // Linux may_open(): opening directories with write/truncate intent is EISDIR.
        if inode.is_dir()
            && (how.access != OpenAccessMode::ReadOnly || how.flags.contains(OpenFlags::TRUNC))
        {
            return Err(Ext4Error::from(Errno::EISDIR));
        }
        if inode.is_file() && how.flags.contains(OpenFlags::TRUNC) {
            // Linux truncates on open if O_TRUNC is present and checks passed.
            truncate(dev, fs, &norm_path, 0)?;
            let (inode_num, inode) =
                get_file_inode(fs, dev, &norm_path)?.ok_or_else(Ext4Error::corrupted)?;
            return Ok(OpenFile {
                inode_num,
                path: norm_path,
                inode,
                offset: 0,
            });
        }
        return Ok(OpenFile {
            inode_num,
            path: norm_path,
            inode,
            offset: 0,
        });
    }

    if !how.flags.contains(OpenFlags::CREAT) {
        return Err(Ext4Error::not_found());
    }

    // Linux open(O_CREAT) does not create missing parent directories.
    let (parent, _) = split_parent_leaf(&norm_path)?;
    let Some((_parent_ino, parent_inode)) = get_file_inode(fs, dev, parent)? else {
        return Err(Ext4Error::not_found());
    };
    if !parent_inode.is_dir() {
        return Err(Ext4Error::not_dir());
    }

    mkfile(dev, fs, &norm_path, None, None)?;

    let (inode_num, inode) = get_file_inode(fs, dev, &norm_path)?.ok_or_else(Ext4Error::corrupted)?;
    Ok(OpenFile {
        inode_num,
        path: norm_path,
        inode,
        offset: 0,
    })
}
