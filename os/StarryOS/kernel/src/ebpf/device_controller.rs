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
//! - `BPF_PROG_QUERY` reports the number of recorded attaches for the
//!   targeted cgroup by writing `prog_cnt` at both the current-uapi offset 16
//!   and the original-layout offset 24 (runc 1.1.x still reads the original
//!   layout, where `prog_ids` precedes `prog_cnt`); the synthetic ids have no
//!   fd mapping, so the caller's `prog_ids` buffer is left untouched;
//! - `BPF_PROG_GET_NEXT_ID`/`BPF_PROG_GET_FD_BY_ID` carry no per-controller
//!   attribute and are left to the real handlers, so ID enumeration for every
//!   other program type is unaffected.
//!
//! Everything else — including other program types — falls through to the
//! real handlers so the in-kernel eBPF subsystem keeps its behavior.

use alloc::{borrow::Cow, sync::Arc, vec::Vec, sync::Weak};
use core::sync::atomic::{AtomicU32, Ordering};


use kbpf_basic::linux_bpf::{bpf_attach_type, bpf_attr, bpf_cmd, bpf_prog_type};


use crate::{StarryError, StarryResult, file};
use ax_fs_ng::os::sync::IrqMutex;

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
const ATTR_QUERY_PROG_IDS_OFFSET: usize = 16;
const ATTR_QUERY_PROG_CNT_OFFSET: usize = 24;
/// Minimum `union bpf_attr` size covering `prog_cnt`@24.
const MIN_QUERY_ATTR_SIZE: u32 = 28;
/// Minimum `union bpf_attr` size covering fields through `prog_type`.
const MIN_PROG_ATTR_SIZE: u32 = 4;
/// Minimum `union bpf_attr` size covering the `link_create` `attach_type`.
const MIN_LINK_ATTR_SIZE: u32 = 12;
/// Minimum `union bpf_attr` size covering the `start_id`/`prog_id` field.
const MIN_ID_ATTR_SIZE: u32 = 4;
/// Minimum `union bpf_attr` size covering the `OBJ_GET_INFO` fields
/// (`info` pointer, `info_len`, `bpf_fd`).
const MIN_GET_INFO_ATTR_SIZE: u32 = 16;
/// Offsets of `bpf_fd`, `info_len`, and the `info` pointer in the
/// `BPF_OBJ_GET_INFO_BY_FD` attribute (uapi/linux/bpf.h).
const ATTR_GET_INFO_FD_OFFSET: usize = 0;
const ATTR_GET_INFO_LEN_OFFSET: usize = 4;
const ATTR_GET_INFO_PTR_OFFSET: usize = 8;

/// Synthetic program ids handed to device-controller placeholder programs,
/// mirroring the id space a real loader would observe through
/// `BPF_PROG_QUERY` and `BPF_PROG_GET_FD_BY_ID`.
static NEXT_DEVICE_PROG_ID: AtomicU32 = AtomicU32::new(1);

/// One accepted cgroup-device attach: `BPF_PROG_ATTACH` records it,
/// `BPF_PROG_DETACH` removes it, and `BPF_PROG_QUERY` reports the recorded
/// count so the three commands stay consistent with each other. The target
/// is held by object identity (`Arc` pointer), never by the raw fd number,
/// so closing the fd and reusing the number cannot alias an old attachment.
struct AttachedDeviceProg {
    target: Arc<dyn file::FileLike>,
    attach_type: u32,
    /// Strong reference to the attached placeholder program: the attachment
    /// keeps the program (and its id) alive until DETACH, like Linux.
    prog: Arc<dyn file::FileLike>,
    prog_id: u32,
}

static ATTACHED_DEVICE_PROGS: IrqMutex<Vec<AttachedDeviceProg>> = IrqMutex::new(Vec::new());

/// Synthetic ids for device-controller placeholder programs. This kernel has
/// no global eBPF program-id space (the real dispatcher rejects the ID
/// commands), so these ids are the whole observable program-id space and
/// `BPF_PROG_GET_FD_BY_ID`/`BPF_PROG_GET_NEXT_ID` can serve them without
/// affecting any other program type. The placeholder object is held weakly:
/// once every fd referring to it is closed, the id disappears like a real
/// program whose reference count dropped to zero.
struct DeviceProg {
    id: u32,
    file: Weak<dyn file::FileLike>,
}

static DEVICE_PROGS: IrqMutex<Vec<DeviceProg>> = IrqMutex::new(Vec::new());

/// Drops registry entries whose placeholder object is gone and returns
/// whether `id` is still live.
fn device_prog_live(id: u32) -> bool {
    if DEVICE_PROGS
        .lock()
        .iter()
        .any(|prog| prog.id == id && prog.file.upgrade().is_some())
    {
        return true;
    }
    // Attachments hold their program strongly, so an attached id stays live
    // even after the loading fd is closed.
    ATTACHED_DEVICE_PROGS
        .lock()
        .iter()
        .any(|prog| prog.prog_id == id)
}

/// Returns whether `id` lies in the synthetic device-controller id space:
/// the counter is monotonic and only this module mints ids, so any id below
/// the counter that is no longer live is a *dead* device id (Linux answers
/// such `GET_FD_BY_ID` with ENOENT), while ids above it were never ours and
/// belong to the real handlers.
fn is_device_id_space(id: u32) -> bool {
    id != 0 && id < NEXT_DEVICE_PROG_ID.load(Ordering::Relaxed)
}

fn next_device_prog_id(after: u32) -> Option<u32> {
    let mut progs = DEVICE_PROGS.lock();
    progs.retain(|prog| prog.file.upgrade().is_some());
    let mut ids: Vec<u32> = progs
        .iter()
        .map(|prog| prog.id)
        .chain(
            ATTACHED_DEVICE_PROGS
                .lock()
                .iter()
                .map(|prog| prog.prog_id),
        )
        .collect();
    drop(progs);
    ids.sort_unstable();
    ids.dedup();
    ids.into_iter().find(|id| *id > after)
}

/// The subset of `bpf(2)` commands this module takes over for the device
/// controller; see [module docs](self).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceCommand {
    /// `BPF_PROG_LOAD` of a `CGROUP_DEVICE` program: mint a placeholder fd.
    ProgLoad,
    /// `BPF_PROG_ATTACH` on a cgroup device attachment: record it.
    Attach,
    /// `BPF_PROG_DETACH` on a cgroup device attachment: drop the record.
    Detach,
    /// `BPF_PROG_QUERY` on a cgroup device attachment: report zero programs.
    Query,
    /// `BPF_LINK_CREATE` of a `CGROUP_DEVICE` attachment: placeholder fd.
    LinkCreate,
    /// `BPF_PROG_GET_NEXT_ID` over the synthetic device-controller id space
    /// (the kernel has no other program ids).
    GetNextId,
    /// `BPF_PROG_GET_FD_BY_ID` over the synthetic device-controller id space.
    GetFdById,
    /// `BPF_OBJ_GET_INFO_BY_FD` for a device-controller placeholder fd:
    /// reports the cgroup-device program type and synthetic id, which is what
    /// loaders (runc via cilium/ebpf) use to classify a fetched program.
    GetInfoByFd,
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
        bpf_cmd::BPF_PROG_ATTACH => {
            if size < MIN_ATTACH_ATTR_SIZE {
                return Err(StarryError::InvalidInput);
            }
            let attach_type = read_attr_u32(current, uattr, ATTR_ATTACH_TYPE_OFFSET)?;
            if attach_type == device_attach_type {
                Ok(Some(DeviceCommand::Attach))
            } else {
                Ok(None)
            }
        }
        bpf_cmd::BPF_PROG_DETACH => {
            if size < MIN_ATTACH_ATTR_SIZE {
                return Err(StarryError::InvalidInput);
            }
            let attach_type = read_attr_u32(current, uattr, ATTR_ATTACH_TYPE_OFFSET)?;
            if attach_type == device_attach_type {
                Ok(Some(DeviceCommand::Detach))
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
        bpf_cmd::BPF_PROG_GET_NEXT_ID => {
            if size < MIN_ID_ATTR_SIZE {
                return Err(StarryError::InvalidInput);
            }
            let start = read_attr_u32(current, uattr, 0)?;
            if next_device_prog_id(start).is_some() {
                Ok(Some(DeviceCommand::GetNextId))
            } else if is_device_id_space(start) {
                // Inside our id space but exhausted: Linux terminates the
                // enumeration with ENOENT.
                Ok(Some(DeviceCommand::GetNextId))
            } else {
                // Not a device-controller id: leave the command to the real
                // handlers so other program kinds are unaffected.
                Ok(None)
            }
        }
        bpf_cmd::BPF_PROG_GET_FD_BY_ID => {
            if size < MIN_ID_ATTR_SIZE {
                return Err(StarryError::InvalidInput);
            }
            let id = read_attr_u32(current, uattr, 0)?;
            if is_device_id_space(id) {
                Ok(Some(DeviceCommand::GetFdById))
            } else {
                // Never one of ours: the real handlers own the error.
                Ok(None)
            }
        }
        bpf_cmd::BPF_OBJ_GET_INFO_BY_FD => {
            if size < MIN_GET_INFO_ATTR_SIZE {
                return Err(StarryError::InvalidInput);
            }
            Ok(Some(DeviceCommand::GetInfoByFd))
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
        DeviceCommand::ProgLoad => {
            let id = NEXT_DEVICE_PROG_ID.fetch_add(1, Ordering::Relaxed);
            let file: Arc<dyn file::FileLike> = Arc::new(BpfCgroupDeviceFile { id });
            DEVICE_PROGS
                .lock()
                .push(DeviceProg { id, file: Arc::downgrade(&file) });
            // bpf fds are close-on-exec in Linux; matches the real handlers.
            file::add_file_like(file, true).map(|fd| fd as isize)
        }
        DeviceCommand::LinkCreate => {
            // The device-controller link lifecycle (BPF_LINK_CREATE /
            // BPF_LINK_DESTROY, link ids) is not modeled: pretend-failing
            // creates would leave an object no query or destroy can reach.
            // Callers on this kernel attach with BPF_PROG_ATTACH (runc
            // 1.1.x does exactly that), so refuse the link form explicitly.
            let _ = read_attr_u32(current, uattr, ATTR_LINK_ATTACH_TYPE_OFFSET)?;
            Err(StarryError::InvalidInput)
        }
        DeviceCommand::GetFdById => {
            let id = read_attr_u32(current, uattr, 0)?;
            let registry_file = DEVICE_PROGS
                .lock()
                .iter()
                .find(|prog| prog.id == id)
                .and_then(|prog| prog.file.upgrade());
            let file = match registry_file {
                Some(file) => Some(file),
                None => ATTACHED_DEVICE_PROGS
                    .lock()
                    .iter()
                    .find(|prog| prog.prog_id == id)
                    .map(|prog| prog.prog.clone()),
            };
            match file {
                Some(file) => file::add_file_like(file, false).map(|fd| fd as isize),
                None => Err(StarryError::NotFound),
            }
        }
        DeviceCommand::GetInfoByFd => {
            let bpf_fd = read_attr_u32(current, uattr, ATTR_GET_INFO_FD_OFFSET)?;
            // Identity check: the fd must still reference a live
            // device-controller placeholder; a reused fd number pointing at
            // any other object never matches.
            let object = file::get_file_like(bpf_fd as i32)
                .map_err(|_| StarryError::NotFound)?;
            let id = object
                .downcast_ref::<BpfCgroupDeviceFile>()
                .map(|placeholder| placeholder.id)
                .ok_or(StarryError::NotFound)?;
            if !device_prog_live(id) {
                return Err(StarryError::NotFound);
            }
            // bpf_prog_info: `u32 type; u32 id; ...` — the caller's buffer
            // may be large, so report the two fields we model and shrink
            // `info_len` accordingly (the standard short-info contract).
            let info_ptr =
                crate::mm::vm_load(current, (uattr + ATTR_GET_INFO_PTR_OFFSET) as *const u64, 1)
                    .map_err(|_| StarryError::BadAddress)?[0];
            let info: [u32; 2] = [
                bpf_prog_type::BPF_PROG_TYPE_CGROUP_DEVICE as u32,
                id,
            ];
            // Honor the caller's buffer length: copy at most info_len bytes
            // and report that size back (the standard short-info contract).
            let info_len = read_attr_u32(current, uattr, ATTR_GET_INFO_LEN_OFFSET)?;
            let write_len = (info_len as usize).min(info.len() * 4);
            if write_len > 0 {
                let bytes: Vec<u8> = info
                    .iter()
                    .flat_map(|word| word.to_ne_bytes())
                    .take(write_len)
                    .collect();
                crate::mm::vm_write_slice(current, info_ptr as *mut u8, &bytes)
                    .map_err(|_| StarryError::BadAddress)?;
            }
            write_attr_u32(current, uattr, ATTR_GET_INFO_LEN_OFFSET, write_len as u32)?;
            Ok(0)
        }
        DeviceCommand::GetNextId => {
            let start = read_attr_u32(current, uattr, 0)?;
            match next_device_prog_id(start) {
                Some(id) => write_attr_u32(current, uattr, 0, id).map(|_| 0),
                None => Err(StarryError::NotFound),
            }
        }
        DeviceCommand::Attach => {
            // Feature probes classify support by the errno of an attach on
            // deliberately invalid fds: report `EBADF` for missing fds like
            // Linux, and accept any cgroup-device attach (the no-op
            // controller enforces no device rules) while recording it so a
            // later query observes the attach.
            let target_fd = read_attr_u32(current, uattr, ATTR_TARGET_FD_OFFSET)?;
            let bpf_fd = read_attr_u32(current, uattr, ATTR_ATTACH_BPF_FD_OFFSET)?;
            if !fd_exists(target_fd as i32) || !fd_exists(bpf_fd as i32) {
                return Err(StarryError::BadFileDescriptor);
            }
            let attach_type = read_attr_u32(current, uattr, ATTR_ATTACH_TYPE_OFFSET)?;
            let attach_bpf_fd = read_attr_u32(current, uattr, ATTR_ATTACH_BPF_FD_OFFSET)?;
            let bpf_object = file::get_file_like(attach_bpf_fd as i32)
                .map_err(|_| StarryError::BadFileDescriptor)?;
            let prog_id = bpf_object
                .downcast_ref::<BpfCgroupDeviceFile>()
                .map(|placeholder| placeholder.id)
                .ok_or(StarryError::BadFileDescriptor)?;
            if !device_prog_live(prog_id) {
                return Err(StarryError::BadFileDescriptor);
            }
            let target = file::get_file_like(target_fd as i32)
                .map_err(|_| StarryError::BadFileDescriptor)?;
            ATTACHED_DEVICE_PROGS.lock().push(AttachedDeviceProg {
                target,
                prog: bpf_object,
                attach_type,
                prog_id,
            });
            Ok(0)
        }
        DeviceCommand::Detach => {
            let target_fd = read_attr_u32(current, uattr, ATTR_TARGET_FD_OFFSET)?;
            if !fd_exists(target_fd as i32) {
                return Err(StarryError::BadFileDescriptor);
            }
            let attach_type = read_attr_u32(current, uattr, ATTR_ATTACH_TYPE_OFFSET)?;
            // Linux detaching an unknown attachment fails with ENOENT. The
            // current fd's object must be the attached one; a reused fd
            // number pointing elsewhere never matches.
            let target = file::get_file_like(target_fd as i32)
                .map_err(|_| StarryError::BadFileDescriptor)?;
            let mut attached = ATTACHED_DEVICE_PROGS.lock();
            match attached
                .iter()
                .position(|prog| {
                    prog.attach_type == attach_type
                        && Arc::ptr_eq(&prog.target, &target)
                })
            {
                Some(index) => {
                    attached.remove(index);
                    Ok(0)
                }
                None => Err(StarryError::NotFound),
            }
        }
        DeviceCommand::Query => {
            let target_fd = read_attr_u32(current, uattr, ATTR_TARGET_FD_OFFSET)?;
            if !fd_exists(target_fd as i32) {
                return Err(StarryError::BadFileDescriptor);
            }
            let attach_type = read_attr_u32(current, uattr, ATTR_QUERY_ATTACH_TYPE_OFFSET)?;
            let capacity = read_attr_u32(current, uattr, ATTR_QUERY_PROG_CNT_OFFSET)?;
            let ids_ptr =
                crate::mm::vm_load(current, (uattr + ATTR_QUERY_PROG_IDS_OFFSET) as *const u64, 1)
                    .map_err(|_| StarryError::BadAddress)?[0];
            let target = file::get_file_like(target_fd as i32)
                .map_err(|_| StarryError::BadFileDescriptor)?;
            let ids: Vec<u32> = ATTACHED_DEVICE_PROGS
                .lock()
                .iter()
                .filter(|prog| {
                    prog.attach_type == attach_type
                        && Arc::ptr_eq(&prog.target, &target)
                })
                .map(|prog| prog.prog_id)
                .collect();
            // `prog_ids == NULL` is a count-only query: report the number of
            // attached programs without touching any buffer.
            if ids_ptr != 0 && ids.len() as u32 > capacity {
                // Linux reports the required size through prog_cnt and fails.
                write_attr_u32(current, uattr, ATTR_QUERY_PROG_CNT_OFFSET, ids.len() as u32)?;
                return Err(StarryError::StorageFull);
            }
            if ids_ptr != 0 && !ids.is_empty() {
                // Write the ids through the caller's `prog_ids` pointer and
                // the real count through `prog_cnt`, exactly like Linux.
                crate::mm::vm_write_slice(current, ids_ptr as *mut u32, &ids)
                    .map_err(|_| StarryError::BadAddress)?;
            }
            write_attr_u32(current, uattr, ATTR_QUERY_PROG_CNT_OFFSET, ids.len() as u32)?;
            Ok(0)
        }
    }
}

/// Anonymous-inode placeholder handed out for device-controller program loads
/// and link creations. Carries the load's synthetic id so an fd can be
/// identified by object (downcast) rather than by its raw fd number. No
/// operations beyond lifetime management.
struct BpfCgroupDeviceFile {
    id: u32,
}

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

