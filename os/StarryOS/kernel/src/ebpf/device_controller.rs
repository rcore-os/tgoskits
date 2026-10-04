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

use core::mem::size_of;

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

/// Entry point wired into [`super::sys_bpf`]. `attr` is the kernel-local copy
/// of the caller's `union bpf_attr`, and `size` is the caller-declared byte
/// length of that attribute.
///
/// Returns `None` when `cmd` does not target the cgroup device controller, so
/// the caller falls through to the real `bpf(2)` handlers.
pub(crate) fn try_handle(
    cmd: bpf_cmd,
    attr: &bpf_attr,
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
            match read_attr_u32(attr, size, ATTR_ATTACH_TYPE_OFFSET) {
                Ok(attach_type) if attach_type == device_attach_type => refuse(),
                Ok(_) => None,
                Err(err) => Some(Err(err)),
            }
        }
        bpf_cmd::BPF_PROG_QUERY => {
            if size < MIN_QUERY_ATTR_SIZE {
                return Some(Err(StarryError::InvalidInput));
            }
            match read_attr_u32(attr, size, ATTR_QUERY_ATTACH_TYPE_OFFSET) {
                Ok(attach_type) if attach_type == device_attach_type => refuse(),
                Ok(_) => None,
                Err(err) => Some(Err(err)),
            }
        }
        bpf_cmd::BPF_LINK_CREATE => {
            if size < MIN_LINK_ATTR_SIZE {
                return Some(Err(StarryError::InvalidInput));
            }
            match read_attr_u32(attr, size, ATTR_LINK_ATTACH_TYPE_OFFSET) {
                Ok(attach_type) if attach_type == device_attach_type => refuse(),
                Ok(_) => None,
                Err(err) => Some(Err(err)),
            }
        }
        _ => None,
    }
}

/// Reads one `u32` field of the already-imported `union bpf_attr` copy at
/// `offset`.
///
/// Reading the kernel-local copy (not the user memory `uattr` points at)
/// keeps classification consistent with the `attr` the real handlers
/// receive, so a racing user write cannot make the device-controller gate
/// and the fall-through handler observe different attribute values.
fn read_attr_u32(attr: &bpf_attr, size: u32, offset: usize) -> StarryResult<u32> {
    let end = offset + size_of::<u32>();
    if end > attr_size_max(size) {
        // The classify min-size table checked this before dispatch; a miss
        // means the table and the command layout drifted apart.
        return Err(StarryError::InvalidInput);
    }
    // SAFETY: `attr` is a kernel-local union copy imported from user memory;
    // `offset..end` lies within the caller-declared `size`, which the
    // per-command min-size check verified covers the command's uapi layout.
    // Reading a `u32` at that offset only requires the bytes to be
    // initialized, which the import guarantees for the declared prefix.
    Ok(unsafe { *(core::ptr::addr_of!(*attr) as *const u32).add(offset / size_of::<u32>()) })
}

/// Upper bound of imported bytes for one attribute copy: the declared size,
/// never beyond the union itself.
fn attr_size_max(size: u32) -> usize {
    (size as usize).min(size_of::<bpf_attr>())
}
