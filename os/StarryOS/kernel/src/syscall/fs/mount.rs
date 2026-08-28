use alloc::{borrow::Cow, string::String, sync::Arc, vec::Vec};
use core::{
    ffi::{c_char, c_void},
    task::Context,
};

use ax_fs_ng::vfs::is_mount_busy as fs_is_mount_busy;
use ax_task::current;
use axfs_ng_vfs::{Filesystem, MetadataUpdate, Mountpoint, NodePermission};
use axpoll::{IoEvents, Pollable};
use linux_raw_sys::general::{
    AT_EMPTY_PATH, FSMOUNT_CLOEXEC, FSOPEN_CLOEXEC, MOUNT_ATTR__ATIME, MOUNT_ATTR_NOATIME,
    MOUNT_ATTR_NODEV, MOUNT_ATTR_NOEXEC, MOUNT_ATTR_NOSUID, MOUNT_ATTR_RDONLY,
    MOUNT_ATTR_STRICTATIME, MOVE_MOUNT_F_EMPTY_PATH, O_PATH, fsconfig_command,
};
use starry_vm::VmPtr;

use crate::{
    Errno, StarryError, StarryResult,
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
    sync::Mutex,
    task::{AsThread, tasks},
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

const SUPPORTED_FSMOUNT_ATTRIBUTES: u32 =
    MOUNT_ATTR_RDONLY | MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV | MOUNT_ATTR_NOEXEC;
const MOUNT_ATTR_SIZE_VER0: usize = core::mem::size_of::<MountAttr>();
const SUPPORTED_MOUNT_SETATTR_ATTRIBUTES: u64 = (MOUNT_ATTR_RDONLY
    | MOUNT_ATTR_NOSUID
    | MOUNT_ATTR_NODEV
    | MOUNT_ATTR_NOEXEC
    | MOUNT_ATTR__ATIME) as u64;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::AnyBitPattern)]
pub struct MountAttr {
    attr_set: u64,
    attr_clr: u64,
    propagation: u64,
    userns_fd: u64,
}

fn parse_devpts_mode(value: &str) -> StarryResult<NodePermission> {
    let mode = u16::from_str_radix(value, 8).map_err(|_| StarryError::InvalidInput)?;
    NodePermission::from_bits(mode).ok_or(StarryError::InvalidInput)
}

enum DevPtsInstanceKind {
    Legacy,
    New,
}

fn parse_devpts_options(data: *const c_void) -> StarryResult<DevPtsMount> {
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
        let (key, value) = item.split_once('=').ok_or(StarryError::InvalidInput)?;
        match key {
            "mode" => options.slave_mode = parse_devpts_mode(value)?,
            "gid" => {
                options.slave_gid = value.parse().map_err(|_| StarryError::InvalidInput)?;
            }
            "ptmxmode" => options.ptmx_mode = parse_devpts_mode(value)?,
            _ => return Err(StarryError::InvalidInput),
        }
    }
    Ok(match instance {
        DevPtsInstanceKind::Legacy => DevPtsMount::Legacy(options),
        DevPtsInstanceKind::New => DevPtsMount::NewInstance(options),
    })
}

fn parse_overlay_options(
    data: *const c_void,
) -> StarryResult<(Vec<String>, Option<String>, Option<String>)> {
    if data.is_null() {
        return Err(StarryError::InvalidInput);
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
                return Err(StarryError::OperationNotSupported);
            }
            _ => {}
        }
    }

    let lower_dirs = lowerdir
        .ok_or(StarryError::InvalidInput)?
        .split(':')
        .filter(|path| !path.is_empty())
        .map(String::from)
        .collect::<Vec<_>>();
    if lower_dirs.is_empty() {
        return Err(StarryError::InvalidInput);
    }

    if upperdir.is_some() != workdir.is_some() {
        return Err(StarryError::InvalidInput);
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum MountContextKind {
    Tmpfs,
    Ramfs,
    DevPts,
}

struct MountContextState {
    filesystem: Option<Filesystem>,
    source: Option<String>,
    root_mode: NodePermission,
    devpts_options: DevPtsOptions,
    tmpfs_size_limit: Option<u64>,
    unsupported_tmpfs_limits: bool,
    readonly_reconfigure: bool,
    mounts: Vec<Arc<Mountpoint>>,
}

struct MountContext {
    kind: MountContextKind,
    state: Mutex<MountContextState>,
}

impl MountContext {
    fn new(kind: MountContextKind) -> Self {
        Self {
            kind,
            state: Mutex::new(MountContextState {
                filesystem: None,
                source: None,
                root_mode: NodePermission::from_bits_truncate(0o755),
                devpts_options: DevPtsOptions::mounted(),
                tmpfs_size_limit: None,
                unsupported_tmpfs_limits: false,
                readonly_reconfigure: false,
                mounts: Vec::new(),
            }),
        }
    }
}

impl FileLike for MountContext {
    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[fscontext]".into()
    }
}

impl Pollable for MountContext {
    fn poll(&self) -> IoEvents {
        IoEvents::empty()
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {}
}

fn parse_tmpfs_size(value: &str) -> StarryResult<u64> {
    const PAGE_SIZE: u64 = 4096;

    let bytes = if let Some(percent) = value.strip_suffix('%') {
        let percent = percent
            .parse::<u64>()
            .map_err(|_| StarryError::InvalidInput)?;
        if percent == 0 || percent > 100 {
            return Err(StarryError::InvalidInput);
        }
        let total_pages = (ax_runtime::hal::mem::total_ram_size() as u64).div_ceil(PAGE_SIZE);
        total_pages
            .checked_mul(percent)
            .ok_or(StarryError::InvalidInput)?
            .div_ceil(100)
            .checked_mul(PAGE_SIZE)
            .ok_or(StarryError::InvalidInput)?
    } else {
        let (number, multiplier) = match value.as_bytes().last().copied() {
            Some(b'k' | b'K') => (&value[..value.len() - 1], 1_u64 << 10),
            Some(b'm' | b'M') => (&value[..value.len() - 1], 1_u64 << 20),
            Some(b'g' | b'G') => (&value[..value.len() - 1], 1_u64 << 30),
            Some(b't' | b'T') => (&value[..value.len() - 1], 1_u64 << 40),
            Some(b'p' | b'P') => (&value[..value.len() - 1], 1_u64 << 50),
            Some(b'e' | b'E') => (&value[..value.len() - 1], 1_u64 << 60),
            _ => (value, 1),
        };
        number
            .parse::<u64>()
            .map_err(|_| StarryError::InvalidInput)?
            .checked_mul(multiplier)
            .ok_or(StarryError::InvalidInput)?
    };

    if bytes == 0 {
        return Err(StarryError::InvalidInput);
    }
    bytes
        .checked_add(PAGE_SIZE - 1)
        .map(|size| size / PAGE_SIZE * PAGE_SIZE)
        .ok_or(StarryError::InvalidInput)
}

pub fn sys_fsopen(fs_name: *const c_char, flags: u32) -> StarryResult<isize> {
    if flags & !FSOPEN_CLOEXEC != 0 {
        return Err(StarryError::InvalidInput);
    }
    if !current().as_thread().cred().has_cap_sys_admin() {
        return Err(StarryError::OperationNotPermitted);
    }

    let kind = match vm_load_string(fs_name)?.as_str() {
        "tmpfs" => MountContextKind::Tmpfs,
        "ramfs" => MountContextKind::Ramfs,
        "devpts" => MountContextKind::DevPts,
        _ => return Err(StarryError::NoSuchDevice),
    };
    MountContext::new(kind)
        .add_to_fd_table(flags & FSOPEN_CLOEXEC != 0)
        .map(|fd| fd as isize)
}

pub fn sys_fsconfig(
    fs_fd: i32,
    command: u32,
    key: *const c_char,
    value: *const c_void,
    aux: i32,
) -> StarryResult<isize> {
    if !current().as_thread().cred().has_cap_sys_admin() {
        return Err(StarryError::OperationNotPermitted);
    }
    let context = MountContext::from_fd(fs_fd)?;
    let mut state = context.state.lock();

    match command {
        command if command == fsconfig_command::FSCONFIG_SET_STRING as u32 => {
            if key.is_null() || value.is_null() || aux != 0 || state.filesystem.is_some() {
                return Err(StarryError::InvalidInput);
            }
            let key = vm_load_string(key)?;
            let value = vm_load_string(value.cast())?;
            match (context.kind, key.as_str()) {
                (_, "source") if state.filesystem.is_none() && !value.is_empty() => {
                    state.source = Some(value);
                }
                (MountContextKind::Tmpfs, "size") if !value.is_empty() => {
                    state.tmpfs_size_limit = Some(parse_tmpfs_size(&value)?);
                }
                (MountContextKind::Tmpfs, "nr_inodes") if !value.is_empty() => {
                    state.unsupported_tmpfs_limits = true;
                }
                (MountContextKind::Tmpfs | MountContextKind::Ramfs, "mode") => {
                    state.root_mode = parse_devpts_mode(&value)?;
                }
                (MountContextKind::DevPts, "mode") => {
                    state.devpts_options.slave_mode = parse_devpts_mode(&value)?;
                }
                (MountContextKind::DevPts, "gid") => {
                    state.devpts_options.slave_gid =
                        value.parse().map_err(|_| StarryError::InvalidInput)?;
                }
                (MountContextKind::DevPts, "ptmxmode") => {
                    state.devpts_options.ptmx_mode = parse_devpts_mode(&value)?;
                }
                _ => return Err(StarryError::InvalidInput),
            }
        }
        command if command == fsconfig_command::FSCONFIG_SET_FLAG as u32 => {
            if key.is_null() || !value.is_null() || aux != 0 {
                return Err(StarryError::InvalidInput);
            }
            match vm_load_string(key)?.as_str() {
                // Linux systemd deliberately falls back from tmpfs to ramfs
                // when the kernel cannot configure tmpfs with `noswap`.
                "noswap"
                    if context.kind == MountContextKind::Tmpfs && state.filesystem.is_none() =>
                {
                    return Err(StarryError::InvalidInput);
                }
                // A devpts filesystem context already denotes a fresh instance;
                // retain Linux's accepted option spelling without adding a
                // second instance-selection state.
                "newinstance"
                    if context.kind == MountContextKind::DevPts && state.filesystem.is_none() => {}
                "ro" if state.filesystem.is_some() => state.readonly_reconfigure = true,
                _ => return Err(StarryError::InvalidInput),
            }
        }
        command if command == fsconfig_command::FSCONFIG_CMD_CREATE as u32 => {
            if !key.is_null() || !value.is_null() || aux != 0 || state.filesystem.is_some() {
                return Err(StarryError::InvalidInput);
            }
            if context.kind == MountContextKind::Tmpfs && state.unsupported_tmpfs_limits {
                return Err(StarryError::OperationNotSupported);
            }
            state.filesystem = Some(match context.kind {
                MountContextKind::Tmpfs => state
                    .tmpfs_size_limit
                    .map_or_else(MemoryFs::new, MemoryFs::new_with_size_limit),
                MountContextKind::Ramfs => MemoryFs::new_ramfs(),
                MountContextKind::DevPts => {
                    new_devptsfs(DevPtsMount::NewInstance(state.devpts_options))
                }
            });
        }
        command if command == fsconfig_command::FSCONFIG_CMD_RECONFIGURE as u32 => {
            if !key.is_null()
                || !value.is_null()
                || aux != 0
                || state.filesystem.is_none()
                || !state.readonly_reconfigure
            {
                return Err(StarryError::InvalidInput);
            }
            for mountpoint in &state.mounts {
                mountpoint.set_readonly(true);
            }
            state.readonly_reconfigure = false;
        }
        _ => return Err(StarryError::OperationNotSupported),
    }
    Ok(0)
}

pub fn sys_fsmount(fs_fd: i32, flags: u32, mount_attributes: u32) -> StarryResult<isize> {
    if flags & !FSMOUNT_CLOEXEC != 0 || mount_attributes & !SUPPORTED_FSMOUNT_ATTRIBUTES != 0 {
        // systemd retries without MOUNT_ATTR_NOSYMFOLLOW on EINVAL.
        return Err(StarryError::InvalidInput);
    }
    if !current().as_thread().cred().has_cap_sys_admin() {
        return Err(StarryError::OperationNotPermitted);
    }

    let context = MountContext::from_fd(fs_fd)?;
    let mut state = context.state.lock();
    let filesystem = state
        .filesystem
        .as_ref()
        .ok_or(StarryError::InvalidInput)?
        .clone();
    let mountpoint =
        Mountpoint::new_root_with_source(&filesystem, state.source.as_deref().unwrap_or("none"));
    let root = mountpoint.root_location();
    if context.kind != MountContextKind::DevPts {
        root.update_metadata(MetadataUpdate {
            mode: Some(state.root_mode),
            ..Default::default()
        })?;
    }
    mountpoint.set_readonly(mount_attributes & MOUNT_ATTR_RDONLY != 0);
    mountpoint.set_mount_flags(
        mount_attributes & (MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV | MOUNT_ATTR_NOEXEC),
    );
    state.mounts.push(mountpoint);

    Directory::new_detached_mount(root, O_PATH)
        .add_to_fd_table(flags & FSMOUNT_CLOEXEC != 0)
        .map(|fd| fd as isize)
}

pub fn sys_move_mount(
    from_dirfd: i32,
    from_path: *const c_char,
    to_dirfd: i32,
    to_path: *const c_char,
    flags: u32,
) -> StarryResult<isize> {
    if flags != MOVE_MOUNT_F_EMPTY_PATH || !vm_load_string(from_path)?.is_empty() {
        return Err(StarryError::InvalidInput);
    }
    if !current().as_thread().cred().has_cap_sys_admin() {
        return Err(StarryError::OperationNotPermitted);
    }

    let source = Directory::from_fd(from_dirfd)?;
    if !source.is_detached_mount_handle() {
        return Err(StarryError::InvalidInput);
    }
    let path = vm_load_string(to_path)?;
    let fs_context = ax_fs_ng::vfs::current_fs_context();
    let mount_namespace = fs_context.lock().mount_namespace().clone();
    let target = if path.starts_with('/') {
        fs_context.lock().resolve(&path)?
    } else {
        crate::file::with_fs(to_dirfd, |fs| Ok(fs.resolve(&path)?))?
    };
    source.inner().mountpoint().attach_detached(&target)?;
    crate::file::notify_mount_namespace_changed(&mount_namespace);
    Ok(0)
}

/// Apply the Linux VFS attributes currently required by util-linux mounts.
///
/// The supported boundary is an existing mount root referenced by a directory
/// fd and an empty path. Mount propagation, idmapped mounts, recursive changes,
/// and `MOUNT_ATTR_NOSYMFOLLOW` remain explicit `EINVAL` paths.
pub fn sys_mount_setattr(
    dirfd: i32,
    path: *const c_char,
    flags: u32,
    attributes: *const MountAttr,
    size: usize,
) -> StarryResult<isize> {
    // Linux reserves the all-zero request as a side-effect-free syscall
    // availability probe. util-linux uses it before choosing the new mount API.
    if flags == 0 && size == 0 {
        return Ok(0);
    }
    if size != MOUNT_ATTR_SIZE_VER0 || flags != AT_EMPTY_PATH {
        return Err(StarryError::InvalidInput);
    }
    if !current().as_thread().cred().has_cap_sys_admin() {
        return Err(StarryError::OperationNotPermitted);
    }
    if !vm_load_string(path)?.is_empty() {
        return Err(StarryError::InvalidInput);
    }

    let attributes = attributes.vm_read()?;
    validate_mount_attributes(&attributes)?;

    let directory = Directory::from_fd(dirfd)?;
    if !directory.inner().is_root_of_mount() {
        return Err(StarryError::InvalidInput);
    }
    let fs_context = ax_fs_ng::vfs::current_fs_context();
    let mount_namespace = fs_context.lock().mount_namespace().clone();
    let is_visible = mount_namespace
        .walk_tree()
        .into_iter()
        .any(|(_, _, mountpoint)| Arc::ptr_eq(&mountpoint, directory.inner().mountpoint()));
    apply_mount_attributes(directory.inner().mountpoint(), &attributes);
    if is_visible {
        crate::file::notify_mount_namespace_changed(&mount_namespace);
    }
    Ok(0)
}

fn validate_mount_attributes(attributes: &MountAttr) -> StarryResult<()> {
    if attributes.propagation != 0
        || attributes.userns_fd != 0
        || attributes.attr_set & !SUPPORTED_MOUNT_SETATTR_ATTRIBUTES != 0
        || attributes.attr_clr & !SUPPORTED_MOUNT_SETATTR_ATTRIBUTES != 0
    {
        return Err(StarryError::InvalidInput);
    }

    let non_atime_attributes = !(MOUNT_ATTR__ATIME as u64);
    if attributes.attr_set & attributes.attr_clr & non_atime_attributes != 0 {
        return Err(StarryError::InvalidInput);
    }

    let requested_atime = attributes.attr_set & MOUNT_ATTR__ATIME as u64;
    if requested_atime != 0
        && requested_atime != MOUNT_ATTR_NOATIME as u64
        && requested_atime != MOUNT_ATTR_STRICTATIME as u64
    {
        return Err(StarryError::InvalidInput);
    }
    Ok(())
}

fn apply_mount_attributes(mountpoint: &Arc<Mountpoint>, attributes: &MountAttr) {
    if attributes.attr_set & MOUNT_ATTR_RDONLY as u64 != 0 {
        mountpoint.set_readonly(true);
    } else if attributes.attr_clr & MOUNT_ATTR_RDONLY as u64 != 0 {
        mountpoint.set_readonly(false);
    }

    let basic_attributes = (MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV | MOUNT_ATTR_NOEXEC) as u64;
    let mut mount_flags = mountpoint.mount_flags();
    mount_flags |= (attributes.attr_set & basic_attributes) as u32;
    mount_flags &= !(attributes.attr_clr & basic_attributes) as u32;

    if (attributes.attr_set | attributes.attr_clr) & MOUNT_ATTR__ATIME as u64 != 0 {
        mount_flags &= !(MS_NOATIME | MS_RELATIME | MS_STRICTATIME) as u32;
        match attributes.attr_set & MOUNT_ATTR__ATIME as u64 {
            value if value == MOUNT_ATTR_NOATIME as u64 => mount_flags |= MS_NOATIME as u32,
            value if value == MOUNT_ATTR_STRICTATIME as u64 => {
                mount_flags |= MS_STRICTATIME as u32;
            }
            _ => mount_flags |= MS_RELATIME as u32,
        }
    }
    mountpoint.set_mount_flags(mount_flags);
}

pub fn sys_mount(
    source: *const c_char,
    target: *const c_char,
    fs_type: *const c_char,
    flags: i32,
    data: *const c_void,
) -> StarryResult<isize> {
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

    if !current().as_thread().cred().has_cap_sys_admin() {
        return Err(StarryError::OperationNotPermitted);
    }

    let fs_context = ax_fs_ng::vfs::current_fs_context();
    let mount_namespace = fs_context.lock().mount_namespace().clone();
    let propagation = flags & PROPAGATION_FLAGS;

    if propagation.count_ones() > 1 {
        return Err(StarryError::InvalidInput);
    }

    if propagation != 0 {
        let allowed = propagation | MS_REC | MS_SILENT;
        if flags & !allowed != 0 {
            return Err(StarryError::InvalidInput);
        }

        let target = ax_fs_ng::vfs::current_fs_context().lock().resolve(target)?;
        if !target.is_root_of_mount() {
            return Err(StarryError::InvalidInput);
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
        crate::file::notify_mount_namespace_changed(&mount_namespace);
        return Ok(0);
    }

    if (flags & MS_REMOUNT) != 0 {
        let target = ax_fs_ng::vfs::current_fs_context().lock().resolve(target)?;
        if !target.is_root_of_mount() {
            return Err(StarryError::InvalidInput);
        }
        let mp = target.mountpoint();
        mp.set_readonly((flags & MS_RDONLY) != 0);
        mp.set_mount_flags((flags & MOUNT_OPTION_FLAGS) as u32);
        crate::file::notify_mount_namespace_changed(&mount_namespace);
        return Ok(0);
    }

    if (flags & MS_MOVE) != 0 {
        let ctx = fs_context.lock();
        let source = ctx.resolve(source)?;
        let target = ctx.resolve(target)?;
        source.move_mount(&target)?;
        drop(ctx);
        crate::file::notify_mount_namespace_changed(&mount_namespace);
        return Ok(0);
    }

    if (flags & MS_BIND) != 0 {
        let ctx = fs_context.lock();
        let source = ctx.resolve(source)?;
        let target = ctx.resolve(target)?;
        target.bind_mount(&source, (flags & MS_REC) != 0)?;
        drop(ctx);
        crate::file::notify_mount_namespace_changed(&mount_namespace);
        return Ok(0);
    }

    match fs_type.as_str() {
        "proc" => {
            let fs =
                crate::pseudofs::proc::new_procfs(current().as_thread().active_pid_namespace());
            let target = ax_fs_ng::vfs::current_fs_context().lock().resolve(target)?;
            let mp = target.mount_with_source(&fs, mount_source(&source))?;
            if (flags & MS_RDONLY) != 0 {
                mp.set_readonly(true);
            }
            mp.set_mount_flags((flags & MOUNT_OPTION_FLAGS) as u32);
        }
        "sysfs" | "devtmpfs" | "tmpfs" => {
            let fs = MemoryFs::new();
            let target = ax_fs_ng::vfs::current_fs_context().lock().resolve(target)?;
            let mp = target.mount_with_source(&fs, mount_source(&source))?;
            if (flags & MS_RDONLY) != 0 {
                mp.set_readonly(true);
            }
            mp.set_mount_flags((flags & MOUNT_OPTION_FLAGS) as u32);
        }
        "ramfs" => {
            // Linux registers ramfs as a separate nodev filesystem and exposes
            // RAMFS_MAGIC through statfs. It shares the in-memory inode/data
            // machinery here, but must not inherit tmpfs's visible identity.
            let fs = MemoryFs::new_ramfs();
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
                let task = current();
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
        _ => return Err(StarryError::NoSuchDevice),
    }

    crate::file::notify_mount_namespace_changed(&mount_namespace);
    Ok(0)
}

fn mount_source(source: &str) -> &str {
    if source.is_empty() { "none" } else { source }
}

#[cfg(feature = "ext4")]
fn mount_ext4(source: &str, _target: &str, _readonly: bool) -> StarryResult<()> {
    // The old loop-backed ext4 adapter implemented the removed synchronous
    // polling queue API. Keep its source for the later virtual-device
    // migration, but do not expose it through mount(2) as an IRQ-capable
    // device. Linux uses ENODEV when the requested filesystem/device backend
    // is not available in the running kernel.
    warn!(
        "mount_ext4: block backend for source {:?} has not been migrated",
        source
    );
    Err(StarryError::NoSuchDevice)
}

pub fn sys_umount2(target: *const c_char, flags: i32) -> StarryResult<isize> {
    use alloc::boxed::Box;

    let target = vm_load_string(target)?;
    debug!("sys_umount2 <= target: {target:?}, flags: {flags:#x}");

    if (flags & !VALID_UMOUNT_FLAGS) != 0 {
        return Err(StarryError::InvalidInput);
    }

    if (flags & MNT_EXPIRE) != 0 && (flags & (MNT_FORCE | MNT_DETACH)) != 0 {
        return Err(StarryError::InvalidInput);
    }

    if target.is_empty() {
        return Err(StarryError::NotFound);
    }

    let fs_context = ax_fs_ng::vfs::current_fs_context();
    let mount_namespace = fs_context.lock().mount_namespace().clone();
    let target = if (flags & UMOUNT_NOFOLLOW) != 0 {
        fs_context.lock().resolve_no_follow(target)?
    } else {
        fs_context.lock().resolve(target)?
    };

    if !current().as_thread().cred().has_cap_sys_admin() {
        return Err(StarryError::OperationNotPermitted);
    }

    // Linux umount2 returns EINVAL for paths that are not mount points.
    if !target.is_root_of_mount() {
        return Err(StarryError::InvalidInput);
    }

    if (flags & MNT_EXPIRE) != 0 && !target.mountpoint().mark_expired() {
        return Err(StarryError::from(Errno::EAGAIN));
    }

    if (flags & MNT_DETACH) != 0 {
        target.detach_mount()?;
        crate::file::notify_mount_namespace_changed(&mount_namespace);
        return Ok(0);
    }

    let plan = target
        .mountpoint()
        .plan_unmount(axfs_ng_vfs::UnmountKind::Normal)?;
    if plan.targets().any(is_mount_busy) {
        return Err(StarryError::from(Errno::EBUSY));
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
        ud.get::<Box<dyn Fn() -> StarryResult<()> + Send + Sync>>()
    }; // user_data lock released

    if plan.targets().any(is_mount_busy) {
        return Err(StarryError::from(Errno::EBUSY));
    }
    target.commit_unmount(plan)?;
    crate::file::notify_mount_namespace_changed(&mount_namespace);

    // After unmount, filesystem block I/O has stopped; it is safe to do VFS
    // writeback here. Propagate writeback errors so userspace sees EIO when
    // dirty data could not be persisted to the backing file.
    if let Some(cb) = writeback {
        cb()?;
    }

    Ok(0)
}

pub fn sys_pivot_root(new_root: *const c_char, put_old: *const c_char) -> StarryResult<isize> {
    let new_root = vm_load_string(new_root)?;
    let put_old = vm_load_string(put_old)?;
    debug!(
        "sys_pivot_root <= new_root: {:?}, put_old: {:?}",
        new_root, put_old
    );

    if !current().as_thread().cred().has_cap_sys_admin() {
        return Err(StarryError::OperationNotPermitted);
    }

    let fs_context = ax_fs_ng::vfs::current_fs_context();
    let mut ctx = fs_context.lock();

    // The caller's current root must itself be a mount point (Linux
    // EINVAL if e.g. the process chroot'd into a subdirectory).
    if !ctx.root_dir().is_root_of_mount() {
        return Err(StarryError::InvalidInput);
    }

    // Resolve both paths before checking their VFS relationship. Linux permits
    // callers to enter new_root and use pivot_root(".", "old").
    let new_root_loc = ctx.resolve(&new_root)?;
    new_root_loc.check_is_dir()?;
    let put_old_loc = ctx.resolve(&put_old)?;
    put_old_loc.check_is_dir()?;

    if !put_old_loc.is_descendant_of(&new_root_loc) {
        return Err(StarryError::InvalidInput);
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
        return Err(StarryError::InvalidInput);
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
    crate::file::notify_mount_namespace_changed(&mount_namespace);

    Ok(0)
}

#[cfg(all(test, not(axtest)))]
fn mount_flags_validation_rules_hold_for_test() -> bool {
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

    const {
        assert!(MS_SHARED & PROPAGATION_FLAGS != 0);
        assert!(MS_PRIVATE & PROPAGATION_FLAGS != 0);
        assert!(MS_SLAVE & PROPAGATION_FLAGS != 0);
        assert!(MS_UNBINDABLE & PROPAGATION_FLAGS != 0);
    }

    true
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn mount_flags_validation_rules_hold() {
        assert!(super::mount_flags_validation_rules_hold_for_test());
    }
}
