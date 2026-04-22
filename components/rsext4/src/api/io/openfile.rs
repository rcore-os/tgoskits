use bitflags::bitflags;

use crate::{
    api::OpenFile,
    blockdev::{BlockDevice, Jbd2Dev},
    bmalloc::InodeNumber,
    dir::split_paren_child_and_tranlatevalid,
    disknode::Ext4Inode,
    error::{Errno, Ext4Error, Ext4Result},
    ext4::Ext4FileSystem,
    file::{mkfile, read_symlink_target, truncate},
    loopfile::get_file_inode,
    metadata::chmod,
};

/// Linux UAPI `O_ACCMODE` mask (`include/uapi/asm-generic/fcntl.h`).
pub const O_ACCMODE_MASK: u32 = 0o00000003;

/// Linux regular-file creation default mode.
pub const DEFAULT_CREATE_MODE: u16 = 0o666;
/// Linux `SYMLOOP_MAX`-like guard for symlink traversal during open lookup.
const MAX_SYMLINK_FOLLOW: usize = 40;

bitflags! {
    /// ext4 API 接受并执行的 `O_*` 打开标志子集（与 Linux 位值一致）。
    ///
    /// 这些标志都对应“文件系统可观测行为”，不包含 fd 表/进程语义。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct OpenFlags: u32 {
        /// `O_CREAT`: 路径不存在时创建常规文件。
        const CREAT     = 0o00000100;
        /// `O_EXCL`: 与 `O_CREAT` 组合时，目标已存在返回 `EEXIST`。
        const EXCL      = 0o00000200;
        /// `O_TRUNC`: 打开现有常规文件时截断到 0。
        const TRUNC     = 0o00001000;
        /// `O_APPEND`: 后续写入以 EOF 为目标位置。
        const APPEND    = 0o00002000;
        /// `O_DIRECTORY`: 要求目标必须是目录。
        const DIRECTORY = 0o00200000;
        /// `O_NOFOLLOW`: 最后一个路径分量若是符号链接则报错。
        const NOFOLLOW  = 0o00400000;
        /// `O_NOATIME`: 读路径可选择不更新 atime。
        const NOATIME   = 0o01000000;
    }
}

bitflags! {
    /// `open` 创建模式中可接受的 Linux `mode_t` 权限位掩码。
    ///
    /// 来源：`include/uapi/linux/stat.h` (`S_ISUID..S_IXOTH`)。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct OpenMode: u16 {
        /// `S_ISUID`: set-user-ID。
        const S_ISUID = 0o4000;
        /// `S_ISGID`: set-group-ID。
        const S_ISGID = 0o2000;
        /// `S_ISVTX`: sticky。
        const S_ISVTX = 0o1000;

        /// `S_IRWXU`: owner rwx。
        const S_IRWXU = 0o0700;
        /// `S_IRUSR`: owner r。
        const S_IRUSR = 0o0400;
        /// `S_IWUSR`: owner w。
        const S_IWUSR = 0o0200;
        /// `S_IXUSR`: owner x。
        const S_IXUSR = 0o0100;

        /// `S_IRWXG`: group rwx。
        const S_IRWXG = 0o0070;
        /// `S_IRGRP`: group r。
        const S_IRGRP = 0o0040;
        /// `S_IWGRP`: group w。
        const S_IWGRP = 0o0020;
        /// `S_IXGRP`: group x。
        const S_IXGRP = 0o0010;

        /// `S_IRWXO`: other rwx。
        const S_IRWXO = 0o0007;
        /// `S_IROTH`: other r。
        const S_IROTH = 0o0004;
        /// `S_IWOTH`: other w。
        const S_IWOTH = 0o0002;
        /// `S_IXOTH`: other x。
        const S_IXOTH = 0o0001;
    }
}

/// Linux `S_IALLUGO` 掩码（setid/sticky + rwx 权限位）。
pub const S_IALLUGO_MASK: u16 = OpenMode::all().bits();

/// Linux `O_ACCMODE` 低两位访问模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAccessMode {
    /// `O_RDONLY` (`0`)
    ReadOnly  = 0,
    /// `O_WRONLY` (`1`)
    WriteOnly = 1,
    /// `O_RDWR` (`2`)
    ReadWrite = 2,
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

/// ext4 API 层的打开参数。
///
/// 只包含文件系统范围内会影响 inode/data 行为的字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenHow {
    /// `O_ACCMODE` 访问模式。
    pub access: OpenAccessMode,
    /// 文件系统层有效的 `O_*` 标志。
    pub flags: OpenFlags,
    /// 创建时的权限位（仅 `O_CREAT` 有意义）。
    pub mode: u16,
}

/// 校验创建模式是否合法。
fn validate_open_mode_bits(mode: u16) -> Ext4Result<()> {
    if mode & !S_IALLUGO_MASK != 0 || OpenMode::from_bits(mode).is_none() {
        return Err(Ext4Error::from(Errno::EINVAL));
    }
    Ok(())
}

impl OpenHow {
    /// 从 Linux 风格原始 `flags/mode` 构造。
    ///
    /// 只接受 ext4 API 支持的标志位；包含未知位时返回 `EINVAL`。
    pub fn from_raw(raw_flags: u32, mode: u16) -> Ext4Result<Self> {
        let access = OpenAccessMode::try_from(raw_flags)?;
        let flags_only = raw_flags & !O_ACCMODE_MASK;
        let flags =
            OpenFlags::from_bits(flags_only).ok_or_else(|| Ext4Error::from(Errno::EINVAL))?;
        Ok(Self {
            access,
            flags,
            mode,
        })
    }
}

/// 对 ext4 API 范围内可判定的 `open` 规则做前置校验。
fn validate_open_how_ext4(how: OpenHow) -> Ext4Result<()> {
    let will_create = how.flags.contains(OpenFlags::CREAT);
    if will_create {
        validate_open_mode_bits(how.mode)?;
    } else if how.mode != 0 {
        return Err(Ext4Error::from(Errno::EINVAL));
    }

    // Linux 会拒绝 O_DIRECTORY|O_CREAT。
    if how.flags.contains(OpenFlags::DIRECTORY | OpenFlags::CREAT) {
        return Err(Ext4Error::from(Errno::EINVAL));
    }

    Ok(())
}

/// 归一化路径并检查输入有效性。
fn normalize_open_path(path: &str) -> Ext4Result<alloc::string::String> {
    let norm_path = split_paren_child_and_tranlatevalid(path);
    if norm_path.is_empty() {
        return Err(Ext4Error::invalid_input());
    }
    Ok(norm_path)
}

/// 拆分绝对路径为 `(parent, leaf)`。
fn split_parent_leaf(norm_path: &str) -> Ext4Result<(&str, &str)> {
    let idx = norm_path.rfind('/').ok_or_else(Ext4Error::invalid_input)?;
    let leaf = &norm_path[idx + 1..];
    if leaf.is_empty() {
        return Err(Ext4Error::invalid_input());
    }
    let parent = if idx == 0 { "/" } else { &norm_path[..idx] };
    Ok((parent, leaf))
}

/// 计算最后一个路径分量是否允许跟随符号链接。
///
/// 对齐 Linux: `O_CREAT|O_EXCL` 时隐式禁止最终分量 symlink 跟随。
fn should_follow_final_symlink(how: OpenHow) -> bool {
    let nofollow = how.flags.contains(OpenFlags::NOFOLLOW)
        || how.flags.contains(OpenFlags::CREAT | OpenFlags::EXCL);
    !nofollow
}

/// Resolves a symlink target path against the current pathname.
///
/// Absolute targets are rooted from `/`, relative targets are joined with the
/// parent of `current_path`.
fn resolve_symlink_path(current_path: &str, target: &str) -> alloc::string::String {
    if target.starts_with('/') {
        return split_paren_child_and_tranlatevalid(target);
    }
    let parent = match current_path.rfind('/') {
        Some(0) | None => "/",
        Some(pos) => &current_path[..pos],
    };
    let combined = if parent == "/" {
        alloc::format!("/{target}")
    } else {
        alloc::format!("{parent}/{target}")
    };
    split_paren_child_and_tranlatevalid(&combined)
}

fn lookup_inode_for_open_following_final_symlink<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    dev: &mut Jbd2Dev<B>,
    path: &str,
    depth: usize,
) -> Ext4Result<Option<(InodeNumber, Ext4Inode)>> {
    let Some((inode_num, mut inode)) = get_file_inode(fs, dev, path)? else {
        return Ok(None);
    };

    if !inode.is_symlink() {
        return Ok(Some((inode_num, inode)));
    }
    if depth >= MAX_SYMLINK_FOLLOW {
        return Err(Ext4Error::from(Errno::ELOOP));
    }

    // TODO(linux-open-symlink-walk): 当前仅补齐“最终分量 symlink 跟随”。
    // 目标语义：
    // - 全路径逐分量解析时都遵循 Linux namei 规则（含中间分量 symlink）；
    // - 对照 `fs/namei.c` 的路径行走与循环检测策略；
    // - 错误码与边界行为保持一致。
    let target_bytes = read_symlink_target(dev, fs, &mut inode)?;
    let target = core::str::from_utf8(&target_bytes).map_err(|_| Ext4Error::corrupted())?;
    let resolved = resolve_symlink_path(path, target);
    lookup_inode_for_open_following_final_symlink(fs, dev, &resolved, depth + 1)
}

fn lookup_inode_for_open<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    dev: &mut Jbd2Dev<B>,
    path: &str,
    follow_final_symlink: bool,
) -> Ext4Result<Option<(InodeNumber, Ext4Inode)>> {
    if follow_final_symlink {
        return lookup_inode_for_open_following_final_symlink(fs, dev, path, 0);
    }
    get_file_inode(fs, dev, path)
}

/// 构造 `OpenFile` 句柄。
fn make_open_file(
    path: alloc::string::String,
    inode_num: InodeNumber,
    inode: Ext4Inode,
    how: OpenHow,
) -> OpenFile {
    OpenFile {
        inode_num,
        path,
        inode,
        offset: 0,
        access: how.access,
        flags: how.flags,
    }
}

/// 打开已存在目录项。
fn open_existing_entry<B: BlockDevice>(
    dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    norm_path: alloc::string::String,
    inode_num: InodeNumber,
    inode: Ext4Inode,
    how: OpenHow,
) -> Ext4Result<OpenFile> {
    let create_excl = how.flags.contains(OpenFlags::CREAT | OpenFlags::EXCL);
    if create_excl {
        return Err(Ext4Error::from(Errno::EEXIST));
    }

    if inode.is_symlink() && !should_follow_final_symlink(how) {
        return Err(Ext4Error::from(Errno::ELOOP));
    }

    if how.flags.contains(OpenFlags::DIRECTORY) && !inode.is_dir() {
        return Err(Ext4Error::from(Errno::ENOTDIR));
    }

    // Linux may_open 可见行为之一：目录以写/截断方式打开返回 EISDIR。
    if inode.is_dir()
        && (how.access != OpenAccessMode::ReadOnly || how.flags.contains(OpenFlags::TRUNC))
    {
        return Err(Ext4Error::from(Errno::EISDIR));
    }
    // TODO(linux-open-may-open): 对齐 `fs/namei.c::may_open()` 的剩余约束。
    // 目标语义：
    // - inode permission + acc_mode 检查；
    // - append-only inode 上 O_APPEND/O_TRUNC 的 EPERM 分支；
    // - O_NOATIME 仅 owner/capable 允许；
    // - 非普通文件类型上的更多 Linux 错误分支。

    if inode.is_file() && how.flags.contains(OpenFlags::TRUNC) {
        // TODO(linux-open-trunc-perms): `O_TRUNC` 当前未做完整的 Linux 权限门禁。
        // 对照 `build_open_flags()` + `may_open()` + `handle_truncate()` 补齐。
        truncate(dev, fs, &norm_path, 0)?;
        let (inode_num, inode) =
            lookup_inode_for_open(fs, dev, &norm_path, should_follow_final_symlink(how))?
                .ok_or_else(Ext4Error::corrupted)?;
        return Ok(make_open_file(norm_path, inode_num, inode, how));
    }

    Ok(make_open_file(norm_path, inode_num, inode, how))
}

/// 在路径未命中时处理 `O_CREAT` 逻辑。
fn open_create_entry<B: BlockDevice>(
    dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    norm_path: alloc::string::String,
    how: OpenHow,
) -> Ext4Result<OpenFile> {
    if !how.flags.contains(OpenFlags::CREAT) {
        return Err(Ext4Error::not_found());
    }

    // ext4 API 对齐 Linux: O_CREAT 不会创建缺失父目录。
    let (parent, _) = split_parent_leaf(&norm_path)?;
    let Some((_parent_ino, parent_inode)) = lookup_inode_for_open(fs, dev, parent, true)? else {
        // TODO(linux-open-errno-shape): 这里目前会把部分“中间分量非目录”等场景折叠为 ENOENT。
        // 目标语义：按 Linux 路径行走错误形状区分 ENOTDIR/ENOENT/ELOOP 等。
        return Err(Ext4Error::not_found());
    };
    if !parent_inode.is_dir() {
        return Err(Ext4Error::not_dir());
    }

    mkfile(dev, fs, &norm_path, None, None).map_err(|e| {
        if e.code == Errno::EEXIST && how.flags.contains(OpenFlags::EXCL) {
            Ext4Error::from(Errno::EEXIST)
        } else {
            e
        }
    })?;

    // TODO(linux-open-create-mode): 这里是“mkfile 后 chmod”两阶段更新。
    // 目标语义：创建时一次性写入最终 mode（含 umask 后的模式），避免中间态。
    // 该 API 当前仅直接应用调用者给定 mode，不引入 OS 层 umask。
    if how.mode != DEFAULT_CREATE_MODE {
        chmod(dev, fs, &norm_path, how.mode)?;
    }

    let (inode_num, inode) =
        get_file_inode(fs, dev, &norm_path)?.ok_or_else(Ext4Error::corrupted)?;
    Ok(make_open_file(norm_path, inode_num, inode, how))
}

/// ext4 API 层 `open`。
///
/// 语义范围：
/// - 路径归一化与最终对象查找；
/// - `O_CREAT/O_EXCL/O_TRUNC/O_DIRECTORY/O_NOFOLLOW` 的可观测行为；
/// - 创建时 mode 应用。
pub fn open<B: BlockDevice>(
    dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    how: OpenHow,
) -> Ext4Result<OpenFile> {
    validate_open_how_ext4(how)?;
    let norm_path = normalize_open_path(path)?;
    let follow_final_symlink = should_follow_final_symlink(how);

    if let Some((inode_num, inode)) =
        lookup_inode_for_open(fs, dev, &norm_path, follow_final_symlink)?
    {
        return open_existing_entry(dev, fs, norm_path, inode_num, inode, how);
    }

    open_create_entry(dev, fs, norm_path, how)
}
