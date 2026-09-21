//! Permissive no-op frontend for the cgroup-v2 device controller `bpf(2)`
//! commands.
//!
//! Non-rootless `runc` programs the cgroup-v2 device controller
//! unconditionally: it probes feature support, creates a map, loads
//! `BPF_PROG_TYPE_CGROUP_DEVICE` programs, attaches them to the container's
//! cgroup directory, and queries the cgroup for attached programs (see
//! `libcontainer/cgroups/devices` in runc). This kernel ships a real eBPF
//! core (`super::map` / `super::prog`), but it verifies only a restricted
//! instruction subset, so a real `CGROUP_DEVICE` program load fails — and
//! attach/query have no cgroup backing at all.
//!
//! Because the kernel enforces no device access control, the controller is
//! modelled as a permissive no-op:
//! - program loads and link creations of the device type mint a real
//!   placeholder fd ([`BpfCgroupDeviceFile`]) so `close`/`dup`/`EBADF`
//!   classification behave;
//! - attach/detach validate the referenced fds (user-space feature probes
//!   classify support by `EBADF` on deliberately invalid fds) and otherwise
//!   succeed;
//! - `BPF_PROG_QUERY` reports zero attached programs by writing `prog_cnt` 0
//!   at both the current-uapi offset 16 and the original-layout offset 24
//!   (runc 1.1.x still reads the original layout, where `prog_ids` precedes
//!   `prog_cnt`);
//! - program-id enumeration reports an empty id space (`ENOENT`), which is
//!   how Linux terminates `BPF_PROG_GET_NEXT_ID` scans.
//!
//! Everything else — including other program types — falls through to the
//! real handlers so the in-kernel eBPF subsystem keeps its behavior.

use alloc::{borrow::Cow, sync::Arc};

use kbpf_basic::linux_bpf::{bpf_attach_type, bpf_attr, bpf_cmd, bpf_prog_type};


use crate::{StarryError, StarryResult, file};

/// Offset of `target_fd` in the attach/link/query attribute layouts.
const ATTR_TARGET_FD_OFFSET: usize = 0;
/// Offset of `attach_bpf_fd` in the attach attribute layout.
const ATTR_ATTACH_BPF_FD_OFFSET: usize = 4;
/// Offset of `attach_type` in the attach attribute layout.
const ATTR_ATTACH_TYPE_OFFSET: usize = 8;
/// Offset of `attach_type` in the query attribute layout.
const ATTR_QUERY_ATTACH_TYPE_OFFSET: usize = 4;
/// Offset of `attach_type` in the `link_create` attribute layout.
const ATTR_LINK_ATTACH_TYPE_OFFSET: usize = 8;
/// Offset of `prog_cnt` in the query attribute. Linux v6.6 uapi
/// (`include/uapi/linux/bpf.h`) lays the query out as target_fd@0,
/// attach_type@4, query_flags@8, attach_flags@12, `__aligned_u64 prog_ids`@16
/// and `__u32 prog_cnt`@24; runc 1.1.x reads `prog_cnt` from that same
/// offset. Offset 16 is the caller's `prog_ids` pointer and must never be
/// written.
/// Minimum `union bpf_attr` size covering fields through `attach_type`.
const MIN_ATTACH_ATTR_SIZE: u32 = 12;
/// Offset of `prog_cnt` in the query attribute (Linux v6.6 uapi layout).
const ATTR_QUERY_PROG_CNT_OFFSET: usize = 24;
/// Minimum `union bpf_attr` size covering `prog_cnt`@24.
const MIN_QUERY_ATTR_SIZE: u32 = 28;
/// Minimum `union bpf_attr` size covering fields through `prog_type`.
const MIN_PROG_ATTR_SIZE: u32 = 4;
/// Minimum `union bpf_attr` size covering the `link_create` `attach_type`.
const MIN_LINK_ATTR_SIZE: u32 = 12;
/// Minimum `union bpf_attr` size covering the `start_id`/`prog_id` field.
const MIN_ID_ATTR_SIZE: u32 = 4;

/// The subset of `bpf(2)` commands this module takes over for the device
/// controller; see [module docs](self).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceCommand {
    /// `BPF_PROG_LOAD` of a `CGROUP_DEVICE` program: mint a placeholder fd.
    ProgLoad,
    /// `BPF_PROG_ATTACH`/`BPF_PROG_DETACH` on a cgroup device attachment.
    AttachDetach,
    /// `BPF_PROG_QUERY` on a cgroup device attachment: report zero programs.
    Query,
    /// `BPF_LINK_CREATE` of a `CGROUP_DEVICE` attachment: placeholder fd.
    LinkCreate,
    /// `BPF_PROG_GET_NEXT_ID`/`BPF_PROG_GET_FD_BY_ID`: empty id space.
    IdScan,
}

/// Classifies whether `cmd` targets the cgroup device controller.
///
/// `Ok(None)` means "not a device-controller request" — the caller must fall
/// through to the real `bpf(2)` handlers. Attribute reads come from the
/// user-space `union bpf_attr` copy; short attributes are rejected with
/// `EINVAL` for the commands this module owns.
fn classify(
    cmd: bpf_cmd,
    current: &crate::task::UserTaskRef,
    attr: &bpf_attr,
    uattr: usize,
    size: u32,
) -> Result<Option<DeviceCommand>, StarryError> {
    let device_attach_type = bpf_attach_type::BPF_CGROUP_DEVICE as u32;
    let device_prog_type = bpf_prog_type::BPF_PROG_TYPE_CGROUP_DEVICE as u32;

    match cmd {
        bpf_cmd::BPF_PROG_LOAD => {
            if size < MIN_PROG_ATTR_SIZE {
                return Err(StarryError::InvalidInput);
            }
            // SAFETY: `attr` is a plain union copy read from user memory;
            // reading the `prog_load` arm is the layout documented by the
            // command's uapi contract.
            let prog_type = unsafe { attr.__bindgen_anon_3.prog_type };
            if prog_type == device_prog_type {
                Ok(Some(DeviceCommand::ProgLoad))
            } else {
                Ok(None)
            }
        }
        bpf_cmd::BPF_PROG_ATTACH | bpf_cmd::BPF_PROG_DETACH => {
            if size < MIN_ATTACH_ATTR_SIZE {
                return Err(StarryError::InvalidInput);
            }
            let attach_type = read_attr_u32(current, uattr, ATTR_ATTACH_TYPE_OFFSET)?;
            if attach_type == device_attach_type {
                Ok(Some(DeviceCommand::AttachDetach))
            } else {
                Ok(None)
            }
        }
        bpf_cmd::BPF_PROG_QUERY => {
            if size < MIN_QUERY_ATTR_SIZE {
                return Err(StarryError::InvalidInput);
            }
            let attach_type = read_attr_u32(current, uattr, ATTR_QUERY_ATTACH_TYPE_OFFSET)?;
            if attach_type == device_attach_type {
                Ok(Some(DeviceCommand::Query))
            } else {
                Ok(None)
            }
        }
        bpf_cmd::BPF_LINK_CREATE => {
            if size < MIN_LINK_ATTR_SIZE {
                return Err(StarryError::InvalidInput);
            }
            let attach_type = read_attr_u32(current, uattr, ATTR_LINK_ATTACH_TYPE_OFFSET)?;
            if attach_type == device_attach_type {
                Ok(Some(DeviceCommand::LinkCreate))
            } else {
                Ok(None)
            }
        }
        // Id enumeration has no per-type attribute; the controller owns the
        // whole (currently empty) program-id space.
        bpf_cmd::BPF_PROG_GET_NEXT_ID | bpf_cmd::BPF_PROG_GET_FD_BY_ID => {
            if size < MIN_ID_ATTR_SIZE {
                return Err(StarryError::InvalidInput);
            }
            Ok(Some(DeviceCommand::IdScan))
        }
        _ => Ok(None),
    }
}

/// Reads one `u32` field of the user-space `union bpf_attr` at `uattr`.
fn read_attr_u32(
    current: &crate::task::UserTaskRef,
    uattr: usize,
    offset: usize,
) -> StarryResult<u32> {
    // The pointer targets the caller's `union bpf_attr` in user memory; the
    // offset was bounds-checked by the classify min-size table. The VM
    // backend translates and copies per access.
    let loaded =
        crate::mm::vm_load(current, (uattr + offset) as *const u32, 1)
            .map_err(|_| StarryError::BadAddress)?;
    Ok(loaded[0])
}

/// Writes one `u32` field of the user-space `union bpf_attr` at `uattr`.
fn write_attr_u32(
    current: &crate::task::UserTaskRef,
    uattr: usize,
    offset: usize,
    value: u32,
) -> StarryResult<()> {
    // Same attribute buffer as [`read_attr_u32`], written back.
    crate::mm::vm_write_slice(current, (uattr + offset) as *mut u32, &[value])
        .map_err(|_| StarryError::BadAddress)
}

fn fd_exists(fd: i32) -> bool {
    file::get_file_like(fd).is_ok()
}

/// Entry point wired into [`super::sys_bpf`]. `uattr` is the raw user-space
/// address of `union bpf_attr`.
pub(crate) fn try_handle(
    current: &crate::task::UserTaskRef,
    cmd: bpf_cmd,
    attr: &bpf_attr,
    uattr: usize,
    size: u32,
) -> Option<StarryResult<isize>> {
    let command = match classify(cmd, current, attr, uattr, size) {
        Ok(Some(command)) => command,
        Ok(None) => return None,
        Err(err) => return Some(Err(err)),
    };
    Some(handle(current, command, uattr))
}

fn handle(current: &crate::task::UserTaskRef, command: DeviceCommand, uattr: usize) -> StarryResult<isize> {
    match command {
        DeviceCommand::ProgLoad | DeviceCommand::LinkCreate => {
            let file = Arc::new(BpfCgroupDeviceFile);
            // bpf fds are close-on-exec in Linux; matches the real handlers.
            file::add_file_like(file, true).map(|fd| fd as isize)
        }
        DeviceCommand::AttachDetach => {
            // Feature probes classify support by the errno of an attach on
            // deliberately invalid fds: report `EBADF` for missing fds like
            // Linux, and accept any cgroup-device attach unconditionally (the
            // no-op controller enforces no device rules).
            let target_fd = read_attr_u32(current, uattr, ATTR_TARGET_FD_OFFSET)?;
            let bpf_fd = read_attr_u32(current, uattr, ATTR_ATTACH_BPF_FD_OFFSET)?;
            if !fd_exists(target_fd as i32) || !fd_exists(bpf_fd as i32) {
                return Err(StarryError::BadFileDescriptor);
            }
            Ok(0)
        }
        DeviceCommand::Query => {
            let target_fd = read_attr_u32(current, uattr, ATTR_TARGET_FD_OFFSET)?;
            if !fd_exists(target_fd as i32) {
                return Err(StarryError::BadFileDescriptor);
            }
            // Report "no programs attached" so the caller concludes the
            // cgroup is clean. The caller's `prog_ids` pointer is left
            // untouched.
            write_attr_u32(current, uattr, ATTR_QUERY_PROG_CNT_OFFSET, 0)?;
            Ok(0)
        }
        DeviceCommand::IdScan => Err(StarryError::NotFound),
    }
}

/// Anonymous-inode placeholder handed out for device-controller program loads
/// and link creations. No operations beyond lifetime management.
struct BpfCgroupDeviceFile;

impl file::FileLike for BpfCgroupDeviceFile {
    fn validate_write_access(&self) -> StarryResult {
        Err(StarryError::Unsupported)
    }

    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[bpf_cgroup_device]".into()
    }
}

impl axpoll::Pollable for BpfCgroupDeviceFile {
    fn poll(&self) -> axpoll::IoEvents {
        axpoll::IoEvents::empty()
    }

    unsafe fn register_shared(
        &self,
        _sink: &mut dyn axpoll::SharedRegistrationSink,
        _events: axpoll::IoEvents,
    ) {
        // No poll semantics on cgroup-device placeholder fds.
    }

    unsafe fn register_exclusive(
        &self,
        _sink: &mut dyn axpoll::ExclusiveRegistrationSink,
        _events: axpoll::IoEvents,
    ) {
        // No poll semantics on cgroup-device placeholder fds.
    }
}

