//! virtio-gpu wire format.
//!
//! The layouts follow the VirtIO GPU device specification (1.2, section 5.7)
//! and the Linux UAPI header `linux/virtio_gpu.h`. Only the commands this
//! workspace issues are defined here.

use bitflags::bitflags;
use virtio_drivers::config::{ReadOnly, WriteOnly};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use crate::{Error, GpuBox, Rect};

/// Device configuration space (`struct virtio_gpu_config`).
///
/// The fields are kept in wire order so the offsets of the registers this
/// driver reads match the device layout. `num_capsets` is deliberately absent:
/// this driver never iterates capsets by count, and reading a register that
/// legacy devices do not implement would only add a failure mode.
#[repr(C)]
#[derive(FromBytes, Immutable, IntoBytes)]
pub(crate) struct Config {
    /// Events the device has signalled and not yet seen cleared.
    pub(crate) events_read: ReadOnly<u32>,
    /// Registers the driver writes to clear pending events.
    pub(crate) events_clear: WriteOnly<u32>,
    /// Number of scanouts the device supports.
    pub(crate) num_scanouts: ReadOnly<u32>,
}

/// `VIRTIO_GPU_EVENT_DISPLAY`: the display configuration changed.
///
/// Linux reads this out of `virtio_gpu_config.events_read` and writes it back
/// to `events_clear` in `virtio_gpu_config_changed_work_func()`
/// (`virtgpu_drv.c`).
pub(crate) const VIRTIO_GPU_EVENT_DISPLAY: u32 = 1 << 0;

bitflags! {
    /// Device feature bits used by this driver.
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
    pub(crate) struct Features: u64 {
        /// virgl 3D mode is supported.
        const VIRGL = 1 << 0;
        /// Blob resources (host-visible memory, dma-buf sharing) are supported.
        const RESOURCE_BLOB = 1 << 3;
        /// Context-init protocol (virgl2 capset, per-fd context).
        const CONTEXT_INIT = 1 << 4;

        const RING_INDIRECT_DESC = 1 << 28;
        const RING_EVENT_IDX = 1 << 29;
        const VERSION_1 = 1 << 32;
    }
}

/// Feature set this driver is willing to negotiate.
///
/// `VIRTIO_F_ACCESS_PLATFORM` is deliberately absent: the published
/// `virtio-drivers` 0.13.0 `Hal` and virtqueue API carry no platform-access
/// parameter, so the driver must not accept a feature it cannot honour.
/// `EDID` is absent too, because this driver never issues `GET_EDID`.
pub(crate) const SUPPORTED_FEATURES: Features = Features::RING_EVENT_IDX
    .union(Features::RING_INDIRECT_DESC)
    .union(Features::VERSION_1)
    .union(Features::VIRGL)
    .union(Features::CONTEXT_INIT)
    .union(Features::RESOURCE_BLOB);

/// A virtio-gpu control command or response code.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq)]
pub(crate) struct Command(pub(crate) u32);

impl Command {
    // 2D and display commands.
    pub(crate) const GET_DISPLAY_INFO: Command = Command(0x100);
    pub(crate) const RESOURCE_CREATE_2D: Command = Command(0x101);
    pub(crate) const RESOURCE_UNREF: Command = Command(0x102);
    pub(crate) const SET_SCANOUT: Command = Command(0x103);
    pub(crate) const RESOURCE_FLUSH: Command = Command(0x104);
    pub(crate) const TRANSFER_TO_HOST_2D: Command = Command(0x105);
    pub(crate) const RESOURCE_ATTACH_BACKING: Command = Command(0x106);
    pub(crate) const RESOURCE_DETACH_BACKING: Command = Command(0x107);
    pub(crate) const GET_CAPSET_INFO: Command = Command(0x108);
    pub(crate) const GET_CAPSET: Command = Command(0x109);
    pub(crate) const RESOURCE_CREATE_BLOB: Command = Command(0x10c);

    // 3D commands (VirtIO GPU spec section 5.7.5, table 5.7.5.2).
    pub(crate) const CTX_CREATE: Command = Command(0x0200);
    pub(crate) const CTX_DESTROY: Command = Command(0x0201);
    pub(crate) const CTX_ATTACH_RESOURCE: Command = Command(0x0202);
    pub(crate) const CTX_DETACH_RESOURCE: Command = Command(0x0203);
    pub(crate) const RESOURCE_CREATE_3D: Command = Command(0x0204);
    pub(crate) const TRANSFER_TO_HOST_3D: Command = Command(0x0205);
    pub(crate) const TRANSFER_FROM_HOST_3D: Command = Command(0x0206);
    pub(crate) const SUBMIT_3D: Command = Command(0x0207);

    // Success responses used by this driver.
    pub(crate) const OK_NODATA: Command = Command(0x1100);
    pub(crate) const OK_DISPLAY_INFO: Command = Command(0x1101);
    pub(crate) const OK_CAPSET_INFO: Command = Command(0x1102);
    pub(crate) const OK_CAPSET: Command = Command(0x1103);
}

/// `VIRTIO_GPU_FLAG_FENCE`: signalled when the command stream has completed.
const GPU_FLAG_FENCE: u32 = 1 << 0;

/// The header every virtio-gpu control command and response starts with.
#[repr(C)]
#[derive(Debug, Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct CtrlHeader {
    hdr_type: Command,
    flags: u32,
    fence_id: u64,
    ctx_id: u32,
    /// Ring index for fences; only meaningful with `VIRTIO_GPU_F_CONTEXT_INIT`.
    ring_idx: u8,
    _padding: [u8; 3],
}

impl CtrlHeader {
    /// A header for a command that targets no rendering context.
    pub(crate) const fn with_type(hdr_type: Command) -> Self {
        Self {
            hdr_type,
            flags: 0,
            fence_id: 0,
            ctx_id: 0,
            ring_idx: 0,
            _padding: [0; 3],
        }
    }

    /// A header for a command that targets a rendering context.
    pub(crate) const fn with_type_and_ctx(hdr_type: Command, ctx_id: u32) -> Self {
        Self {
            hdr_type,
            flags: 0,
            fence_id: 0,
            ctx_id,
            ring_idx: 0,
            _padding: [0; 3],
        }
    }

    /// A `SUBMIT_3D` header that asks the device for a fence signal.
    pub(crate) const fn with_fence(hdr_type: Command, ctx_id: u32, fence_id: u64) -> Self {
        Self {
            hdr_type,
            flags: GPU_FLAG_FENCE,
            fence_id,
            ctx_id,
            ring_idx: 0,
            _padding: [0; 3],
        }
    }

    /// Accepts the response only if its command code is the expected one.
    pub(crate) fn check_type(&self, expected: Command) -> Result<(), Error> {
        if self.hdr_type == expected {
            Ok(())
        } else {
            Err(Error::InvalidResponse)
        }
    }
}

/// `VIRTIO_GPU_RESP_OK_DISPLAY_INFO`.
#[repr(C)]
#[derive(Debug, FromBytes, Immutable, KnownLayout)]
pub(crate) struct RespDisplayInfo {
    pub(crate) header: CtrlHeader,
    pub(crate) rect: Rect,
    pub(crate) enabled: u32,
    pub(crate) flags: u32,
}

/// `VIRTIO_GPU_CMD_RESOURCE_CREATE_2D`.
#[repr(C)]
#[derive(Debug, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct ResourceCreate2D {
    pub(crate) header: CtrlHeader,
    pub(crate) resource_id: u32,
    pub(crate) format: Format,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// Pixel formats used by `RESOURCE_CREATE_2D`.
#[repr(u32)]
#[derive(Debug, Immutable, IntoBytes, KnownLayout)]
pub(crate) enum Format {
    /// `VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM`.
    B8G8R8A8Unorm = 1,
}

/// `VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING` with a single memory entry.
#[repr(C)]
#[derive(Debug, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct ResourceAttachBacking {
    pub(crate) header: CtrlHeader,
    pub(crate) resource_id: u32,
    pub(crate) nr_entries: u32,
    pub(crate) addr: u64,
    pub(crate) length: u32,
    pub(crate) _padding: u32,
}

/// `VIRTIO_GPU_CMD_RESOURCE_DETACH_BACKING`.
#[repr(C)]
#[derive(Debug, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct ResourceDetachBacking {
    pub(crate) header: CtrlHeader,
    pub(crate) resource_id: u32,
    pub(crate) _padding: u32,
}

/// `VIRTIO_GPU_CMD_RESOURCE_UNREF`.
#[repr(C)]
#[derive(Debug, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct ResourceUnref {
    pub(crate) header: CtrlHeader,
    pub(crate) resource_id: u32,
    pub(crate) _padding: u32,
}

/// `VIRTIO_GPU_CMD_SET_SCANOUT`.
#[repr(C)]
#[derive(Debug, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct SetScanout {
    pub(crate) header: CtrlHeader,
    pub(crate) rect: Rect,
    pub(crate) scanout_id: u32,
    pub(crate) resource_id: u32,
}

/// `VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D`.
#[repr(C)]
#[derive(Debug, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct TransferToHost2D {
    pub(crate) header: CtrlHeader,
    pub(crate) rect: Rect,
    pub(crate) offset: u64,
    pub(crate) resource_id: u32,
    pub(crate) _padding: u32,
}

/// `VIRTIO_GPU_CMD_RESOURCE_FLUSH`.
#[repr(C)]
#[derive(Debug, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct ResourceFlush {
    pub(crate) header: CtrlHeader,
    pub(crate) rect: Rect,
    pub(crate) resource_id: u32,
    pub(crate) _padding: u32,
}

/// `VIRTIO_GPU_CMD_GET_CAPSET_INFO`.
#[repr(C)]
#[derive(Debug, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct CmdGetCapsetInfo {
    pub(crate) header: CtrlHeader,
    pub(crate) capset_index: u32,
    pub(crate) _padding: u32,
}

/// `VIRTIO_GPU_RESP_OK_CAPSET_INFO`.
#[repr(C)]
#[derive(Debug, FromBytes, Immutable, KnownLayout)]
pub(crate) struct RespCapsetInfo {
    pub(crate) header: CtrlHeader,
    pub(crate) capset_id: u32,
    pub(crate) capset_max_version: u32,
    pub(crate) capset_max_size: u32,
    pub(crate) _padding: u32,
}

/// `VIRTIO_GPU_CMD_GET_CAPSET`.
#[repr(C)]
#[derive(Debug, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct CmdGetCapset {
    pub(crate) header: CtrlHeader,
    pub(crate) capset_id: u32,
    pub(crate) capset_version: u32,
}

/// `VIRTIO_GPU_CMD_CTX_CREATE`.
#[repr(C)]
#[derive(Debug, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct CmdCtxCreate {
    pub(crate) header: CtrlHeader,
    /// Length of the debug name, 0..=64.
    pub(crate) nlen: u32,
    /// Capset ID that selects the context protocol (virgl1: 0, virgl2: 2).
    pub(crate) context_init: u32,
    /// Null-padded debug name.
    pub(crate) debug_name: [u8; 64],
}

/// `VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE` and `CTX_DETACH_RESOURCE`.
#[repr(C)]
#[derive(Debug, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct CmdCtxResource {
    pub(crate) header: CtrlHeader,
    pub(crate) resource_id: u32,
    pub(crate) _padding: u32,
}

/// `VIRTIO_GPU_CMD_RESOURCE_CREATE_3D`.
#[repr(C)]
#[derive(Debug, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct CmdResourceCreate3D {
    pub(crate) header: CtrlHeader,
    pub(crate) resource_id: u32,
    pub(crate) target: u32,
    pub(crate) format: u32,
    pub(crate) bind: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) depth: u32,
    pub(crate) array_size: u32,
    pub(crate) last_level: u32,
    pub(crate) nr_samples: u32,
    pub(crate) flags: u32,
    pub(crate) _padding: u32,
}

/// `VIRTIO_GPU_CMD_TRANSFER_TO_HOST_3D` and `TRANSFER_FROM_HOST_3D`.
#[repr(C)]
#[derive(Debug, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct CmdTransferHost3D {
    pub(crate) header: CtrlHeader,
    pub(crate) box_: GpuBox,
    pub(crate) offset: u64,
    pub(crate) resource_id: u32,
    pub(crate) level: u32,
    pub(crate) stride: u32,
    pub(crate) layer_stride: u32,
}

/// `VIRTIO_GPU_CMD_SUBMIT_3D`.
#[repr(C)]
#[derive(Debug, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct CmdSubmit3D {
    pub(crate) header: CtrlHeader,
    /// Size in bytes of the command stream sent as a second buffer.
    pub(crate) size: u32,
    pub(crate) _padding: u32,
}

/// `VIRTIO_GPU_CMD_RESOURCE_CREATE_BLOB`.
///
/// The field order matches `struct virtio_gpu_resource_create_blob` in the
/// Linux UAPI header; `nr_entries` memory entries follow as a second buffer.
#[repr(C)]
#[derive(Debug, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct CmdResourceCreateBlob {
    pub(crate) header: CtrlHeader,
    pub(crate) resource_id: u32,
    pub(crate) blob_mem: u32,
    pub(crate) blob_flags: u32,
    pub(crate) nr_entries: u32,
    pub(crate) blob_id: u64,
    pub(crate) size: u64,
}

/// One entry of a blob resource backing.
///
/// Matches `struct virtio_gpu_mem_entry { __le64 addr; __le32 length; __le32 padding; }`.
#[repr(C)]
#[derive(Debug, Copy, Clone, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct MemEntry {
    pub(crate) addr: u64,
    pub(crate) length: u32,
    pub(crate) padding: u32,
}
