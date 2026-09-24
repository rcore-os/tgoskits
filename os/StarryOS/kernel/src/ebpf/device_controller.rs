//! Explicit refusal for the cgroup-v2 device-controller `bpf(2)` commands.
//!
//! Non-rootless `runc` programs the cgroup-v2 device controller
//! unconditionally: it probes feature support, creates a map, loads
//! `BPF_PROG_TYPE_CGROUP_DEVICE` programs, attaches them to the container's
//! cgroup directory, and queries the cgroup for attached programs (see
//! `libcontainer/cgroups/ebpf` and `libcontainer/cgroups/fs2` in runc). This
//! kernel enforces no device access control, so the whole capability is
//! refused with `EOPNOTSUPP` rather than acknowledged.
//!
//! Acknowledging these commands would report a device policy that never takes
//! effect: runc/OCI would believe the container's device allow/deny list is
//! active while the sandbox still permits every device, turning a security
//! control into a lie. Refusing the capability lets runc's feature probes
//! classify the controller as unsupported and disable device filtering,
//! exactly as on a kernel without the controller.
//!
//! Only the device-controller family is refused:
//! - `BPF_PROG_LOAD` of a `CGROUP_DEVICE` program;
//! - `BPF_PROG_ATTACH`/`BPF_PROG_DETACH` with `BPF_CGROUP_DEVICE`;
//! - `BPF_PROG_QUERY` with `BPF_CGROUP_DEVICE`;
//! - `BPF_LINK_CREATE` with `BPF_CGROUP_DEVICE`.
//!
//! Every other `bpf(2)` command and program type falls through to the real
//! handlers, so the in-kernel eBPF subsystem keeps its behavior. In
//! particular, no synthetic program-id space is minted, so
//! `BPF_PROG_GET_NEXT_ID`/`BPF_PROG_GET_FD_BY_ID`/`BPF_OBJ_GET_INFO_BY_FD`
//! keep their existing (unsupported) fall-through.

use kbpf_basic::linux_bpf::{bpf_attach_type, bpf_attr, bpf_cmd, bpf_prog_type};

use crate::{StarryError, StarryResult};

/// Offset of `attach_type` in the attach/detach attribute layout.
const ATTR_ATTACH_TYPE_OFFSET: usize = 8;
/// Offset of `attach_type` in the query attribute layout.
const ATTR_QUERY_ATTACH_TYPE_OFFSET: usize = 4;
/// Offset of `attach_type` in the `link_create` attribute layout.
const ATTR_LINK_ATTACH_TYPE_OFFSET: usize = 8;
/// Minimum `union bpf_attr` size covering fields through `attach_type`.
const MIN_ATTACH_ATTR_SIZE: u32 = 12;
/// Minimum `union bpf_attr` size covering `prog_cnt`@24.
const MIN_QUERY_ATTR_SIZE: u32 = 28;
/// Minimum `union bpf_attr` size covering fields through `prog_type`.
const MIN_PROG_ATTR_SIZE: u32 = 4;
/// Minimum `union bpf_attr` size covering the `link_create` `attach_type`.
const MIN_LINK_ATTR_SIZE: u32 = 12;

/// Entry point wired into [`super::sys_bpf`]. `uattr` is the raw user-space
/// address of `union bpf_attr`.
///
/// Returns `None` when `cmd` does not target the cgroup device controller, so
/// the caller falls through to the real `bpf(2)` handlers.
pub(crate) fn try_handle(
    current: &crate::task::UserTaskRef,
    cmd: bpf_cmd,
    attr: &bpf_attr,
    uattr: usize,
    size: u32,
) -> Option<StarryResult<isize>> {
    let device_attach_type = bpf_attach_type::BPF_CGROUP_DEVICE as u32;
    let device_prog_type = bpf_prog_type::BPF_PROG_TYPE_CGROUP_DEVICE as u32;
    let refuse = || Some(Err(StarryError::OperationNotSupported));

    match cmd {
        bpf_cmd::BPF_PROG_LOAD => {
            if size < MIN_PROG_ATTR_SIZE {
                return Some(Err(StarryError::InvalidInput));
            }
            // SAFETY: `attr` is a plain union copy read from user memory;
            // reading the `prog_load` arm is the layout documented by the
            // command's uapi contract.
            let prog_type = unsafe { attr.__bindgen_anon_3.prog_type };
            (prog_type == device_prog_type).then(refuse).flatten()
        }
        bpf_cmd::BPF_PROG_ATTACH | bpf_cmd::BPF_PROG_DETACH => {
            if size < MIN_ATTACH_ATTR_SIZE {
                return Some(Err(StarryError::InvalidInput));
            }
            match read_attr_u32(current, uattr, ATTR_ATTACH_TYPE_OFFSET) {
                Ok(attach_type) if attach_type == device_attach_type => refuse(),
                Ok(_) => None,
                Err(err) => Some(Err(err)),
            }
        }
        bpf_cmd::BPF_PROG_QUERY => {
            if size < MIN_QUERY_ATTR_SIZE {
                return Some(Err(StarryError::InvalidInput));
            }
            match read_attr_u32(current, uattr, ATTR_QUERY_ATTACH_TYPE_OFFSET) {
                Ok(attach_type) if attach_type == device_attach_type => refuse(),
                Ok(_) => None,
                Err(err) => Some(Err(err)),
            }
        }
        bpf_cmd::BPF_LINK_CREATE => {
            if size < MIN_LINK_ATTR_SIZE {
                return Some(Err(StarryError::InvalidInput));
            }
            match read_attr_u32(current, uattr, ATTR_LINK_ATTACH_TYPE_OFFSET) {
                Ok(attach_type) if attach_type == device_attach_type => refuse(),
                Ok(_) => None,
                Err(err) => Some(Err(err)),
            }
        }
        _ => None,
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
    let loaded = crate::mm::vm_load(current, (uattr + offset) as *const u32, 1)
        .map_err(|_| StarryError::BadAddress)?;
    Ok(loaded[0])
}