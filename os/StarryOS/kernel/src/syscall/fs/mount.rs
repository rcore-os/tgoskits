use alloc::{string::String, vec::Vec};
use core::ffi::{c_char, c_void};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_fs_ng::vfs::{
    MountIdentity, MountPropagation, current_fs_context, is_mount_busy as fs_is_mount_busy,
};

use crate::{
    file::{Directory, FD_TABLE, File, FileLike},
    mm::vm_load_string,
    pseudofs::{MemoryFs, overlay::OverlayOptions},
    task::tasks,
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

fn fd_points_to_mount(fd: &dyn FileLike, mount: &MountIdentity) -> bool {
    fd.downcast_ref::<File>()
        .is_some_and(|file| file.inner().is_on_mount(mount).unwrap_or(false))
        || fd.downcast_ref::<Directory>().is_some_and(|directory| {
            directory
                .with_operation(|view| Ok(view.is_on_mount(mount)))
                .unwrap_or(false)
        })
}

fn is_mount_busy(mount: &MountIdentity) -> AxResult<bool> {
    if fs_is_mount_busy(mount) {
        return Ok(true);
    }
    for task in tasks()? {
        let thread = task.as_thread();
        let scope = thread.proc_data.scope.read();
        let fd_table = FD_TABLE.scope_cell(&scope).clone();
        drop(scope);
        let table = fd_table.read();
        if table.ids().any(|id| {
            table
                .get(id)
                .is_some_and(|fd| fd_points_to_mount(&*fd.inner, mount))
        }) {
            return Ok(true);
        }
    }
    Ok(false)
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

        current_fs_context()
            .lock()
            .with_namespace_operation(|namespace| {
                let target = namespace.resolve_path(target)?;
                if !target.is_mount_root() {
                    return Err(AxError::InvalidInput);
                }
                let propagation = match propagation {
                    MS_SHARED => MountPropagation::Shared,
                    MS_PRIVATE => MountPropagation::Private,
                    MS_SLAVE => MountPropagation::Slave,
                    MS_UNBINDABLE => MountPropagation::Unbindable,
                    _ => return Err(AxError::InvalidInput),
                };
                target.set_mount_propagation(propagation);
                Ok(())
            })?;
        return Ok(0);
    }

    if (flags & MS_REMOUNT) != 0 {
        current_fs_context()
            .lock()
            .with_namespace_operation(|namespace| {
                let target = namespace.resolve_path(target)?;
                if !target.is_mount_root() {
                    return Err(AxError::InvalidInput);
                }
                if (flags & MS_RDONLY) != 0 {
                    target.set_mount_readonly(true);
                }
                Ok(())
            })?;
        return Ok(0);
    }

    if (flags & MS_MOVE) != 0 {
        let fs_context = current_fs_context();
        let ctx = fs_context.lock();
        ctx.with_namespace_operation(|namespace| {
            let source = namespace.resolve_path(source)?;
            let target = namespace.resolve_path(target)?;
            source.move_mount(&target)
        })?;
        return Ok(0);
    }

    if (flags & MS_BIND) != 0 {
        let fs_context = current_fs_context();
        let ctx = fs_context.lock();
        ctx.with_namespace_operation(|namespace| {
            let source = namespace.resolve_path(source)?;
            let target = namespace.resolve_path(target)?;
            target
                .bind_mount(&source, (flags & MS_REC) != 0, (flags & MS_RDONLY) != 0)
                .map(drop)
        })?;
        return Ok(0);
    }

    match fs_type.as_str() {
        "proc" | "sysfs" | "devtmpfs" | "devpts" | "tmpfs" => {
            let fs = MemoryFs::new();
            current_fs_context()
                .lock()
                .with_namespace_operation(|namespace| {
                    namespace
                        .resolve_path(target)?
                        .mount_filesystem(&fs, (flags & MS_RDONLY) != 0)
                        .map(drop)
                })?;
        }
        "cgroup2" => {
            let fs = crate::pseudofs::cgroup::new_cgroup2fs();
            current_fs_context()
                .lock()
                .with_namespace_operation(|namespace| {
                    namespace
                        .resolve_path(target)?
                        .mount_filesystem(&fs, (flags & MS_RDONLY) != 0)
                        .map(drop)
                })?;
        }
        #[cfg(feature = "ext4")]
        "ext4" => {
            mount_ext4(&source, &target, (flags & MS_RDONLY) != 0)?;
        }
        "overlay" => {
            let (lower_paths, upper_path, work_path) = parse_overlay_options(data)?;
            let fs_context = current_fs_context();
            let ctx = fs_context.lock();
            ctx.with_namespace_operation(|namespace| {
                let lower_dirs = lower_paths
                    .iter()
                    .map(|path| namespace.retain(path))
                    .collect::<Result<Vec<_>, _>>()?;
                let upper_dir = upper_path
                    .as_ref()
                    .map(|path| namespace.retain(path))
                    .transpose()?;
                let work_dir = work_path
                    .as_ref()
                    .map(|path| namespace.retain(path))
                    .transpose()?;
                let readonly = upper_dir.is_none();
                let target = namespace.resolve_path(target)?;
                let fs = crate::pseudofs::overlay::new_overlayfs(
                    &target,
                    OverlayOptions {
                        lower_dirs,
                        upper_dir,
                        work_dir,
                    },
                )?;
                target
                    .mount_filesystem(&fs, readonly || (flags & MS_RDONLY) != 0)
                    .map(drop)
            })?;
        }
        _ => return Err(AxError::NoSuchDevice),
    }

    Ok(0)
}

#[cfg(feature = "ext4")]
fn mount_ext4(source: &str, target: &str, readonly: bool) -> AxResult<()> {
    use alloc::{boxed::Box, sync::Arc};

    let fs_context = current_fs_context();
    let ctx = fs_context.lock();

    ctx.with_namespace_operation(|namespace| {
        // Resolve source device path (e.g., "/dev/loop0") and snapshot the
        // device capability while the same namespace lease remains active.
        let source_location = namespace.resolve_path(source)?;
        let (block_device, ops) = source_location
            .with_node::<crate::pseudofs::Device, _>(|device| {
                let loop_dev = device
                    .inner()
                    .as_any()
                    .downcast_ref::<crate::pseudofs::dev::LoopDevice>()
                    .ok_or_else(|| {
                        warn!("mount_ext4: {:?} is not a loop device", source);
                        AxError::NoSuchDevice
                    })?;
                let block_device = loop_dev.block_device().inspect_err(|error| {
                    warn!("mount_ext4: loop block device creation failed: {error:?}");
                })?;
                let ops: Arc<dyn crate::pseudofs::DeviceOps> = device.inner().clone();
                Ok((block_device, ops))
            })
            .inspect_err(|_| warn!("mount_ext4: {:?} is not a loop device", source))?;

        let num_blocks = block_device.metadata().num_blocks();
        let region = ax_fs_ng::BlockRegion::from_num_blocks(num_blocks);

        // Loop devices are synchronous software block services and need no
        // IRQ or runtime request queue.
        let filesystem =
            ax_fs_ng::vfs::new_filesystem_from_device(block_device, region).map_err(|error| {
                warn!("mount_ext4: failed to create ext4 filesystem: {error:?}");
                AxError::Io
            })?;

        namespace
            .resolve_path(target)?
            .mount_filesystem(&filesystem, readonly)
            .map_err(|error| {
                warn!("mount_ext4: failed to mount at {:?}: {error:?}", target);
                AxError::Io
            })?;

        // Re-resolve through the newly installed mount and attach its
        // writeback action without exposing the mounted root location.
        namespace
            .resolve_path(target)?
            .insert_user_data(Box::new(move || {
                if let Some(loop_device) = ops
                    .as_any()
                    .downcast_ref::<crate::pseudofs::dev::LoopDevice>()
                {
                    loop_device.flush_cache_to_file()
                } else {
                    Ok(())
                }
            })
                as Box<dyn Fn() -> AxResult<()> + Send + Sync>);
        Ok(())
    })
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

    let fs_context = current_fs_context();
    let target = if (flags & UMOUNT_NOFOLLOW) != 0 {
        fs_context.lock().resolve_file_location_no_follow(target)?
    } else {
        fs_context.lock().resolve_file_location(target)?
    };

    target.with_operation(|target| {
        // Linux umount2 returns EINVAL for paths that are not mount points.
        if !target.is_mount_root() {
            return Err(AxError::InvalidInput);
        }

        if (flags & MNT_EXPIRE) != 0 && !target.mark_mount_expired() {
            return Err(AxError::from(LinuxError::EAGAIN));
        }

        if (flags & MNT_DETACH) != 0 {
            target.detach_mount()?;
            return Ok(());
        }

        // The opaque identity can be compared after dropping the context
        // lock, while this exact target operation lease blocks a concurrent
        // filesystem freeze.
        let mount = target.mount_identity();
        if is_mount_busy(&mount)? {
            return Err(AxError::from(LinuxError::EBUSY));
        }

        ax_fs_ng::file::sync_all_cached_files(false)?;

        let writeback = target.get_user_data::<Box<dyn Fn() -> AxResult<()> + Send + Sync>>();
        target.unmount()?;

        // Once unmounted, filesystem block I/O has stopped; flush the loop
        // backing cache before releasing the generation operation.
        if let Some(callback) = writeback {
            callback()?;
        }
        Ok(())
    })?;

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

    let fs_context = current_fs_context();
    let mut ctx = fs_context.lock();
    let transition = ctx
        .pivot_root_paths(&new_root, &put_old)
        .inspect_err(|error| {
            warn!("sys_pivot_root: failed to pivot to {new_root:?}: {error:?}");
        })?;
    drop(ctx); // Release this task's lock before touching others.

    ax_fs_ng::vfs::FsContext::propagate_pivot_root(&transition)?;

    Ok(0)
}
