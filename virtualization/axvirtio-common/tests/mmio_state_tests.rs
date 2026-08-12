//! Shared MMIO transport state tests (plan section 13.2).

use std::sync::Arc;

use ax_memory_addr::PhysAddr;
use axaddrspace::GuestMemoryAccessor;
use axvirtio_common::{
    MmioReadOutcome, MmioWriteAction, VirtioMmioState, VirtioQueue, constants as vc,
};
use axvm_types::{AccessWidth, GuestPhysAddr};

const BASE: usize = 0x0a00_0000;
const LEN: usize = 0x200;

/// Mock guest memory: a flat backing buffer where guest physical address `gpa`
/// maps to `buf[gpa..]` for `gpa < 0x10000`. The accessor returns real host
/// pointers, so the memory-based ring layout check and the trait defaults work
/// against the same buffer.
#[derive(Clone)]
struct Mem {
    buf: std::sync::Arc<Vec<u8>>,
}
impl GuestMemoryAccessor for Mem {
    fn translate_and_get_limit(&self, guest_addr: GuestPhysAddr) -> Option<(PhysAddr, usize)> {
        let off = guest_addr.as_usize();
        if off < self.buf.len() {
            Some((
                PhysAddr::from(self.buf.as_ptr() as usize + off),
                self.buf.len() - off,
            ))
        } else {
            None
        }
    }
}

fn mem() -> Mem {
    Mem {
        buf: std::sync::Arc::new(vec![0u8; 0x1_0000]),
    }
}

fn state(device_features: u64) -> VirtioMmioState<Mem> {
    let accessor = Arc::new(mem());
    let queue = VirtioQueue::new(0, vc::DEFAULT_QUEUE_SIZE, accessor);
    VirtioMmioState::new(
        GuestPhysAddr::from(BASE),
        LEN,
        2, // device id (block, arbitrary for these tests)
        vc::VIRTIO_VENDOR_ID,
        device_features,
        vec![queue],
    )
}

fn bounded_state(device_features: u64) -> VirtioMmioState<Mem> {
    // Same mock: backing covers `[0, 0x10000)`, so a used ring at 0xfff8
    // crosses the end of the guest address space.
    state(device_features)
}

fn bounded_rd(s: &VirtioMmioState<Mem>, reg: usize) -> u32 {
    match s
        .mmio_read(GuestPhysAddr::from(BASE + reg), AccessWidth::Dword)
        .unwrap()
    {
        MmioReadOutcome::Standard(v) => v,
        MmioReadOutcome::DeviceConfig { .. } => panic!("expected standard register"),
    }
}
fn bounded_wr(s: &VirtioMmioState<Mem>, reg: usize, val: u32) -> MmioWriteAction {
    s.mmio_write(
        GuestPhysAddr::from(BASE + reg),
        AccessWidth::Dword,
        val as usize,
    )
    .unwrap()
}

fn rd(s: &VirtioMmioState<Mem>, reg: usize) -> u32 {
    match s
        .mmio_read(GuestPhysAddr::from(BASE + reg), AccessWidth::Dword)
        .unwrap()
    {
        MmioReadOutcome::Standard(v) => v,
        MmioReadOutcome::DeviceConfig { .. } => panic!("expected standard register"),
    }
}
fn wr(s: &VirtioMmioState<Mem>, reg: usize, val: u32) -> MmioWriteAction {
    s.mmio_write(
        GuestPhysAddr::from(BASE + reg),
        AccessWidth::Dword,
        val as usize,
    )
    .unwrap()
}

#[test]
fn identity_registers() {
    let s = state(0);
    assert_eq!(rd(&s, vc::VIRTIO_MMIO_MAGIC_VALUE), vc::MMIO_MAGIC_VALUE);
    assert_eq!(rd(&s, vc::VIRTIO_MMIO_VERSION), vc::MMIO_VERSION);
    assert_eq!(rd(&s, vc::VIRTIO_MMIO_DEVICE_ID), 2);
    assert_eq!(rd(&s, vc::VIRTIO_MMIO_VENDOR_ID), vc::VIRTIO_VENDOR_ID);
}

#[test]
fn device_features_split_by_selector() {
    // Bit 5 (lo) and bit 33 (hi).
    let s = state((1u64 << 5) | (1u64 << 33));
    wr(&s, vc::VIRTIO_MMIO_DEVICE_FEATURES_SEL, 0);
    assert_eq!(rd(&s, vc::VIRTIO_MMIO_DEVICE_FEATURES), 1u32 << 5);
    wr(&s, vc::VIRTIO_MMIO_DEVICE_FEATURES_SEL, 1);
    assert_eq!(rd(&s, vc::VIRTIO_MMIO_DEVICE_FEATURES), 1u32 << 1);
    wr(&s, vc::VIRTIO_MMIO_DEVICE_FEATURES_SEL, 5);
    assert_eq!(rd(&s, vc::VIRTIO_MMIO_DEVICE_FEATURES), 0);
}

#[test]
fn driver_features_low_high_combine() {
    let s = state(0);
    wr(&s, vc::VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0);
    wr(&s, vc::VIRTIO_MMIO_DRIVER_FEATURES, 0x0000_00ff);
    wr(&s, vc::VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1);
    wr(&s, vc::VIRTIO_MMIO_DRIVER_FEATURES, 0x0000_00aa);

    wr(&s, vc::VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0);
    assert_eq!(rd(&s, vc::VIRTIO_MMIO_DRIVER_FEATURES), 0xff);
    wr(&s, vc::VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1);
    assert_eq!(rd(&s, vc::VIRTIO_MMIO_DRIVER_FEATURES), 0xaa);
}

#[test]
fn feature_negotiation_rejects_non_subset() {
    let s = state(1u64 << 5); // only bit 5 supported
    wr(&s, vc::VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0);
    wr(&s, vc::VIRTIO_MMIO_DRIVER_FEATURES, 0xffff_ffff); // claims unsupported bits
    let action = wr(&s, vc::VIRTIO_MMIO_STATUS, vc::VIRTIO_STATUS_FEATURES_OK);
    assert_eq!(action, MmioWriteAction::None);
    let status = rd(&s, vc::VIRTIO_MMIO_STATUS);
    assert_eq!(status & vc::VIRTIO_STATUS_FEATURES_OK, 0);
    assert_ne!(status & vc::VIRTIO_STATUS_FAILED, 0);
}

#[test]
fn queue_notify_returns_action() {
    let s = state(0);
    assert_eq!(
        wr(&s, vc::VIRTIO_MMIO_QUEUE_NOTIFY, 0),
        MmioWriteAction::QueueNotified(0)
    );
}

#[test]
fn status_zero_resets() {
    let s = state(0);
    wr(&s, vc::VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0);
    wr(&s, vc::VIRTIO_MMIO_DRIVER_FEATURES, 0x1234);
    assert_eq!(wr(&s, vc::VIRTIO_MMIO_STATUS, 0), MmioWriteAction::Reset);
    assert_eq!(rd(&s, vc::VIRTIO_MMIO_STATUS), 0);
    wr(&s, vc::VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0);
    assert_eq!(
        rd(&s, vc::VIRTIO_MMIO_DRIVER_FEATURES),
        0,
        "reset must clear driver features"
    );
}

#[test]
fn queue_ready_rejected_when_ring_outside_guest_address_space() {
    let s = bounded_state(0);
    bounded_wr(&s, vc::VIRTIO_MMIO_QUEUE_SEL, 0);
    bounded_wr(&s, vc::VIRTIO_MMIO_QUEUE_NUM, 4);
    bounded_wr(&s, vc::VIRTIO_MMIO_QUEUE_DESC_LOW, 0x1000);
    bounded_wr(&s, vc::VIRTIO_MMIO_QUEUE_AVAIL_LOW, 0x2000);
    bounded_wr(&s, vc::VIRTIO_MMIO_QUEUE_USED_LOW, 0xfff8); // tail beyond 0x10000
    bounded_wr(&s, vc::VIRTIO_MMIO_QUEUE_READY, 1);
    assert_eq!(
        bounded_rd(&s, vc::VIRTIO_MMIO_QUEUE_READY),
        0,
        "a ring crossing the guest address-space boundary must not become ready"
    );

    // Re-programming a fully in-space layout lets the queue become ready.
    bounded_wr(&s, vc::VIRTIO_MMIO_QUEUE_USED_LOW, 0xf000);
    bounded_wr(&s, vc::VIRTIO_MMIO_QUEUE_READY, 1);
    assert_eq!(bounded_rd(&s, vc::VIRTIO_MMIO_QUEUE_READY), 1);
}

#[test]
fn queue_ready_rejected_on_malformed_ring_layout() {
    let s = state(0);
    wr(&s, vc::VIRTIO_MMIO_QUEUE_SEL, 0);
    wr(&s, vc::VIRTIO_MMIO_QUEUE_NUM, 4);
    wr(&s, vc::VIRTIO_MMIO_QUEUE_DESC_LOW, 0x1000);
    wr(&s, vc::VIRTIO_MMIO_QUEUE_AVAIL_LOW, 0x2000);
    wr(&s, vc::VIRTIO_MMIO_QUEUE_USED_LOW, 0x3003); // not 4-byte aligned
    wr(&s, vc::VIRTIO_MMIO_QUEUE_READY, 1);
    assert_eq!(
        rd(&s, vc::VIRTIO_MMIO_QUEUE_READY),
        0,
        "ready must not become effective on a malformed layout"
    );

    // Re-programming a valid layout lets the same queue become ready.
    wr(&s, vc::VIRTIO_MMIO_QUEUE_USED_LOW, 0x3000);
    wr(&s, vc::VIRTIO_MMIO_QUEUE_READY, 1);
    assert_eq!(rd(&s, vc::VIRTIO_MMIO_QUEUE_READY), 1);

    // Writing ready=0 clears it again.
    wr(&s, vc::VIRTIO_MMIO_QUEUE_READY, 0);
    assert_eq!(rd(&s, vc::VIRTIO_MMIO_QUEUE_READY), 0);
}

#[test]
fn out_of_range_read_returns_zero_not_magic() {
    // Regression: a read past the MMIO region must return 0, not be mistaken for
    // offset 0 (the magic register).
    let s = state(0);
    let out = s
        .mmio_read(GuestPhysAddr::from(BASE + LEN + 16), AccessWidth::Dword)
        .unwrap();
    assert_eq!(out, MmioReadOutcome::Standard(0));
}

#[test]
fn non_dword_standard_register_rejected() {
    let s = state(0);
    assert!(
        s.mmio_read(
            GuestPhysAddr::from(BASE + vc::VIRTIO_MMIO_MAGIC_VALUE),
            AccessWidth::Byte
        )
        .is_err()
    );
}

#[test]
fn interrupt_ack_preserves_same_bit_raised_after_status_read() {
    let s = state(0);
    s.set_interrupt(vc::VIRTIO_MMIO_INT_VRING);
    assert_eq!(
        rd(&s, vc::VIRTIO_MMIO_INTERRUPT_STATUS),
        vc::VIRTIO_MMIO_INT_VRING
    );

    // A second completion races in after the driver's status read. Its event
    // must not be consumed by the acknowledgement for the first completion.
    s.set_interrupt(vc::VIRTIO_MMIO_INT_VRING);
    assert_eq!(
        wr(&s, vc::VIRTIO_MMIO_INTERRUPT_ACK, vc::VIRTIO_MMIO_INT_VRING,),
        MmioWriteAction::InterruptPending
    );

    assert_eq!(
        s.interrupt_status(),
        vc::VIRTIO_MMIO_INT_VRING,
        "the post-read completion must remain pending"
    );

    assert_eq!(
        rd(&s, vc::VIRTIO_MMIO_INTERRUPT_STATUS),
        vc::VIRTIO_MMIO_INT_VRING
    );
    assert_eq!(
        wr(&s, vc::VIRTIO_MMIO_INTERRUPT_ACK, vc::VIRTIO_MMIO_INT_VRING,),
        MmioWriteAction::None
    );
    assert_eq!(s.interrupt_status(), 0);
}
