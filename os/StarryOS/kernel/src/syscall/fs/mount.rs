use core::ffi::{c_char, c_void};

use ax_errno::{AxError, AxResult};
use ax_fs::FS_CONTEXT;

use crate::{mm::vm_load_string, pseudofs::MemoryFs};

pub fn sys_mount(
    source: *const c_char,
    target: *const c_char,
    fs_type: *const c_char,
    _flags: i32,
    _data: *const c_void,
) -> AxResult<isize> {
    let source = vm_load_string(source)?;
    let target = vm_load_string(target)?;
    let fs_type = vm_load_string(fs_type)?;
    debug!("sys_mount <= source: {source:?}, target: {target:?}, fs_type: {fs_type:?}");

    match fs_type.as_str() {
        "tmpfs" => {
            let fs = MemoryFs::new();
            let target = FS_CONTEXT.lock().resolve(target)?;
            target.mount(&fs)?;
        }
        #[cfg(feature = "ext4")]
        "ext4" => {
            mount_ext4(&source, &target)?;
        }
        _ => return Err(AxError::NoSuchDevice),
    }

    Ok(0)
}

#[cfg(feature = "ext4")]
fn mount_ext4(source: &str, target: &str) -> AxResult<()> {
    use alloc::boxed::Box;

    use ax_driver::prelude::BlockDriverOps;

    let mut ctx = FS_CONTEXT.lock();

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
    let block_dev = loop_dev.as_dyn_block_device().map_err(|e| {
        warn!("mount_ext4: loop device has no backing file: {:?}", e);
        AxError::Io
    })?;

    let num_blocks = block_dev.num_blocks();
    let region = ax_driver::PartitionRegion::from_num_blocks(num_blocks);

    // Create ext4 filesystem from the dynamic block device
    let fs = ax_fs::new_filesystem_from_dyn(block_dev, region).map_err(|e| {
        warn!("mount_ext4: failed to create ext4 filesystem: {:?}", e);
        AxError::Io
    })?;

    // Mount at the target location
    let target_loc = ctx.resolve(target)?;
    target_loc.mount(&fs).map_err(|e| {
        warn!("mount_ext4: failed to mount at {:?}: {:?}", target, e);
        AxError::Io
    })?;

    Ok(())
}

pub fn sys_umount2(target: *const c_char, _flags: i32) -> AxResult<isize> {
    let target = vm_load_string(target)?;
    debug!("sys_umount2 <= target: {target:?}");
    let target = FS_CONTEXT.lock().resolve(target)?;
    target.unmount()?;
    Ok(0)
}

pub fn sys_pivot_root(new_root: *const c_char, put_old: *const c_char) -> AxResult<isize> {
    let new_root = vm_load_string(new_root)?;
    let put_old = vm_load_string(put_old)?;
    debug!(
        "sys_pivot_root <= new_root: {:?}, put_old: {:?}",
        new_root, put_old
    );

    // Validate: put_old must be under new_root
    if !put_old.starts_with(&new_root) {
        return Err(AxError::InvalidInput);
    }
    // new_root cannot be "/"
    if new_root == "/" {
        return Err(AxError::InvalidInput);
    }

    let mut ctx = FS_CONTEXT.lock();

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

    // Capture the old root mountpoint BEFORE the pivot, so that we can
    // propagate the change to every other task afterwards (Linux
    // chroot_fs_refs semantics).
    let old_root_mp = ctx.root_dir().mountpoint().clone();

    // Perform pivot: swap the root mount (updates this task's FsContext).
    ctx.pivot_root(new_root_loc, put_old_loc)?;

    let new_root_loc = ctx.root_dir().clone();
    drop(ctx); // Release this task's lock before touching others.

    // Propagate root / cwd to all other tasks whose root_dir or current_dir
    // still points at the old root mountpoint — mirroring Linux
    // chroot_fs_refs() in fs/namespace.c.
    ax_fs::FsContext::propagate_pivot_root(&old_root_mp, &new_root_loc);

    Ok(0)
}
