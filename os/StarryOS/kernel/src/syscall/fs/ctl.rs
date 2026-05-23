use alloc::{ffi::CString, vec, vec::Vec};
use core::{
    ffi::{c_char, c_int},
    mem::offset_of,
    time::Duration,
};

use ax_errno::{AxError, AxResult};
use ax_fs::{FS_CONTEXT, FsContext};
use ax_hal::time::wall_time;
use ax_task::current;
use axfs_ng_vfs::{DeviceId, Location, MetadataUpdate, NodePermission, NodeType, path::Path};
use linux_raw_sys::{
    general::*,
    ioctl::{BLKGETSIZE64, BLKRAGET, BLKSSZGET, FIOASYNC, FIONBIO, TIOCGWINSZ},
};
use starry_vm::{VmPtr, vm_write_slice};

use crate::{
    file::{Directory, FileLike, fd_is_path, get_file_like, resolve_at, with_fs},
    mm::vm_load_string,
    task::AsThread,
    time::TimeValueLike,
};

fn check_dir_search_write_permission(dir: &Location) -> AxResult<()> {
    let cred = current().as_thread().cred();
    let metadata = dir.metadata()?;
    let file_uid = metadata.uid;
    let file_gid = metadata.gid;
    let file_mode = metadata.mode.bits() as u32;

    if cred.fsuid == 0 {
        let any_exec = NodePermission::OWNER_EXEC.bits()
            | NodePermission::GROUP_EXEC.bits()
            | NodePermission::OTHER_EXEC.bits();
        if file_mode as u16 & any_exec == 0 {
            return Err(AxError::PermissionDenied);
        }
        return Ok(());
    }

    let effective_bits = if cred.fsuid == file_uid {
        (file_mode >> 6) & 0o7
    } else if cred.fsgid == file_gid || cred.groups.contains(&file_gid) {
        (file_mode >> 3) & 0o7
    } else {
        file_mode & 0o7
    };

    if effective_bits & 0o3 != 0o3 {
        return Err(AxError::PermissionDenied);
    }

    Ok(())
}

fn check_sticky_rename_permission(dir: &Location, victim: &Location) -> AxResult<()> {
    let cred = current().as_thread().cred();
    let dir_meta = dir.metadata()?;
    if !dir_meta.mode.contains(NodePermission::STICKY) {
        return Ok(());
    }
    if cred.has_cap_fowner() || cred.fsuid == dir_meta.uid {
        return Ok(());
    }

    let victim_meta = victim.metadata()?;
    if cred.fsuid == victim_meta.uid {
        return Ok(());
    }

    Err(AxError::OperationNotPermitted)
}

/// The ioctl() system call manipulates the underlying device parameters
/// of special files.
pub fn sys_ioctl(fd: i32, cmd: u32, arg: usize) -> AxResult<isize> {
    debug!("sys_ioctl <= fd: {fd}, cmd: {cmd}, arg: {arg}");
    let f = get_file_like(fd)?;
    if cmd == FIONBIO {
        let val: i32 = (arg as *const i32).vm_read()?;
        f.set_nonblocking(val != 0)?;
        return Ok(0);
    }
    if cmd == FIOASYNC {
        let val: i32 = (arg as *const i32).vm_read()?;
        f.set_async_mode(val != 0)?;
        return Ok(0);
    }
    f.ioctl(cmd, arg)
        .map(|result| result as isize)
        .inspect_err(|err| {
            if *err == AxError::NotATty {
                // Applications commonly probe non-terminal/blobk fds with
                // these ioctls; suppress noise.
                if matches!(cmd, TIOCGWINSZ | BLKGETSIZE64 | BLKRAGET | BLKSSZGET) {
                    return;
                }
                warn!("Unsupported ioctl command: {cmd} for fd: {fd}");
            }
        })
}

#[ddebug::named]
pub fn sys_chdir(path: *const c_char) -> AxResult<isize> {
    let path = vm_load_string(path)?;
    debug_fn!("sys_chdir <= path: {path}");

    let mut fs = FS_CONTEXT.lock();
    let entry = fs.resolve(path)?;
    if entry.node_type() != NodeType::Directory {
        return Err(AxError::NotADirectory);
    }
    check_dir_search_permission(&entry)?;
    fs.set_current_dir(entry)?;
    Ok(0)
}

fn check_dir_search_permission(dir: &Location) -> AxResult<()> {
    let meta = dir.metadata()?;
    let cred = current().as_thread().cred();

    if cred.fsuid == 0 {
        let any_exec =
            NodePermission::OWNER_EXEC | NodePermission::GROUP_EXEC | NodePermission::OTHER_EXEC;
        return if meta.mode.intersects(any_exec) {
            Ok(())
        } else {
            Err(AxError::PermissionDenied)
        };
    }

    let has_search = if cred.fsuid == meta.uid {
        meta.mode.contains(NodePermission::OWNER_EXEC)
    } else if cred.in_group(meta.gid) {
        meta.mode.contains(NodePermission::GROUP_EXEC)
    } else {
        meta.mode.contains(NodePermission::OTHER_EXEC)
    };

    if has_search {
        Ok(())
    } else {
        Err(AxError::PermissionDenied)
    }
}

pub fn sys_fchdir(dirfd: i32) -> AxResult<isize> {
    debug!("sys_fchdir <= dirfd: {dirfd}");

    let entry = Directory::from_fd(dirfd)?.inner().clone();
    check_dir_search_permission(&entry)?;
    FS_CONTEXT.lock().set_current_dir(entry)?;
    Ok(0)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_mkdir(path: *const c_char, mode: u32) -> AxResult<isize> {
    sys_mkdirat(AT_FDCWD, path, mode)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_mknod(path: *const c_char, mode: u32, dev: u64) -> AxResult<isize> {
    sys_mknodat(AT_FDCWD, path, mode, dev)
}

pub fn sys_chroot(path: *const c_char) -> AxResult<isize> {
    let path = vm_load_string(path)?;
    debug!("sys_chroot <= path: {path}");

    let cred = current().as_thread().cred();
    let mut fs = FS_CONTEXT.lock();
    match fs.resolve_parent(Path::new(&path)) {
        Ok((parent, _)) => check_dir_search_permission(&parent)?,
        // axfs-ng reports "/" (and only the namespace root) as InvalidInput
        // because it has no parent; Linux still allows chroot("/") and should
        // continue with target resolution / privilege checks instead of EINVAL.
        Err(AxError::InvalidInput) => {}
        Err(AxError::NotFound) => {}
        Err(err) => return Err(err),
    }
    let loc = fs.resolve(path)?;
    if loc.node_type() != NodeType::Directory {
        return Err(AxError::NotADirectory);
    }
    check_dir_search_permission(&loc)?;
    if !cred.has_cap_sys_chroot() {
        return Err(AxError::OperationNotPermitted);
    }
    *fs = FsContext::new(loc);
    Ok(0)
}

ktracepoint::define_event_trace!(
    sys_mkdirat,
    TP_kops(crate::tracepoint::KernelTraceAux),
    TP_system(syscalls),
    TP_PROTO(path:&str, mode: u16),
    TP_STRUCT__entry {
        mode: u16,
        path: [u8;64],
    },
    TP_fast_assign {
        mode: mode,
        path: {
            let mut buf = [0u8; 64];
            let bytes = path.as_bytes();
            let mut len = bytes.len().min(63);
            while !path.is_char_boundary(len) {
                len -= 1;
            }
            buf[..len].copy_from_slice(&bytes[..len]);
            buf[len] = 0; // null-terminate
            buf
        },
    },
    TP_ident(__entry),
    TP_printk({
        let nul = __entry.path.iter().position(|&b| b == 0).unwrap_or(__entry.path.len());
        let path = core::str::from_utf8(&__entry.path[..nul]).unwrap_or("invalid utf8");
        let mode = __entry.mode;
        let mode = NodePermission::from_bits_truncate(mode);
        alloc::format!("mkdir at {path} with mode {mode:?}")
    })
);

pub fn sys_mkdirat(dirfd: i32, path: *const c_char, mode: u32) -> AxResult<isize> {
    let path = vm_load_string(path)?;
    debug!("sys_mkdirat <= dirfd: {dirfd}, path: {path}, mode: {mode}");

    let mode = mode & !current().as_thread().proc_data.umask();
    let mode = NodePermission::from_bits_truncate(mode as u16);

    // call tp:trace_sys_mkdirat
    trace_sys_mkdirat(&path, mode.bits());
    with_fs(dirfd, |fs| {
        let cred = current().as_thread().cred();
        let path = Path::new(&path);
        if let Ok((dir, _)) = fs.resolve_nonexistent(path) {
            check_dir_search_write_permission(&dir)?;
        }
        let loc = fs.create_dir(path, mode)?;
        loc.update_metadata(MetadataUpdate {
            owner: Some((cred.fsuid, cred.fsgid)),
            ..Default::default()
        })?;
        Ok(0)
    })
}

pub fn sys_mknodat(dirfd: i32, path: *const c_char, mode: u32, dev: u64) -> Result<isize, AxError> {
    let path = vm_load_string(path)?;
    debug!(
        "sys_mknodat <= dirfd: {}, path: {:?}, mode: {}, dev: {}",
        dirfd, path, mode, dev
    );

    // Split type and permission bits
    let ftype = mode & S_IFMT;
    let mut perm = mode & !S_IFMT;
    // apply umask like mkdir
    perm &= !current().as_thread().proc_data.umask();

    // Linux mknod semantics: S_IFDIR → EPERM, unknown type bits → EINVAL.
    let node_type = match ftype {
        0 | S_IFREG => NodeType::RegularFile,
        S_IFCHR => NodeType::CharacterDevice,
        S_IFBLK => NodeType::BlockDevice,
        S_IFIFO => NodeType::Fifo,
        S_IFSOCK => NodeType::Socket,
        S_IFDIR => return Err(AxError::OperationNotPermitted),
        _ => return Err(AxError::InvalidInput),
    };

    let res = with_fs(dirfd, |fs| {
        let (dir, name) = fs.resolve_nonexistent(Path::new(&path))?;
        let loc = dir.create(
            name,
            node_type,
            NodePermission::from_bits_truncate(perm as u16),
        )?;

        // If device node, set rdev via update_metadata
        if matches!(node_type, NodeType::CharacterDevice | NodeType::BlockDevice) {
            loc.update_metadata(MetadataUpdate {
                rdev: Some(DeviceId(dev)),
                ..Default::default()
            })?;
        }

        Ok(0)
    })?;
    Ok(res)
}

// Directory buffer for getdents64 syscall
struct DirBuffer {
    buf: Vec<u8>,
    offset: usize,
}

impl DirBuffer {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0; len],
            offset: 0,
        }
    }

    fn remaining_space(&self) -> usize {
        self.buf.len().saturating_sub(self.offset)
    }

    fn write_entry(&mut self, d_ino: u64, d_off: i64, d_type: NodeType, name: &[u8]) -> bool {
        const NAME_OFFSET: usize = offset_of!(linux_dirent64, d_name);

        let len = NAME_OFFSET + name.len() + 1;
        // alignment
        let len = len.next_multiple_of(align_of::<linux_dirent64>());
        if self.remaining_space() < len {
            return false;
        }

        // FIXME: safety
        unsafe {
            let entry_ptr = self.buf.as_mut_ptr().add(self.offset);
            entry_ptr.cast::<linux_dirent64>().write(linux_dirent64 {
                d_ino,
                d_off,
                d_reclen: len as _,
                d_type: d_type as _,
                d_name: Default::default(),
            });

            let name_ptr = entry_ptr.add(NAME_OFFSET);
            name_ptr.copy_from_nonoverlapping(name.as_ptr(), name.len());
            name_ptr.add(name.len()).write(0);
        }

        self.offset += len;
        true
    }
}

pub fn sys_getdents64(fd: i32, buf: *mut u8, len: usize) -> AxResult<isize> {
    debug!("sys_getdents64 <= fd: {fd}, buf: {buf:?}, len: {len}");

    let mut buffer = DirBuffer::new(len);

    let dir = Directory::from_fd(fd)?;
    let mut dir_offset = dir.offset.lock();

    let mut has_remaining = false;

    dir.inner()
        .read_dir(*dir_offset, &mut |name: &str, ino, node_type, offset| {
            has_remaining = true;
            if !buffer.write_entry(ino, offset as _, node_type, name.as_bytes()) {
                return false;
            }
            *dir_offset = offset;
            true
        })?;

    if has_remaining && buffer.offset == 0 {
        return Err(AxError::InvalidInput);
    }

    vm_write_slice(buf, &buffer.buf)?;

    Ok(buffer.offset as _)
}

/// create a link from new_path to old_path
/// old_path: old file path
/// new_path: new file path
/// flags: link flags
/// return value: return 0 when success, else return -1.
pub fn sys_linkat(
    old_dirfd: c_int,
    old_path: *const c_char,
    new_dirfd: c_int,
    new_path: *const c_char,
    flags: u32,
) -> AxResult<isize> {
    const LINKAT_VALID_FLAGS: u32 = AT_SYMLINK_FOLLOW | AT_EMPTY_PATH;
    if flags & !LINKAT_VALID_FLAGS != 0 {
        return Err(AxError::InvalidInput);
    }

    let old_path = old_path.nullable().map(vm_load_string).transpose()?;
    let new_path = vm_load_string(new_path)?;
    debug!(
        "sys_linkat <= old_dirfd: {old_dirfd}, old_path: {old_path:?}, new_dirfd: {new_dirfd}, \
         new_path: {new_path}, flags: {flags}"
    );

    // Unlike most *at syscalls, linkat() does not follow old_path when flags
    // is 0. It follows the final symlink only with AT_SYMLINK_FOLLOW.
    let resolve_flags = if flags & AT_SYMLINK_FOLLOW != 0 {
        flags & AT_EMPTY_PATH
    } else {
        (flags & AT_EMPTY_PATH) | AT_SYMLINK_NOFOLLOW
    };

    if flags & AT_EMPTY_PATH != 0 && old_path.as_deref() == Some("") {
        return Err(AxError::NotFound);
    }

    let old = resolve_at(old_dirfd, old_path.as_deref(), resolve_flags)?
        .into_file()
        .ok_or(AxError::BadFileDescriptor)?;
    if old.is_dir() {
        return Err(AxError::OperationNotPermitted);
    }
    let new_dirfd = if new_path.starts_with('/') {
        AT_FDCWD
    } else {
        new_dirfd
    };
    let (new_dir, new_name) =
        with_fs(new_dirfd, |fs| fs.resolve_nonexistent(Path::new(&new_path)))?;

    new_dir.link(new_name, &old)?;
    Ok(0)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_link(old_path: *const c_char, new_path: *const c_char) -> AxResult<isize> {
    sys_linkat(AT_FDCWD, old_path, AT_FDCWD, new_path, 0)
}

/// remove link of specific file (can be used to delete file)
/// dir_fd: the directory of link to be removed
/// path: the name of link to be removed
/// flags: can be 0 or AT_REMOVEDIR
/// return 0 when success, else return -1
pub fn sys_unlinkat(dirfd: i32, path: *const c_char, flags: usize) -> AxResult<isize> {
    let path = vm_load_string(path)?;

    debug!("sys_unlinkat <= dirfd: {dirfd}, path: {path:?}, flags: {flags}");

    // Linux kernel (fs/namei.c) rejects any flag bit other than AT_REMOVEDIR
    // with EINVAL. Silently ignoring unknown bits would mask caller bugs and
    // diverge from POSIX semantics (see man 2 unlinkat).
    if flags & !(AT_REMOVEDIR as usize) != 0 {
        return Err(AxError::InvalidInput);
    }

    with_fs(dirfd, |fs| {
        if flags & AT_REMOVEDIR as usize != 0 {
            fs.remove_dir(path)?;
        } else {
            fs.remove_file(path)?;
        }
        Ok(0)
    })
}

#[cfg(target_arch = "x86_64")]
pub fn sys_rmdir(path: *const c_char) -> AxResult<isize> {
    sys_unlinkat(AT_FDCWD, path, AT_REMOVEDIR as _)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_unlink(path: *const c_char) -> AxResult<isize> {
    sys_unlinkat(AT_FDCWD, path, 0)
}

pub fn sys_getcwd(buf: *mut u8, size: isize) -> AxResult<isize> {
    let size: usize = size.try_into().map_err(|_| AxError::BadAddress)?;

    let cwd = FS_CONTEXT.lock().current_dir().absolute_path()?;
    debug!("sys_getcwd => cwd: {cwd}");

    let cwd = CString::new(cwd.as_str()).map_err(|_| AxError::InvalidInput)?;
    let cwd = cwd.as_bytes_with_nul();

    if cwd.len() <= size {
        vm_write_slice(buf, cwd)?;
        Ok(cwd.len() as _)
    } else {
        Err(AxError::OutOfRange)
    }
}

#[cfg(target_arch = "x86_64")]
pub fn sys_symlink(target: *const c_char, linkpath: *const c_char) -> AxResult<isize> {
    sys_symlinkat(target, AT_FDCWD, linkpath)
}

pub fn sys_symlinkat(
    target: *const c_char,
    new_dirfd: i32,
    linkpath: *const c_char,
) -> AxResult<isize> {
    let target = vm_load_string(target)?;
    let linkpath = vm_load_string(linkpath)?;
    debug!("sys_symlinkat <= target: {target:?}, new_dirfd: {new_dirfd}, linkpath: {linkpath:?}");

    with_fs(new_dirfd, |fs| {
        let path = Path::new(&linkpath);
        if let Ok((dir, _)) = fs.resolve_nonexistent(path) {
            check_dir_search_write_permission(&dir)?;
        }
        fs.symlink(target, linkpath)?;
        Ok(0)
    })
}

#[cfg(target_arch = "x86_64")]
pub fn sys_readlink(path: *const c_char, buf: *mut u8, size: usize) -> AxResult<isize> {
    sys_readlinkat(AT_FDCWD, path, buf, size)
}

pub fn sys_readlinkat(
    dirfd: i32,
    path: *const c_char,
    buf: *mut u8,
    size: usize,
) -> AxResult<isize> {
    if size == 0 {
        return Err(AxError::InvalidInput);
    }

    let path = vm_load_string(path)?;

    debug!("sys_readlinkat <= dirfd: {dirfd}, path: {path:?}");

    with_fs(dirfd, |fs| {
        let path_obj = Path::new(&path);
        if let Ok((dir, _)) = fs.resolve_nonexistent(path_obj) {
            check_dir_search_permission(&dir)?;
        }
        let entry = fs.resolve_no_follow(path)?;
        let link = entry.read_link()?;
        let read = size.min(link.len());
        vm_write_slice(buf, &link.as_bytes()[..read])?;
        Ok(read as isize)
    })
}

#[cfg(target_arch = "x86_64")]
pub fn sys_chown(path: *const c_char, uid: i32, gid: i32) -> AxResult<isize> {
    sys_fchownat(AT_FDCWD, path, uid, gid, 0)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_lchown(path: *const c_char, uid: i32, gid: i32) -> AxResult<isize> {
    use linux_raw_sys::general::AT_SYMLINK_NOFOLLOW;
    sys_fchownat(AT_FDCWD, path, uid, gid, AT_SYMLINK_NOFOLLOW)
}

pub fn sys_fchown(fd: i32, uid: i32, gid: i32) -> AxResult<isize> {
    sys_fchownat(fd, core::ptr::null(), uid, gid, AT_EMPTY_PATH)
}

pub fn sys_fchownat(
    dirfd: i32,
    path: *const c_char,
    uid: i32,
    gid: i32,
    flags: u32,
) -> AxResult<isize> {
    const FCHOWNAT_VALID_FLAGS: u32 = AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW;
    if flags & !FCHOWNAT_VALID_FLAGS != 0 {
        return Err(AxError::InvalidInput);
    }

    let path = path.nullable().map(vm_load_string).transpose()?;
    let loc = resolve_at(dirfd, path.as_deref(), flags)?
        .into_file()
        .ok_or(AxError::BadFileDescriptor)?;
    let meta = loc.metadata()?;

    let cred = current().as_thread().cred();

    // Permission checks following Linux semantics:
    // - Changing the file owner (uid) requires CAP_CHOWN.
    // - Changing the file group (gid) without CAP_CHOWN is allowed only if
    //   the caller owns the file and the target group is one the caller
    //   belongs to.
    let changing_owner = uid != -1 && uid as u32 != meta.uid;
    let changing_group = gid != -1 && gid as u32 != meta.gid;

    if changing_owner && !cred.has_cap_chown() {
        return Err(AxError::OperationNotPermitted);
    }

    if changing_group && !cred.has_cap_chown() {
        // Non-root: must own the file and target group must be in our groups.
        if cred.fsuid != meta.uid {
            return Err(AxError::OperationNotPermitted);
        }
        if !cred.in_group(gid as u32) {
            return Err(AxError::OperationNotPermitted);
        }
    }

    let mut mode = meta.mode;
    // Linux chown_common() semantics for clearing setuid/setgid on
    // non-directory files:
    //   - ATTR_KILL_SUID is set unconditionally for all non-dir chown,
    //     regardless of whether uid/gid participates (i.e. even chown
    //     with -1/-1 clears SUID).
    //   - After SUID clearing adds ATTR_MODE to ia_valid, notify_change()
    //     calls should_remove_sgid() which strips SGID on non-directory
    //     files only when GROUP_EXEC (S_IXGRP) is set.
    // Directories preserve SETGID (used for new-file group inheritance).
    let is_dir = meta.node_type == NodeType::Directory;

    if !is_dir {
        mode.remove(NodePermission::SET_UID);
        if mode.contains(NodePermission::GROUP_EXEC) {
            mode.remove(NodePermission::SET_GID);
        }
    }

    let uid = if uid == -1 { meta.uid } else { uid as _ };
    let gid = if gid == -1 { meta.gid } else { gid as _ };
    loc.update_metadata(MetadataUpdate {
        owner: Some((uid, gid)),
        mode: Some(mode),
        ..Default::default()
    })?;
    Ok(0)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_chmod(path: *const c_char, mode: u32) -> AxResult<isize> {
    sys_fchmodat(AT_FDCWD, path, mode, 0)
}

pub fn sys_fchmod(fd: i32, mode: u32) -> AxResult<isize> {
    sys_fchmodat(fd, core::ptr::null(), mode, AT_EMPTY_PATH)
}

pub fn sys_fchmodat(dirfd: i32, path: *const c_char, mode: u32, flags: u32) -> AxResult<isize> {
    const FCHMODAT_VALID_FLAGS: u32 = AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW;
    if flags & !FCHMODAT_VALID_FLAGS != 0 {
        return Err(AxError::InvalidInput);
    }

    let path = path.nullable().map(vm_load_string).transpose()?;

    // man 2 open §"O_PATH": "other file operations (e.g., read(2), write(2),
    // fchmod(2), fchown(2), fgetxattr(2), ioctl(2), mmap(2)) fail with the
    // error EBADF." Fixes bug-open-path-fchmod-bypass.
    //
    // Three paths reach fchmod on a PATH fd; all three must be rejected to
    // match Linux:
    //   (1) Direct: SYS_fchmod(fd) — implemented as fchmodat(fd, NULL,
    //       mode, AT_EMPTY_PATH).
    //   (2) musl libc fallback: when (1) returns EBADF, musl re-tries
    //       fchmodat(AT_FDCWD, "/proc/self/fd/<n>", mode, 0). Linux's procfs
    //       propagates the PATH-handle restriction through the symlink.
    //   (3) (theoretical) Direct user use of /proc/self/fd/<n>.
    let path_is_empty = path.as_deref().is_none_or(|s| s.is_empty());
    if path_is_empty && flags & AT_EMPTY_PATH != 0 && fd_is_path(dirfd) {
        return Err(AxError::BadFileDescriptor); // (1)
    }
    if let Some(p) = path.as_deref()
        && let Some(rest) = p.strip_prefix("/proc/self/fd/")
        && let Ok(n) = rest.parse::<i32>()
        && fd_is_path(n)
    {
        return Err(AxError::BadFileDescriptor); // (2) and (3)
    }

    let loc = resolve_at(dirfd, path.as_deref(), flags)?
        .into_file()
        .ok_or(AxError::BadFileDescriptor)?;

    // Only the file owner or a process with CAP_FOWNER may change mode bits.
    let cred = current().as_thread().cred();
    if !cred.has_cap_fowner() {
        let meta = loc.metadata()?;
        if cred.fsuid != meta.uid {
            return Err(AxError::OperationNotPermitted);
        }
    }

    loc.update_metadata(MetadataUpdate {
        mode: Some(NodePermission::from_bits_truncate(mode as u16)),
        ..Default::default()
    })?;
    Ok(0)
}

#[cfg(target_arch = "x86_64")]
fn update_times(
    dirfd: i32,
    path: *const c_char,
    atime: Option<Duration>,
    mtime: Option<Duration>,
    flags: u32,
) -> AxResult<()> {
    let path = path.nullable().map(vm_load_string).transpose()?;
    resolve_at(dirfd, path.as_deref(), flags)?
        .into_file()
        .ok_or(AxError::BadFileDescriptor)?
        .update_metadata(MetadataUpdate {
            atime,
            mtime,
            ..Default::default()
        })?;
    Ok(())
}

#[cfg(target_arch = "x86_64")]
#[allow(non_camel_case_types)]
#[repr(C)]
#[derive(Clone, Copy, bytemuck::AnyBitPattern)]
pub struct utimbuf {
    actime: linux_raw_sys::general::__kernel_old_time_t,
    modtime: linux_raw_sys::general::__kernel_old_time_t,
}

#[cfg(target_arch = "x86_64")]
pub fn sys_utime(path: *const c_char, times: *const utimbuf) -> AxResult<isize> {
    let (atime, mtime) = if let Some(times) = times.nullable() {
        // SAFETY: `utimbuf` is #[repr(C)] with only integer fields;
        // any bit pattern is a valid value.
        let times = unsafe { times.vm_read_uninit()?.assume_init() };
        (
            Duration::from_secs(times.actime as _),
            Duration::from_secs(times.modtime as _),
        )
    } else {
        let time = wall_time();
        (time, time)
    };
    update_times(AT_FDCWD, path, Some(atime), Some(mtime), 0)?;
    Ok(0)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_utimes(
    path: *const c_char,
    times: *const [linux_raw_sys::general::timeval; 2],
) -> AxResult<isize> {
    let (atime, mtime) = if let Some(times) = times.nullable() {
        // SAFETY: `timeval` is #[repr(C)] with only integer fields;
        // any bit pattern is a valid value.
        let [atime, mtime] = unsafe { times.vm_read_uninit()?.assume_init() };
        (atime.try_into_time_value()?, mtime.try_into_time_value()?)
    } else {
        let time = wall_time();
        (time, time)
    };
    update_times(AT_FDCWD, path, Some(atime), Some(mtime), 0)?;
    Ok(0)
}

pub fn sys_utimensat(
    dirfd: i32,
    path: *const c_char,
    times: *const [timespec; 2],
    mut flags: u32,
) -> AxResult<isize> {
    const UTIMENSAT_VALID_FLAGS: u32 = AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW;
    if flags & !UTIMENSAT_VALID_FLAGS != 0 {
        return Err(AxError::InvalidInput);
    }
    if path.is_null() {
        flags |= AT_EMPTY_PATH;
    }
    fn utime_to_duration(time: &timespec) -> Option<AxResult<Duration>> {
        match time.tv_nsec {
            val if val == UTIME_OMIT as _ => None,
            val if val == UTIME_NOW as _ => Some(Ok(wall_time())),
            _ => Some(time.try_into_time_value()),
        }
    }

    let (atime, mtime) = if let Some(times) = times.nullable() {
        // SAFETY: `timespec` is #[repr(C)] with only integer fields;
        // any bit pattern is a valid value.
        let [atime, mtime] = unsafe { times.vm_read_uninit()?.assume_init() };
        (
            utime_to_duration(&atime).transpose()?,
            utime_to_duration(&mtime).transpose()?,
        )
    } else {
        let time = wall_time();
        (Some(time), Some(time))
    };
    if atime.is_none() && mtime.is_none() {
        return Ok(0);
    }

    // Resolve file and check permissions.
    let path = path.nullable().map(vm_load_string).transpose()?;
    let loc = resolve_at(dirfd, path.as_deref(), flags)?
        .into_file()
        .ok_or(AxError::BadFileDescriptor)?;

    let cred = current().as_thread().cred();
    if !cred.has_cap_fowner() {
        let meta = loc.metadata()?;
        if cred.fsuid != meta.uid {
            return Err(AxError::OperationNotPermitted);
        }
    }

    loc.update_metadata(MetadataUpdate {
        atime,
        mtime,
        ..Default::default()
    })?;
    Ok(0)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_rename(old_path: *const c_char, new_path: *const c_char) -> AxResult<isize> {
    sys_renameat(AT_FDCWD, old_path, AT_FDCWD, new_path)
}

#[cfg(not(target_arch = "riscv64"))]
pub fn sys_renameat(
    old_dirfd: i32,
    old_path: *const c_char,
    new_dirfd: i32,
    new_path: *const c_char,
) -> AxResult<isize> {
    sys_renameat2(old_dirfd, old_path, new_dirfd, new_path, 0)
}

pub fn sys_renameat2(
    old_dirfd: i32,
    old_path: *const c_char,
    new_dirfd: i32,
    new_path: *const c_char,
    flags: u32,
) -> AxResult<isize> {
    const RENAMEAT2_SUPPORTED_FLAGS: u32 = RENAME_NOREPLACE;
    if flags & !RENAMEAT2_SUPPORTED_FLAGS != 0 {
        return Err(AxError::InvalidInput);
    }

    let old_path = vm_load_string(old_path)?;
    let new_path = vm_load_string(new_path)?;
    debug!(
        "sys_renameat2 <= old_dirfd: {old_dirfd}, old_path: {old_path:?}, new_dirfd: {new_dirfd}, \
         new_path: {new_path}, flags: {flags}"
    );

    let (old_dir, old_name) = with_fs(old_dirfd, |fs| fs.resolve_parent(Path::new(&old_path)))?;
    let (new_dir, new_name) = with_fs(new_dirfd, |fs| fs.resolve_parent(Path::new(&new_path)))?;

    check_dir_search_write_permission(&old_dir)?;
    check_dir_search_write_permission(&new_dir)?;

    let old = old_dir.lookup_no_follow(&old_name)?;
    check_sticky_rename_permission(&old_dir, &old)?;

    let new = match new_dir.lookup_no_follow(&new_name) {
        Ok(loc) => Some(loc),
        Err(AxError::NotFound) => None,
        Err(err) => return Err(err),
    };

    if flags & RENAME_NOREPLACE != 0 {
        if new.is_some() {
            return Err(AxError::AlreadyExists);
        }
    } else if let Some(ref dst) = new {
        check_sticky_rename_permission(&new_dir, dst)?;
    }

    old_dir.rename(&old_name, &new_dir, &new_name)?;
    Ok(0)
}

pub fn sys_sync() -> AxResult<isize> {
    debug!("sys_sync");
    // Only syncs root filesystem; does not iterate all mount points like Linux sync(2).
    // ext4 NodeOps::sync is a no-op (Ok(())); FAT NodeOps::sync calls file.flush()
    // to write dirty data to disk.
    FS_CONTEXT.lock().root_dir().sync(false)?;
    Ok(0)
}

pub fn sys_syncfs(fd: c_int) -> AxResult<isize> {
    debug!("sys_syncfs <= fd: {fd}");
    // TODO: File::from_fd only accepts regular file fds; Linux syncfs(2) accepts any fd type.
    let f = crate::file::File::from_fd(fd)?;
    f.inner().location().filesystem().flush()?;
    Ok(0)
}
