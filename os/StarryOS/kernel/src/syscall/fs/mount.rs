use alloc::{string::String, sync::Arc, vec::Vec};
use core::ffi::{c_char, c_void};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_fs_ng::vfs::is_mount_busy as fs_is_mount_busy;
use ax_task::current;

use crate::{
    file::{Directory, FD_TABLE, File, FileLike},
    mm::vm_load_string,
    pseudofs::{MemoryFs, overlay::OverlayOptions},
    task::{AsThread, tasks},
};

const MNT_FORCE: i32 = 1;
const MNT_DETACH: i32 = 2;
const MNT_EXPIRE: i32 = 4;
const UMOUNT_NOFOLLOW: i32 = 8;

const MS_RDONLY: i32 = 1;
const MS_REMOUNT: i32 = 1 << 5;
const MS_BIND: i32 = 1 << 12;
const MS_MOVE: i32 = 1 << 13;
const MS_REC: i32 = 1 << 14;
const MS_SILENT: i32 = 1 << 15;
const MS_UNBINDABLE: i32 = 1 << 17;
const MS_PRIVATE: i32 = 1 << 18;
const MS_SLAVE: i32 = 1 << 19;
const MS_SHARED: i32 = 1 << 20;

const PROPAGATION_FLAGS: i32 = MS_SHARED | MS_PRIVATE | MS_SLAVE | MS_UNBINDABLE;
const VALID_UMOUNT_FLAGS: i32 = MNT_FORCE | MNT_DETACH | MNT_EXPIRE | UMOUNT_NOFOLLOW;

fn parse_overlay_options(
    data: *const c_void,
) -> AxResult<(Vec<String>, Option<String>, Option<String>)> {
    if data.is_null() {
        return Err(AxError::InvalidInput);
    }
    let data = vm_load_string(data.cast())?;
    let mut lowerdir = None;
    let mut upperdir = None;
    let mut workdir = None;

    for item in data.split(',') {
        let Some((key, value)) = item.split_once('=') else {
            continue;
        };
        match key {
            "lowerdir" => lowerdir = Some(value),
            "upperdir" => upperdir = Some(value),
            "workdir" => workdir = Some(value),
            "index" | "redirect_dir" if value != "off" => {
                return Err(AxError::OperationNotSupported);
            }
            _ => {}
        }
    }

    let lower_dirs = lowerdir
        .ok_or(AxError::InvalidInput)?
        .split(':')
        .filter(|path| !path.is_empty())
        .map(String::from)
        .collect::<Vec<_>>();
    if lower_dirs.is_empty() {
        return Err(AxError::InvalidInput);
    }

    if upperdir.is_some() != workdir.is_some() {
        return Err(AxError::InvalidInput);
    }

    Ok((
        lower_dirs,
        upperdir.map(String::from),
        workdir.map(String::from),
    ))
}

fn fd_points_to_mount(fd: &dyn FileLike, mp: &Arc<axfs_ng_vfs::Mountpoint>) -> bool {
    fd.downcast_ref::<File>()
        .is_some_and(|f| Arc::ptr_eq(f.inner().location().mountpoint(), mp))
        || fd
            .downcast_ref::<Directory>()
            .is_some_and(|d| Arc::ptr_eq(d.inner().mountpoint(), mp))
}

fn is_mount_busy(mp: &Arc<axfs_ng_vfs::Mountpoint>) -> bool {
    if fs_is_mount_busy(mp) {
        return true;
    }
    for task in tasks() {
        let Some(thread) = task.try_as_thread() else {
            continue;
        };
        let scope = thread.scope.read();
        let fd_table = FD_TABLE.scope(&scope).clone();
        drop(scope);
        let table = fd_table.read();
        if table.ids().any(|id| {
            table
                .get(id)
                .is_some_and(|fd| fd_points_to_mount(&*fd.inner, mp))
        }) {
            return true;
        }
    }
    false
}

pub fn sys_mount(
    source: *const c_char,
    target: *const c_char,
    fs_type: *const c_char,
    flags: i32,
    data: *const c_void,
) -> AxResult<isize> {
    let source = vm_load_string(source)?;
    let target = vm_load_string(target)?;
    let fs_type = if fs_type.is_null() {
        String::new()
    } else {
        vm_load_string(fs_type)?
    };
    debug!("sys_mount <= source: {source:?}, target: {target:?}, fs_type: {fs_type:?}");

    let propagation = flags & PROPAGATION_FLAGS;

    if propagation.count_ones() > 1 {
        return Err(AxError::InvalidInput);
    }

    if propagation != 0 {
        let allowed = propagation | MS_REC | MS_SILENT;
        if flags & !allowed != 0 {
            return Err(AxError::InvalidInput);
        }

        let target = ax_fs_ng::vfs::current_fs_context().lock().resolve(target)?;
        if !target.is_root_of_mount() {
            return Err(AxError::InvalidInput);
        }
        let mountpoint = target.mountpoint().clone();
        match propagation {
            MS_SHARED => mountpoint.set_shared(),
            MS_PRIVATE => mountpoint.set_private(),
            MS_SLAVE => mountpoint.set_slave(),
            MS_UNBINDABLE => mountpoint.set_unbindable(),
            _ => {}
        }
        return Ok(0);
    }

    if (flags & MS_REMOUNT) != 0 {
        let target = ax_fs_ng::vfs::current_fs_context().lock().resolve(target)?;
        if !target.is_root_of_mount() {
            return Err(AxError::InvalidInput);
        }
        if (flags & MS_RDONLY) != 0 {
            target.mountpoint().set_readonly(true);
        }
        return Ok(0);
    }

    if (flags & MS_MOVE) != 0 {
        let fs_context = ax_fs_ng::vfs::current_fs_context();
        let ctx = fs_context.lock();
        let source = ctx.resolve(source)?;
        let target = ctx.resolve(target)?;
        source.move_mount(&target)?;
        return Ok(0);
    }

    if (flags & MS_BIND) != 0 {
        let fs_context = ax_fs_ng::vfs::current_fs_context();
        let ctx = fs_context.lock();
        let source = ctx.resolve(source)?;
        let target = ctx.resolve(target)?;
        let mp = target.bind_mount(&source, (flags & MS_REC) != 0)?;
        if (flags & MS_RDONLY) != 0 {
            mp.set_readonly(true);
        }
        return Ok(0);
    }

    match fs_type.as_str() {
        "proc" | "sysfs" | "devtmpfs" | "devpts" | "tmpfs" => {
            let fs = MemoryFs::new();
            let target = ax_fs_ng::vfs::current_fs_context().lock().resolve(target)?;
            let mp = target.mount(&fs)?;
            if (flags & MS_RDONLY) != 0 {
                mp.set_readonly(true);
            }
        }
        "cgroup2" => {
            let fs = crate::pseudofs::cgroup::new_cgroup2fs();
            let target = ax_fs_ng::vfs::current_fs_context().lock().resolve(target)?;
            let mp = target.mount(&fs)?;
            if (flags & MS_RDONLY) != 0 {
                mp.set_readonly(true);
            }
        }
        #[cfg(feature = "ext4")]
        "ext4" => {
            mount_ext4(&source, &target, (flags & MS_RDONLY) != 0)?;
        }
        "overlay" => {
            let (lower_paths, upper_path, work_path) = parse_overlay_options(data)?;
            let fs_context = ax_fs_ng::vfs::current_fs_context();
            let ctx = fs_context.lock();
            let mut lower_dirs = Vec::new();
            for lower in lower_paths {
                lower_dirs.push(ctx.resolve(lower)?);
            }
            let upper_dir = upper_path.map(|path| ctx.resolve(path)).transpose()?;
            let work_dir = work_path.map(|path| ctx.resolve(path)).transpose()?;
            let readonly = upper_dir.is_none();
            let fs = crate::pseudofs::overlay::new_overlayfs(OverlayOptions {
                lower_dirs,
                upper_dir,
                work_dir,
            })?;
            let target = ctx.resolve(target)?;
            let mp = target.mount(&fs)?;
            if readonly || (flags & MS_RDONLY) != 0 {
                mp.set_readonly(true);
            }
        }
        _ => return Err(AxError::NoSuchDevice),
    }

    Ok(0)
}

#[cfg(feature = "ext4")]
fn mount_ext4(source: &str, target: &str, readonly: bool) -> AxResult<()> {
    use alloc::{boxed::Box, sync::Arc};

    let fs_context = ax_fs_ng::vfs::current_fs_context();
    let ctx = fs_context.lock();

    // Resolve source device path (e.g., "/dev/loop0") to a block device
    let source_loc = ctx.resolve(source)?;
    let device = source_loc
        .entry()
        .downcast::<crate::pseudofs::Device>()
        .map_err(|_| {
            warn!("mount_ext4: {:?} is not a device", source);
            AxError::NoSuchDevice
        })?;
    let loop_dev = device
        .inner()
        .as_any()
        .downcast_ref::<crate::pseudofs::dev::LoopDevice>()
        .ok_or_else(|| {
            warn!("mount_ext4: {:?} is not a loop device", source);
            AxError::NoSuchDevice
        })?;
    let handle = loop_dev.block_handle().inspect_err(|e| {
        warn!("mount_ext4: loop device block handle failed: {:?}", e);
    })?;

    let num_blocks = handle.device_info().num_blocks;
    let region = ax_fs_ng::BlockRegion::from_num_blocks(num_blocks);

    // Create ext4 filesystem from the native block runtime handle
    let fs = ax_fs_ng::vfs::new_filesystem_from_handle(handle, region).map_err(|e| {
        warn!("mount_ext4: failed to create ext4 filesystem: {:?}", e);
        AxError::Io
    })?;

    // Mount at the target location
    let target_loc = ctx.resolve(target)?;
    let mountpoint = target_loc.mount(&fs).map_err(|e| {
        warn!("mount_ext4: failed to mount at {:?}: {:?}", target, e);
        AxError::Io
    })?;
    mountpoint.set_readonly(readonly);

    // Store a writeback callback in the mount root's user_data so that
    // sys_umount2 can flush the loop device's block cache to the backing
    // file after the filesystem is unmounted.
    let ops: Arc<dyn crate::pseudofs::DeviceOps> = device.inner().clone();
    {
        let mount_root = ctx.resolve(target)?;
        mount_root.user_data().insert(Box::new(move || {
            if let Some(ld) = ops
                .as_any()
                .downcast_ref::<crate::pseudofs::dev::LoopDevice>()
            {
                ld.flush_cache_to_file()
            } else {
                Ok(())
            }
        }) as Box<dyn Fn() -> AxResult<()> + Send + Sync>);
    }

    Ok(())
}

pub fn sys_umount2(target: *const c_char, flags: i32) -> AxResult<isize> {
    use alloc::boxed::Box;

    let target = vm_load_string(target)?;
    debug!("sys_umount2 <= target: {target:?}, flags: {flags:#x}");

    if (flags & !VALID_UMOUNT_FLAGS) != 0 {
        return Err(AxError::InvalidInput);
    }

    if (flags & MNT_EXPIRE) != 0 && (flags & (MNT_FORCE | MNT_DETACH)) != 0 {
        return Err(AxError::InvalidInput);
    }

    if target.is_empty() {
        return Err(AxError::NotFound);
    }

    let target = if (flags & UMOUNT_NOFOLLOW) != 0 {
        ax_fs_ng::vfs::current_fs_context()
            .lock()
            .resolve_no_follow(target)?
    } else {
        ax_fs_ng::vfs::current_fs_context().lock().resolve(target)?
    };

    if !current().as_thread().cred().has_cap_sys_admin() {
        return Err(AxError::OperationNotPermitted);
    }

    // Linux umount2 returns EINVAL for paths that are not mount points.
    if !target.is_root_of_mount() {
        return Err(AxError::InvalidInput);
    }

    if (flags & MNT_EXPIRE) != 0 && !target.mountpoint().mark_expired() {
        return Err(AxError::from(LinuxError::EAGAIN));
    }

    if (flags & MNT_DETACH) != 0 {
        target.detach_mount()?;
        return Ok(0);
    }

    // Linux umount2 returns EBUSY if any task has cwd/root or open fd
    // inside the mount.
    if is_mount_busy(target.mountpoint()) {
        return Err(AxError::from(LinuxError::EBUSY));
    }

    // Flush closed-file page cache entries before the filesystem itself is
    // flushed by `Location::unmount()`. Otherwise data written through a file
    // descriptor that has already been closed can remain only in axfs-ng's
    // global cached-file list and miss the unmount writeback.
    ax_fs_ng::file::sync_all_cached_files(false)?;

    // Retrieve the writeback callback (if any) before unmount tears down
    // the mount.  For ext4-on-loop mounts this flushes the block device
    // cache to the backing file after the filesystem is unmounted; for
    // other filesystem types (tmpfs) the callback is absent.
    let writeback = {
        let ud = target.user_data();
        ud.get::<Box<dyn Fn() -> AxResult<()> + Send + Sync>>()
    }; // user_data lock released

    target.unmount()?;

    // After unmount, filesystem block I/O has stopped; it is safe to do VFS
    // writeback here. Propagate writeback errors so userspace sees EIO when
    // dirty data could not be persisted to the backing file.
    if let Some(cb) = writeback {
        cb()?;
    }

    Ok(0)
}

pub fn sys_pivot_root(new_root: *const c_char, put_old: *const c_char) -> AxResult<isize> {
    let new_root = vm_load_string(new_root)?;
    let put_old = vm_load_string(put_old)?;
    debug!(
        "sys_pivot_root <= new_root: {:?}, put_old: {:?}",
        new_root, put_old
    );

    // Validate: put_old must be at or under new_root (path-separator-aware
    // so that "/new" does not falsely match "/newroot/old").
    let nr = new_root.trim_end_matches('/');
    let nr_slash = alloc::format!("{}/", nr);
    if !(put_old == nr || put_old.starts_with(&nr_slash)) {
        return Err(AxError::InvalidInput);
    }
    // new_root cannot be "/"
    if new_root == "/" {
        return Err(AxError::InvalidInput);
    }

    let fs_context = ax_fs_ng::vfs::current_fs_context();
    let mut ctx = fs_context.lock();

    // The caller's current root must itself be a mount point (Linux
    // EINVAL if e.g. the process chroot'd into a subdirectory).
    if !ctx.root_dir().is_root_of_mount() {
        return Err(AxError::InvalidInput);
    }

    // Resolve paths
    let new_root_loc = ctx.resolve(&new_root)?;

    // Both must be directories
    new_root_loc.check_is_dir()?;
    let put_old_loc = ctx.resolve(&put_old)?;
    put_old_loc.check_is_dir()?;

    // new_root must be the root of a non-root mount (i.e. the root of a
    // filesystem mounted somewhere, not the global root).  Because path
    // resolution crosses mount boundaries transparently, the resolved
    // Location is the *root entry* of the mounted filesystem, so we check
    // is_root_of_mount + the mountpoint is not the global root.
    if !(new_root_loc.is_root_of_mount() && !new_root_loc.mountpoint().is_root()) {
        warn!(
            "sys_pivot_root: new_root {:?} is not the root of a mounted filesystem",
            new_root
        );
        return Err(AxError::InvalidInput);
    }

    // Capture the old root Location BEFORE the pivot, so that we can
    // propagate the change to every other task afterwards (Linux
    // chroot_fs_refs semantics).  We save the full Location (mountpoint +
    // dentry) rather than just the mountpoint, so that tasks chroot'd
    // into a subdirectory of the old root are not incorrectly updated.
    let old_root = ctx.root_dir().clone();

    // Perform pivot: swap the root mount (updates this task's FsContext).
    ctx.pivot_root(new_root_loc, put_old_loc)?;

    let new_root_loc = ctx.root_dir().clone();
    drop(ctx); // Release this task's lock before touching others.

    // Propagate root / cwd to all other tasks whose root_dir or current_dir
    // exactly matches the old root Location — mirroring Linux
    // chroot_fs_refs() in fs/namespace.c.
    ax_fs_ng::vfs::FsContext::propagate_pivot_root(&old_root, &new_root_loc);

    Ok(0)
}
