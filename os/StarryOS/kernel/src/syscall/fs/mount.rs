use alloc::{string::String, sync::Arc, vec::Vec};
use core::ffi::{c_char, c_void};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_fs_ng::vfs::is_mount_busy as fs_is_mount_busy;
use axfs_ng_vfs::NodePermission;

use crate::{
    file::{Directory, FD_TABLE, File, FileLike},
    mm::vm_load_string,
    pseudofs::{
        MemoryFs,
        dev::{
            new_devptsfs,
            tty::{DevPtsMount, DevPtsOptions},
        },
        overlay::OverlayOptions,
    },
    task::{current_user_task, tasks},
};

const MNT_FORCE: i32 = 1;
const MNT_DETACH: i32 = 2;
const MNT_EXPIRE: i32 = 4;
const UMOUNT_NOFOLLOW: i32 = 8;

const MS_RDONLY: i32 = 1;
const MS_NOSUID: i32 = 2;
const MS_NODEV: i32 = 4;
const MS_NOEXEC: i32 = 8;
const MS_NOATIME: i32 = 1 << 10;
const MS_RELATIME: i32 = 1 << 21;
const MS_STRICTATIME: i32 = 1 << 24;
const MS_REMOUNT: i32 = 1 << 5;
const MS_BIND: i32 = 1 << 12;
const MS_MOVE: i32 = 1 << 13;
const MS_REC: i32 = 1 << 14;
const MS_SILENT: i32 = 1 << 15;
const MS_UNBINDABLE: i32 = 1 << 17;
const MS_PRIVATE: i32 = 1 << 18;
const MS_SLAVE: i32 = 1 << 19;
const MS_SHARED: i32 = 1 << 20;

const MOUNT_OPTION_FLAGS: i32 =
    MS_RDONLY | MS_NOSUID | MS_NODEV | MS_NOEXEC | MS_NOATIME | MS_RELATIME | MS_STRICTATIME;

const PROPAGATION_FLAGS: i32 = MS_SHARED | MS_PRIVATE | MS_SLAVE | MS_UNBINDABLE;
const VALID_UMOUNT_FLAGS: i32 = MNT_FORCE | MNT_DETACH | MNT_EXPIRE | UMOUNT_NOFOLLOW;

fn parse_devpts_mode(value: &str) -> AxResult<NodePermission> {
    let mode = u16::from_str_radix(value, 8).map_err(|_| AxError::InvalidInput)?;
    NodePermission::from_bits(mode).ok_or(AxError::InvalidInput)
}

enum DevPtsInstanceKind {
    Legacy,
    New,
}

fn parse_devpts_options(data: *const c_void) -> AxResult<DevPtsMount> {
    let mut options = DevPtsOptions::mounted();
    let mut instance = DevPtsInstanceKind::Legacy;
    if data.is_null() {
        return Ok(DevPtsMount::Legacy(options));
    }

    for item in vm_load_string(data.cast())?.split(',') {
        if item.is_empty() {
            continue;
        }
        if item == "newinstance" {
            instance = DevPtsInstanceKind::New;
            continue;
        }
        let (key, value) = item.split_once('=').ok_or(AxError::InvalidInput)?;
        match key {
            "mode" => options.slave_mode = parse_devpts_mode(value)?,
            "gid" => {
                options.slave_gid = value.parse().map_err(|_| AxError::InvalidInput)?;
            }
            "ptmxmode" => options.ptmx_mode = parse_devpts_mode(value)?,
            _ => return Err(AxError::InvalidInput),
        }
    }
    Ok(match instance {
        DevPtsInstanceKind::Legacy => DevPtsMount::Legacy(options),
        DevPtsInstanceKind::New => DevPtsMount::NewInstance(options),
    })
}

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
    let Ok(tasks) = tasks() else {
        return true;
    };
    for task in tasks {
        let fd_table = task
            .as_thread()
            .with_scope(|scope| FD_TABLE.scope_cell(scope).clone());
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
    let source = if source.is_null() {
        String::new()
    } else {
        vm_load_string(source)?
    };
    let target = vm_load_string(target)?;
    let fs_type = if fs_type.is_null() {
        String::new()
    } else {
        vm_load_string(fs_type)?
    };
    debug!("sys_mount <= source: {source:?}, target: {target:?}, fs_type: {fs_type:?}");

    if !current_user_task().as_thread().cred().has_cap_sys_admin() {
        return Err(AxError::OperationNotPermitted);
    }

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
        if (flags & MS_REC) != 0 {
            match propagation {
                MS_SHARED => mountpoint.set_shared_recursive(),
                MS_PRIVATE => mountpoint.set_private_recursive(),
                MS_SLAVE => mountpoint.set_slave_recursive(),
                MS_UNBINDABLE => mountpoint.set_unbindable_recursive(),
                _ => {}
            }
        } else {
            match propagation {
                MS_SHARED => mountpoint.set_shared(),
                MS_PRIVATE => mountpoint.set_private(),
                MS_SLAVE => mountpoint.set_slave(),
                MS_UNBINDABLE => mountpoint.set_unbindable(),
                _ => {}
            }
        }
        return Ok(0);
    }

    if (flags & MS_REMOUNT) != 0 {
        let target = ax_fs_ng::vfs::current_fs_context().lock().resolve(target)?;
        if !target.is_root_of_mount() {
            return Err(AxError::InvalidInput);
        }
        let mp = target.mountpoint();
        mp.set_readonly((flags & MS_RDONLY) != 0);
        mp.set_mount_flags((flags & MOUNT_OPTION_FLAGS) as u32);
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
        target.bind_mount(&source, (flags & MS_REC) != 0)?;
        return Ok(0);
    }

    match fs_type.as_str() {
        "proc" | "sysfs" | "devtmpfs" | "tmpfs" => {
            let fs = MemoryFs::new();
            let target = ax_fs_ng::vfs::current_fs_context().lock().resolve(target)?;
            let mp = target.mount_with_source(&fs, mount_source(&source))?;
            if (flags & MS_RDONLY) != 0 {
                mp.set_readonly(true);
            }
            mp.set_mount_flags((flags & MOUNT_OPTION_FLAGS) as u32);
        }
        "devpts" => {
            let fs = new_devptsfs(parse_devpts_options(data)?);
            let target = ax_fs_ng::vfs::current_fs_context().lock().resolve(target)?;
            let mp = target.mount(&fs)?;
            if (flags & MS_RDONLY) != 0 {
                mp.set_readonly(true);
            }
            mp.set_mount_flags((flags & MOUNT_OPTION_FLAGS) as u32);
        }
        "cgroup2" => {
            let (cgroup_root, cgroup_root_pin) = {
                let task = current_user_task();
                let nsproxy = task.as_thread().proc_data.nsproxy.lock();
                let namespace = nsproxy.cgroup_ns.lock();
                (namespace.root(), namespace.pin_root())
            };
            let fs = crate::pseudofs::cgroup::new_cgroup2fs(cgroup_root);
            let target = ax_fs_ng::vfs::current_fs_context().lock().resolve(target)?;
            let mp = target.mount_with_source(&fs, mount_source(&source))?;
            mp.set_lifetime_guard(Arc::new(cgroup_root_pin));
            if (flags & MS_RDONLY) != 0 {
                mp.set_readonly(true);
            }
            mp.set_mount_flags((flags & MOUNT_OPTION_FLAGS) as u32);
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
            let mp = target.mount_with_source(&fs, mount_source(&source))?;
            if readonly || (flags & MS_RDONLY) != 0 {
                mp.set_readonly(true);
            }
            mp.set_mount_flags((flags & MOUNT_OPTION_FLAGS) as u32);
        }
        _ => return Err(AxError::NoSuchDevice),
    }

    Ok(0)
}

fn mount_source(source: &str) -> &str {
    if source.is_empty() { "none" } else { source }
}

#[cfg(feature = "ext4")]
fn mount_ext4(source: &str, _target: &str, _readonly: bool) -> AxResult<()> {
    // The old loop-backed ext4 adapter implemented the removed synchronous
    // polling queue API. Keep its source for the later virtual-device
    // migration, but do not expose it through mount(2) as an IRQ-capable
    // device. Linux uses ENODEV when the requested filesystem/device backend
    // is not available in the running kernel.
    warn!(
        "mount_ext4: block backend for source {:?} has not been migrated",
        source
    );
    Err(AxError::NoSuchDevice)
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

    if !current_user_task().as_thread().cred().has_cap_sys_admin() {
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

    let plan = target
        .mountpoint()
        .plan_unmount(axfs_ng_vfs::UnmountKind::Normal)?;
    if plan.targets().any(is_mount_busy) {
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

    if plan.targets().any(is_mount_busy) {
        return Err(AxError::from(LinuxError::EBUSY));
    }
    target.commit_unmount(plan)?;

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

    if !current_user_task().as_thread().cred().has_cap_sys_admin() {
        return Err(AxError::OperationNotPermitted);
    }

    let fs_context = ax_fs_ng::vfs::current_fs_context();
    let mut ctx = fs_context.lock();

    // The caller's current root must itself be a mount point (Linux
    // EINVAL if e.g. the process chroot'd into a subdirectory).
    if !ctx.root_dir().is_root_of_mount() {
        return Err(AxError::InvalidInput);
    }

    // Resolve both paths before checking their VFS relationship. Linux permits
    // callers to enter new_root and use pivot_root(".", "old").
    let new_root_loc = ctx.resolve(&new_root)?;
    new_root_loc.check_is_dir()?;
    let put_old_loc = ctx.resolve(&put_old)?;
    put_old_loc.check_is_dir()?;

    if !put_old_loc.is_descendant_of(&new_root_loc) {
        return Err(AxError::InvalidInput);
    }

    // `pivot_root` rearranges mounts rather than arbitrary directories.
    if new_root_loc.is_root()
        || !new_root_loc.is_root_of_mount()
        || new_root_loc.ptr_eq(ctx.root_dir())
    {
        warn!(
            "sys_pivot_root: new_root {:?} is not a distinct non-global mount root",
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
    let mount_namespace = ctx.mount_namespace().clone();

    // Perform pivot: swap the root mount (updates this task's FsContext).
    ctx.pivot_root(new_root_loc, put_old_loc)?;

    let new_root_loc = ctx.root_dir().clone();
    drop(ctx); // Release this task's lock before touching others.

    // Propagate root / cwd to all other tasks whose root_dir or current_dir
    // exactly matches the old root Location — mirroring Linux
    // chroot_fs_refs() in fs/namespace.c.
    ax_fs_ng::vfs::FsContext::propagate_pivot_root(&mount_namespace, &old_root, &new_root_loc);

    Ok(0)
}

#[cfg(axtest)]
pub(crate) fn mount_flags_validation_rules_hold_for_test() -> bool {
    // Test umount flag validation
    const VALID_UMOUNT_FLAGS: i32 = MNT_FORCE | MNT_DETACH | MNT_EXPIRE | UMOUNT_NOFOLLOW;

    let flags = 0i32;
    assert!(flags & !VALID_UMOUNT_FLAGS == 0);

    let force_only = MNT_FORCE;
    assert!(force_only & !VALID_UMOUNT_FLAGS == 0);

    let detach_only = MNT_DETACH;
    assert!(detach_only & !VALID_UMOUNT_FLAGS == 0);

    let all_valid = VALID_UMOUNT_FLAGS;
    assert!(all_valid & !VALID_UMOUNT_FLAGS == 0);

    // Invalid flag should be detected
    let invalid_flags = 0xFFFFi32;
    assert!(invalid_flags & !VALID_UMOUNT_FLAGS != 0);

    // Test propagation flags
    const PROPAGATION_FLAGS: i32 = MS_SHARED | MS_PRIVATE | MS_SLAVE | MS_UNBINDABLE;

    assert!(MS_SHARED & PROPAGATION_FLAGS != 0);
    assert!(MS_PRIVATE & PROPAGATION_FLAGS != 0);
    assert!(MS_SLAVE & PROPAGATION_FLAGS != 0);
    assert!(MS_UNBINDABLE & PROPAGATION_FLAGS != 0);

    true
}
