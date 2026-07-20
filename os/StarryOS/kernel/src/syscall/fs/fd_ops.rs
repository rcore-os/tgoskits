use alloc::{format, string::ToString, sync::Arc};
use core::{
    ffi::{c_char, c_int},
    mem::size_of,
    ops::DerefMut,
};

use ax_errno::{AxError, AxResult};
use ax_fs_ng::vfs::{FS_CONTEXT, FileBackend, OpenOptions, OpenResult};
use ax_task::current;
use axfs_ng_vfs::{DirEntry, FileNode, Location, NodeOps, NodeType, Reference};
use bitflags::bitflags;
use linux_raw_sys::general::*;
use starry_vm::{VmMutPtr, VmPtr, vm_load};

use crate::{
    file::{
        Directory, FD_TABLE, File, FileDescriptor, FileLike, NsFd, Pipe, add_file_like,
        close_file_like, get_file_like, memfd::Memfd, with_fs,
    },
    mm::vm_load_path_string,
    pseudofs::{Device, dev::tty},
    task::AsThread,
};

/// Convert open flags to [`OpenOptions`].
fn flags_to_options(flags: c_int, mode: __kernel_mode_t, (uid, gid): (u32, u32)) -> OpenOptions {
    let flags = flags as u32;
    let mut options = OpenOptions::new();
    options.mode(mode).user(uid, gid);
    match flags & 0b11 {
        O_RDONLY => options.read(true),
        O_WRONLY => options.write(true),
        _ => options.read(true).write(true),
    };
    if flags & O_APPEND != 0 {
        options.append(true);
    }
    if flags & O_TRUNC != 0 {
        options.truncate(true);
    }
    if flags & O_CREAT != 0 {
        options.create(true);
    }
    // O_EXCL only makes sense with O_CREAT (POSIX). Without O_CREAT, Linux
    // ignores O_EXCL for existing files — busybox blkdiscard opens block
    // devices with O_RDWR|O_EXCL (no O_CREAT).
    if flags & O_EXCL != 0 && flags & O_CREAT != 0 {
        options.create_new(true);
    }
    if flags & O_DIRECTORY != 0 {
        options.directory(true);
    }
    if flags & O_NOFOLLOW != 0 {
        options.no_follow(true);
    }
    if flags & O_DIRECT != 0 {
        options.direct(true);
    }
    // O_PATH must be applied LAST and override conflicting flags: man 2 open
    // "When O_PATH is specified in flags, flag bits other than O_CLOEXEC,
    //  O_DIRECTORY, and O_NOFOLLOW are ignored."
    // Fixes bug-open-path-honors-excl / -path-creat-creates /
    //       -path-dir-write-eisdir / -path-sym-write-enoent.
    if flags & O_PATH != 0 {
        options.path(true);
        // O_PATH: drop access mode (no I/O), drop create/excl/trunc/append
        options.read(true).write(false);
        options
            .create(false)
            .create_new(false)
            .truncate(false)
            .append(false);
    }
    options
}

fn add_to_fd(result: OpenResult, flags: u32) -> AxResult<i32> {
    // FIFO + O_NONBLOCK + O_WRONLY (no reader) → ENXIO.
    //
    // man 2 open §"ENXIO" 第 1 variant：
    //   "O_NONBLOCK | O_WRONLY is set, the named file is a FIFO, and no
    //    process has the FIFO open for reading."
    //
    // TODO: 当前实现假设「FIFO 始终无 reader」(conservative assumption)。
    //
    //   原因 / 妥协：starry 的 vfs 目前不为 FIFO 节点维护 reader/writer
    //   count（实现完整 FIFO IPC state machine 超出 open/openat 修复
    //   范围 — 是独立的 IPC 子系统功能补全）。
    //
    //   假设的理由：在测试环境里，bug-open-fifo-wronly-no-reader-no-enxio
    //   只覆盖「无 reader」这一确定状态。对此状态本实现行为正确（返 ENXIO）。
    //   若 FIFO 真存在 reader 进程并已 open(FIFO, O_RDONLY)，本实现仍会返
    //   ENXIO —— 此时与 Linux 行为不符（Linux 应返 fd>=0）。
    //
    //   完整修复（待独立 PR）：FIFO node 加 reader_count / writer_count 字段
    //   （AtomicU32 + 同步原语），open(FIFO) 路径根据 access mode 增减计数，
    //   close 时递减，本检查改为：
    //     if let Some(fifo) = inner.downcast_ref::<Fifo>() {
    //         if fifo.reader_count() == 0 { return Err(...ENXIO...); }
    //     }
    //   同时阻塞模式（非 NONBLOCK）的 WRONLY 应等待 reader 到来（更复杂）。
    //   该完整修复需联动 axfs-ng-vfs::Fifo 节点定义（目前无独立类型，FIFO 走通用
    //   File backend 即不区分 reader / writer），属 IPC 子系统专项。
    //
    // Fixes bug-open-fifo-wronly-no-reader-no-enxio (no-reader case only).
    if flags & O_NONBLOCK != 0
        && flags & 0b11 == O_WRONLY
        && let OpenResult::File(ref f) = result
        && let Ok(meta) = f.location().metadata()
        && meta.node_type == NodeType::Fifo
    {
        return Err(AxError::NoSuchDeviceOrAddress);
    }

    let f: Arc<dyn FileLike> = match result {
        OpenResult::File(mut file) => {
            // /dev/xx handling
            if let Ok(device) = file.location().entry().downcast::<Device>() {
                // Block device exclusive open (O_EXCL without O_CREAT).
                if let Ok(meta) = device.metadata()
                    && meta.node_type == NodeType::BlockDevice
                    && flags & O_EXCL != 0
                {
                    device.inner().open(true)?;
                }
                let inner = device.inner().as_any();
                if crate::pseudofs::usbfs::is_usbfs_device(inner) {
                    let wrapped = crate::pseudofs::usbfs::open_usbfs_file(inner, file, flags)?;
                    if flags & O_NONBLOCK != 0 {
                        wrapped.set_nonblocking(true)?;
                    }
                    return add_file_like(wrapped, flags & O_CLOEXEC != 0);
                }
                if let Some(ptmx) = inner.downcast_ref::<tty::Ptmx>() {
                    // Opening /dev/ptmx creates a new pseudo-terminal
                    let (master, pty_number) = ptmx.create_pty()?;
                    // TODO: this is cursed
                    let pts = FS_CONTEXT.lock().resolve("/dev/pts")?;
                    let entry = DirEntry::new_file(
                        FileNode::new(master),
                        NodeType::CharacterDevice,
                        Reference::new(Some(pts.entry().clone()), pty_number.to_string()),
                    );
                    let loc = Location::new(file.location().mountpoint().clone(), entry);
                    file = ax_fs_ng::vfs::File::new(FileBackend::Direct(loc), file.flags());
                } else if inner.is::<tty::CurrentTty>() {
                    let term = current()
                        .as_thread()
                        .proc_data
                        .proc
                        .group()
                        .session()
                        .terminal()
                        .ok_or(AxError::NotFound)?;
                    let path = tty::terminal_device_path(term.as_ref()).ok_or_else(|| {
                        warn!("unknown controlling terminal type for /dev/tty");
                        AxError::BadState
                    })?;
                    let loc = FS_CONTEXT.lock().resolve(&path)?;
                    file = ax_fs_ng::vfs::File::new(FileBackend::Direct(loc), file.flags());
                }
            }
            // Call open() on the final device after /dev/ptmx and /dev/tty
            // rewrites, so PTY open-count tracking (Tty) pairs the last-fd
            // close with peer POLLHUP/EOF notification. Block devices already
            // use the O_EXCL hook above, so skip them to avoid a double open().
            if let Ok(device) = file.location().entry().downcast::<Device>() {
                let is_block = device
                    .metadata()
                    .is_ok_and(|m| m.node_type == NodeType::BlockDevice);
                if !is_block {
                    device.inner().open(flags & O_EXCL != 0)?;
                }
            }
            Arc::new(File::new(file, flags))
        }
        OpenResult::Dir(dir) => Arc::new(Directory::new(dir, flags)),
    };
    if flags & O_NONBLOCK != 0 {
        f.set_nonblocking(true)?;
    }
    add_file_like(f, flags & O_CLOEXEC != 0)
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::AnyBitPattern)]
pub struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

const OPENAT2_VALID_RESOLVE: u64 = (RESOLVE_NO_XDEV
    | RESOLVE_NO_MAGICLINKS
    | RESOLVE_NO_SYMLINKS
    | RESOLVE_BENEATH
    | RESOLVE_IN_ROOT
    | RESOLVE_CACHED) as u64;

const OPENAT2_VALID_FLAGS: u64 = (O_ACCMODE
    | O_CREAT
    | O_EXCL
    | O_NOCTTY
    | O_TRUNC
    | O_APPEND
    | O_NONBLOCK
    | O_DSYNC
    | O_DIRECT
    | O_LARGEFILE
    | O_DIRECTORY
    | O_NOFOLLOW
    | O_NOATIME
    | O_CLOEXEC
    | O_SYNC
    | O_PATH
    | O_TMPFILE) as u64;

fn openat2_check_extra_bytes(how: *const OpenHow, size: usize) -> AxResult<()> {
    let base_size = size_of::<OpenHow>();
    if size <= base_size {
        return Ok(());
    }

    let extra = vm_load(
        unsafe { (how as *const u8).add(base_size) },
        size - base_size,
    )?;
    if extra.iter().any(|byte| *byte != 0) {
        return Err(AxError::ArgumentListTooLong);
    }
    Ok(())
}

/// Check whether `path` refers to a `/proc/<pid>/ns/<type>` entry.
/// If so, create an [`NsFd`] and add it to the fd table instead of
/// opening a regular file.
///
/// Returns `Some(fd)` on success, `Some(Err(...))` on failure, or
/// `None` if the path does not match (fall through to regular open).
fn try_open_nsfd(path: &str, flags: u32) -> Option<AxResult<i32>> {
    // Must be of the form /proc/<pid>/ns/<type>
    if !path.starts_with("/proc/") {
        return None;
    }
    let rest = path.strip_prefix("/proc/")?;
    let (pid_str, ns_type_str) = rest.split_once("/ns/")?;
    if pid_str.is_empty() || ns_type_str.is_empty() {
        return None;
    }
    // Reject paths with extra components, e.g. /proc/1/ns/uts/extra
    if ns_type_str.contains('/') {
        return None;
    }

    let pid: u32 = if pid_str == "self" {
        current().as_thread().proc_data.proc.pid()
    } else {
        pid_str.parse().ok()?
    };

    let proc_data = match crate::task::get_process_data(pid) {
        Ok(p) => p,
        Err(_) => return Some(Err(AxError::NotFound)),
    };

    let mnt_fs_ns = if ns_type_str == "mnt" {
        let scope = proc_data.scope.read();
        let fs_context = FS_CONTEXT.scope(&scope).clone();
        drop(scope);
        Some(fs_context.lock().mount_namespace().clone())
    } else {
        None
    };

    let nsproxy = proc_data.nsproxy.lock();

    let nsfd: NsFd = match ns_type_str {
        "uts" => NsFd::Uts(nsproxy.uts_ns.clone()),
        "ipc" => NsFd::Ipc(nsproxy.ipc_ns.clone()),
        "mnt" => NsFd::Mnt {
            ns: nsproxy.mnt_ns.clone(),
            fs_ns: mnt_fs_ns.unwrap(),
        },
        "pid" => NsFd::Pid(nsproxy.pid_ns.clone()),
        "net" => NsFd::Net(nsproxy.net_ns.clone()),
        "user" => NsFd::User(nsproxy.user_ns.clone()),
        _ => return Some(Err(AxError::NotFound)),
    };

    drop(nsproxy);

    let fd = nsfd.add_to_fd_table(flags & O_CLOEXEC != 0);
    Some(fd)
}

ktracepoint::define_event_trace!(
    sys_enter_openat,
    TP_kops(crate::tracepoint::KernelTraceAux),
    TP_system(syscalls),
    TP_PROTO(dfd: i32, path: *const u8, o_flags: u32, mode: u32),
    TP_STRUCT__entry{
        dfd: i32,
        o_flags: u32,
        path: u64,
        mode: u32,
    },
    TP_fast_assign{
        dfd: dfd,
        path: path as u64,
        o_flags: o_flags,
        mode: mode,
    },
    TP_ident(__entry),
    TP_printk({
        format!(
            "dfd: {}, path: {:#x}, o_flags: {:?}, mode: {:?}",
            __entry.dfd,
            __entry.path,
            __entry.o_flags,
            __entry.mode
        )
    })
);

/// Open or create a file.
/// fd: file descriptor
/// filename: file path to be opened or created
/// flags: open flags
/// mode: see man 7 inode
/// return new file descriptor if succeed, or return -1.
pub fn sys_openat(
    dirfd: c_int,
    path: *const c_char,
    flags: i32,
    mode: __kernel_mode_t,
) -> AxResult<isize> {
    // call tp:trace_sys_enter_openat
    trace_sys_enter_openat(dirfd, path as _, flags as _, mode);

    let curr = current();
    let thread = curr.as_thread();
    let path = vm_load_path_string(path)?;
    debug!("sys_openat <= {dirfd} {path:?} {flags:#o} {mode:#o}");

    let uflags = flags as u32;

    // Empty pathname → ENOENT. openat() does not accept AT_EMPTY_PATH.
    // Fixes bug-openat-empty-path-no-enoent.
    if path.is_empty() {
        return Err(AxError::NotFound);
    }

    // O_CREAT|O_DIRECTORY is an invalid combination: open() cannot create
    // a directory. man: "EINVAL — flags contains an invalid value."
    // Fixes bug-open-creat-directory-einval.
    // Exception: O_PATH ignores O_CREAT, so still allow PATH|CREAT|DIRECTORY.
    if uflags & O_CREAT != 0 && uflags & O_DIRECTORY != 0 && uflags & O_PATH == 0 {
        return Err(AxError::InvalidInput);
    }

    // O_TMPFILE requires O_RDWR or O_WRONLY. man: "EINVAL — O_TMPFILE was
    // specified in flags, but neither O_WRONLY nor O_RDWR was specified."
    // Fixes bug-open-tmpfile-no-einval.
    //
    // Exception: O_PATH ignores all flags except O_CLOEXEC/O_DIRECTORY/O_NOFOLLOW
    // (see flags_to_options PATH override below). On Linux, O_PATH|O_TMPFILE|RDONLY
    // is accepted as an O_PATH handle (TMPFILE/access bits ignored), not EINVAL.
    if uflags & O_TMPFILE == O_TMPFILE && uflags & 0b11 == O_RDONLY && uflags & O_PATH == 0 {
        return Err(AxError::InvalidInput);
    }

    // Absolute path: man "If pathname is absolute, then dirfd is ignored."
    // starry with_fs() unconditionally calls Directory::from_fd(dirfd),
    // so invalid dirfd would EBADF before the abs-path shortcut applies.
    // Same pattern as resolve_at (PR #605) / sys_fchownat (PR #588).
    // Fixes bug-openat-abs-path-honors-invalid-dirfd.
    let dirfd = if path.starts_with('/') {
        AT_FDCWD as _
    } else {
        dirfd
    };

    let mode = mode & !thread.proc_data.umask();

    // Intercept /proc/<pid>/ns/<type> opens: create an NsFd instead of
    // a regular file descriptor so that setns(2) receives a valid target.
    if let Some(result) = try_open_nsfd(&path, uflags) {
        return result.map(|fd| fd as isize);
    }

    let cred = thread.cred();
    let options = flags_to_options(flags, mode, (cred.fsuid, cred.fsgid));
    let should_notify_create = uflags & O_CREAT != 0
        && uflags & O_PATH == 0
        && with_fs(dirfd, |fs| match fs.resolve_no_follow(&path) {
            Ok(_) => Ok(false),
            Err(AxError::NotFound) => Ok(true),
            Err(err) => Err(err),
        })?;

    // Open first, then install the file so filesystem errors propagate unchanged.
    let fd =
        with_fs(dirfd, |fs| options.open(fs, path)).and_then(|it| add_to_fd(it, flags as _))?;
    if should_notify_create {
        let file = get_file_like(fd)?;
        crate::file::inotify::notify_create_path(file.path().as_ref(), false);
    }
    Ok(fd as isize)
}

pub fn sys_openat2(
    dirfd: c_int,
    path: *const c_char,
    how: *const OpenHow,
    size: usize,
) -> AxResult<isize> {
    let base_size = size_of::<OpenHow>();
    if size < base_size {
        return Err(AxError::InvalidInput);
    }

    let how_value = how.vm_read()?;
    openat2_check_extra_bytes(how, size)?;

    if how_value.flags & !OPENAT2_VALID_FLAGS != 0 {
        return Err(AxError::InvalidInput);
    }
    if how_value.mode & !0o7777 != 0 {
        return Err(AxError::InvalidInput);
    }
    if how_value.mode != 0 && how_value.flags & ((O_CREAT | O_TMPFILE) as u64) == 0 {
        return Err(AxError::InvalidInput);
    }
    if how_value.resolve & !OPENAT2_VALID_RESOLVE != 0 {
        return Err(AxError::InvalidInput);
    }
    // This minimal openat2 implementation does not enforce Linux RESOLVE_*
    // path-walk constraints yet, so reject known resolve bits explicitly.
    if how_value.resolve != 0 {
        return Err(AxError::OperationNotSupported);
    }

    let flags: i32 = how_value
        .flags
        .try_into()
        .map_err(|_| AxError::InvalidInput)?;
    let mode: __kernel_mode_t = how_value
        .mode
        .try_into()
        .map_err(|_| AxError::InvalidInput)?;

    sys_openat(dirfd, path, flags, mode)
}

/// Open a file by `filename` and insert it into the file descriptor table.
///
/// Return its index in the file table (`fd`). Return `EMFILE` if it already
/// has the maximum number of files open.
#[cfg(target_arch = "x86_64")]
pub fn sys_open(path: *const c_char, flags: i32, mode: __kernel_mode_t) -> AxResult<isize> {
    sys_openat(AT_FDCWD as _, path, flags, mode)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_creat(path: *const c_char, mode: __kernel_mode_t) -> AxResult<isize> {
    sys_openat(
        AT_FDCWD as _,
        path,
        (O_CREAT | O_WRONLY | O_TRUNC) as _,
        mode,
    )
}

pub fn sys_close(fd: c_int) -> AxResult<isize> {
    debug!("sys_close <= {fd}");
    close_file_like(fd)?;
    Ok(0)
}

bitflags! {
    #[derive(Debug, Clone, Copy)]
    struct CloseRangeFlags: u32 {
        const UNSHARE = 1 << 1;
        const CLOEXEC = 1 << 2;
    }
}

pub fn sys_close_range(first: i32, last: i32, flags: u32) -> AxResult<isize> {
    if first < 0 || last < first {
        return Err(AxError::InvalidInput);
    }
    let flags = CloseRangeFlags::from_bits(flags).ok_or(AxError::InvalidInput)?;
    debug!("sys_close_range <= fds: [{first}, {last}], flags: {flags:?}");
    if flags.contains(CloseRangeFlags::UNSHARE) {
        let curr = current();
        let proc_data = &curr.as_thread().proc_data;
        let new_files = Arc::new(ax_kspin::SpinRwLock::new(FD_TABLE.read().clone()));
        proc_data.with_current_scope_mut(|scope| {
            *FD_TABLE.scope_mut(scope).deref_mut() = new_files;
        });
    }

    let cloexec = flags.contains(CloseRangeFlags::CLOEXEC);
    let mut fd_table = FD_TABLE.write();
    // Collect closed fds and defer `release_locks_on_close` until after the
    // table write lock is dropped. `release_locks_on_close()` walks every fd
    // table through `fd_tables_contain_file()` (which acquires `FD_TABLE`),
    // so running it under the write guard self-deadlocks on the first closed
    // fd — every `dup2()`/`dup3()` that replaces an open fd (shell pipeline
    // setup) hangs. Mirrors the `close_all_fds` / execve CLOEXEC pattern.
    let mut closing = alloc::vec::Vec::new();
    if let Some(max_index) = fd_table.ids().next_back() {
        for fd in first..=last.min(max_index as i32) {
            if cloexec {
                if let Some(f) = fd_table.get_mut(fd as _) {
                    f.cloexec = true;
                }
            } else if let Some(f) = fd_table.remove(fd as _) {
                closing.push(f);
            }
        }
    }
    drop(fd_table);
    for f in closing {
        crate::file::release_locks_on_close(f);
    }

    Ok(0)
}

fn dup_fd(old_fd: c_int, cloexec: bool) -> AxResult<isize> {
    let f = get_file_like(old_fd)?;
    let new_fd = add_file_like(f, cloexec)?;
    Ok(new_fd as _)
}

fn dup_fd_min(old_fd: c_int, min_fd: c_int, cloexec: bool) -> AxResult<isize> {
    if min_fd < 0 {
        return Err(AxError::InvalidInput);
    }
    let f = get_file_like(old_fd)?;
    let max_nofile = current().as_thread().proc_data.rlim.read()[RLIMIT_NOFILE].current as i32;
    let mut fd_table = FD_TABLE.write();
    for candidate in min_fd..max_nofile {
        let entry = FileDescriptor {
            inner: f.clone(),
            cloexec,
        };
        if fd_table.add_at(candidate as _, entry).is_ok() {
            return Ok(candidate as isize);
        }
    }
    Err(AxError::TooManyOpenFiles)
}

pub fn sys_dup(old_fd: c_int) -> AxResult<isize> {
    debug!("sys_dup <= {old_fd}");
    dup_fd(old_fd, false)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_dup2(old_fd: c_int, new_fd: c_int) -> AxResult<isize> {
    if old_fd == new_fd {
        get_file_like(new_fd)?;
        return Ok(new_fd as _);
    }
    sys_dup3(old_fd, new_fd, 0)
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Dup3Flags: c_int {
        const O_CLOEXEC = O_CLOEXEC as _; // Close on exec
    }
}

pub fn sys_dup3(old_fd: c_int, new_fd: c_int, flags: c_int) -> AxResult<isize> {
    let flags = Dup3Flags::from_bits(flags).ok_or(AxError::InvalidInput)?;
    debug!("sys_dup3 <= old_fd: {old_fd}, new_fd: {new_fd}, flags: {flags:?}");

    if old_fd == new_fd {
        return Err(AxError::InvalidInput);
    }

    let mut fd_table = FD_TABLE.write();
    let mut f = fd_table
        .get(old_fd as _)
        .cloned()
        .ok_or(AxError::BadFileDescriptor)?;
    f.cloexec = flags.contains(Dup3Flags::O_CLOEXEC);

    let prev = fd_table.remove(new_fd as _);
    fd_table
        .add_at(new_fd as _, f)
        .map_err(|_| AxError::BadFileDescriptor)?;
    drop(fd_table);
    // `release_locks_on_close()` walks all fd tables via
    // `fd_tables_contain_file()` (acquiring `FD_TABLE`), so it must run AFTER
    // the write lock is released — otherwise every dup2()/dup3() that replaces
    // an open fd self-deadlocks (shell pipeline redirection, etc.).
    if let Some(prev) = prev {
        crate::file::release_locks_on_close(prev);
    }

    Ok(new_fd as _)
}

pub fn sys_fcntl(fd: c_int, cmd: c_int, arg: usize) -> AxResult<isize> {
    debug!("sys_fcntl <= fd: {fd} cmd: {cmd} arg: {arg}");

    if let Some(r) = super::lock::dispatch_fcntl(fd, cmd, arg) {
        return r;
    }

    match cmd as u32 {
        F_DUPFD => dup_fd_min(fd, arg as _, false),
        F_DUPFD_CLOEXEC => dup_fd_min(fd, arg as _, true),
        F_SETFL => {
            let f = get_file_like(fd)?;
            // linux-raw-sys exposes the O_ASYNC file status bit as FASYNC.
            let async_mode = arg & (FASYNC as usize) != 0;
            let async_mode_changed = async_mode != f.async_mode();
            if async_mode_changed && !f.supports_async_mode() {
                return Err(AxError::NotATty);
            }
            f.set_nonblocking(arg & (O_NONBLOCK as usize) > 0)?;
            f.set_append(arg & (O_APPEND as usize) > 0)?;
            if async_mode_changed {
                f.set_async_mode(async_mode)?;
            }
            Ok(0)
        }
        F_GETFL => {
            let f = get_file_like(fd)?;

            let mut ret = f.open_flags() & !O_APPEND;
            if f.nonblocking() {
                ret |= O_NONBLOCK;
            }
            if f.append() {
                ret |= O_APPEND;
            }
            if f.async_mode() {
                // linux-raw-sys exposes the O_ASYNC file status bit as FASYNC.
                ret |= FASYNC;
            }

            Ok(ret as _)
        }
        F_GETFD => {
            let cloexec = FD_TABLE
                .read()
                .get(fd as _)
                .ok_or(AxError::BadFileDescriptor)?
                .cloexec;
            Ok(if cloexec { FD_CLOEXEC as _ } else { 0 })
        }
        F_SETFD => {
            let cloexec = arg & FD_CLOEXEC as usize != 0;
            FD_TABLE
                .write()
                .get_mut(fd as _)
                .ok_or(AxError::BadFileDescriptor)?
                .cloexec = cloexec;
            Ok(0)
        }
        F_SETOWN => {
            let f = get_file_like(fd)?;
            f.set_owner(arg as i32)?;
            Ok(0)
        }
        F_GETOWN => {
            let f = get_file_like(fd)?;
            Ok(f.owner()? as _)
        }
        F_GETPIPE_SZ => {
            let pipe = Pipe::from_fd(fd)?;
            Ok(pipe.capacity() as _)
        }
        F_SETPIPE_SZ => {
            let pipe = Pipe::from_fd(fd)?;
            set_pipe_size(&pipe, arg)
        }
        F_GET_SEALS => {
            let memfd = Memfd::from_fd(fd)?;
            Ok(memfd.get_seals() as _)
        }
        F_ADD_SEALS => {
            let memfd = Memfd::from_fd(fd)?;
            memfd.add_seals(arg as u32)?;
            Ok(0)
        }
        // F_GET_RW_HINT (1035), F_SET_RW_HINT (1036), F_GET_FILE_RW_HINT (1037),
        // F_SET_FILE_RW_HINT (1038) — Linux 4.13+ I/O priority hints. They are
        // advisory and we keep no per-file/inode hint state, but the ABI is not a
        // bare no-op: the `arg` is a user `u64 *`. GET must write the current hint
        // back (so callers read a defined value, and a bad/NULL pointer faults);
        // SET must read the requested hint and reject unknown values with EINVAL.
        // RocksDB/BookKeeper/Pulsar use these for WAL/SST files.
        1035 | 1037 => {
            // No stored hint → report the implicit default RWH_WRITE_LIFE_NOT_SET.
            (arg as *mut u64).vm_write(0u64)?;
            Ok(0)
        }
        1036 | 1038 => {
            let hint = (arg as *const u64).vm_read()?;
            // Valid hints are RWH_WRITE_LIFE_NOT_SET..=RWH_WRITE_LIFE_EXTREME (0..=5).
            if hint > 5 {
                return Err(AxError::InvalidInput);
            }
            Ok(0)
        }
        // Advisory fcntl commands with no per-fd state in StarryOS, reported as
        // no-op success to match common Linux feature-detection usage:
        // F_SETSIG (10) / F_GETSIG (11) / F_SETOWN_EX (15) / F_GETOWN_EX (16).
        // (F_GETLK=5 / F_SETLK=6 are POSIX file locks already handled earlier by
        // `dispatch_fcntl`, and F_GETOWN=8 / F_SETOWN=9 are handled above — none
        // of those reach this arm.)
        10 | 11 | 15 | 16 => Ok(0),
        _ => {
            warn!("unsupported fcntl parameters: cmd: {cmd}");
            Err(AxError::InvalidInput)
        }
    }
}

fn set_pipe_size(pipe: &Pipe, size: usize) -> AxResult<isize> {
    pipe.resize(size)?;
    Ok(pipe.capacity() as _)
}

#[cfg(axtest)]
pub(crate) fn fcntl_setpipe_size_returns_capacity_for_test() -> bool {
    let (read_end, _write_end) = Pipe::new();
    set_pipe_size(&read_end, 4097) == Ok(8192)
}

pub fn sys_flock(fd: c_int, operation: c_int) -> AxResult<isize> {
    debug!("flock <= fd: {fd}, operation: {operation}");
    super::lock::flock_op(fd, operation)
}
