//! # AxVirtIO Network Device
//!
//! A `no_std` VirtIO 1.x MMIO network device model for the ArceOS-Hypervisor
//! ecosystem. It owns only the VirtIO-net wire format, configuration and RX/TX
//! behavior on top of [`axvirtio_common`]; TAP/virtual-switch lifecycle and
//! virtual IRQ injection belong to the VMM glue.
//!
//! ## First-version scope
//!
//! - VirtIO 1.x MMIO transport, device ID `1` (network).
//! - Split virtqueue with one RX/TX queue pair.
//! - Features: `VIRTIO_F_VERSION_1`, `VIRTIO_NET_F_MAC`, `VIRTIO_NET_F_STATUS`.
//! - Legacy 10-byte and modern 12-byte `virtio_net_hdr` layouts; RX writes a
//!   zero header and TX rejects any requested offload.
//! - Explicit host-driven RX via [`VirtioMmioNetDevice::receive_frame`].
//!
//! Out of scope: control queue, multiqueue, mergeable buffers, indirect
//! descriptors, event index, checksum/GSO offload.

#![no_std]
extern crate alloc;

mod backend;
mod config;
mod constants;
mod device;
mod error;
mod header;
mod managed;
pub mod switch;

pub use axvirtio_common::{VirtioError, VirtioResult};
pub use backend::NetworkBackend;
pub use config::{LinkStatus, VirtioNetConfig};
pub use constants::*;
pub use device::{DeviceEvent, RxOutcome, VirtioMmioNetDevice};
pub use error::{NetError, NetworkBackendError};
pub use header::VirtioNetHdr;
pub use managed::ManagedVirtioNetDevice;
