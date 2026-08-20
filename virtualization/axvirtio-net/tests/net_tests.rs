//! Integration tests for axvirtio-net (plan sections 13.3-13.6).

use std::sync::{Arc, Mutex};

use ax_memory_addr::PhysAddr;
use axaddrspace::GuestMemoryAccessor;
use axdevice_base::{
    BusKind, ControllerInputId, Device, DeviceAccess, DeviceId, DeviceVcpuId,
    InterruptControllerId, InterruptTriggerMode, IrqResult, NoopDeviceContext, Resource,
    WiredIrqInput, WiredIrqSink,
};
// Re-export the MMIO register offsets from the common constants.
use axvirtio_common::constants as vc;
use axvirtio_net::{
    DeviceEvent, LinkStatus, ManagedVirtioNetDevice, NetError, NetworkBackend, NetworkBackendError,
    RxOutcome, VirtioMmioNetDevice, VirtioNetConfig,
};
use axvm_types::{AccessWidth, GuestPhysAddr};

const BASE_IPA: usize = 0x0a00_0000;
const REGION_LEN: usize = 0x200;
const DEFAULT_QSIZE: u16 = 4;
const NEGOTIATED_HEADER_SIZE: usize = axvirtio_net::VIRTIO_NET_HDR_MODERN_SIZE;

/// Mock guest memory: flat backing buffer, guest phys -> real host pointer.
#[derive(Clone)]
struct MockMem {
    buf: Vec<u8>,
}

impl MockMem {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0u8; len],
        }
    }
    fn put(&self, off: usize, bytes: &[u8]) {
        self.write_buffer(GuestPhysAddr::from(off), bytes).unwrap();
    }
    fn peek(&self, off: usize, len: usize) -> Vec<u8> {
        self.buf[off..off + len].to_vec()
    }
}

impl GuestMemoryAccessor for MockMem {
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

// The device wraps the accessor in its own `Arc`; sharing one buffer between the
// test and the device requires the accessor to be a cheap clone of the same
// allocation. A newtype around `Arc<MockMem>` satisfies the orphan rules.
#[derive(Clone)]
struct SharedMem(Arc<MockMem>);

impl GuestMemoryAccessor for SharedMem {
    fn translate_and_get_limit(&self, guest_addr: GuestPhysAddr) -> Option<(PhysAddr, usize)> {
        self.0.translate_and_get_limit(guest_addr)
    }
}

/// Recording TX backend shared between the device and the test.
#[derive(Clone)]
struct RecordBackend {
    frames: Arc<Mutex<Vec<Vec<u8>>>>,
    fail_next: Arc<Mutex<bool>>,
}

impl RecordBackend {
    fn new() -> (Self, Arc<Mutex<Vec<Vec<u8>>>>) {
        let frames = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                frames: frames.clone(),
                fail_next: Arc::new(Mutex::new(false)),
            },
            frames,
        )
    }
    fn fail_next(&self) {
        *self.fail_next.lock().unwrap() = true;
    }
    fn calls(&self) -> usize {
        self.frames.lock().unwrap().len()
    }
}

impl NetworkBackend for RecordBackend {
    fn transmit(&self, frame: &[u8]) -> Result<(), NetworkBackendError> {
        if *self.fail_next.lock().unwrap() {
            return Err(NetworkBackendError::TransmitFailed);
        }
        self.frames.lock().unwrap().push(frame.to_vec());
        Ok(())
    }
}

/// A guest-mem layout for one queue (desc | avail | used), 16-byte aligned.
fn queue_layout(base: usize, size: u16) -> (usize, usize, usize, usize) {
    let desc = base;
    let desc_size = size as usize * 16;
    let avail = align16(desc + desc_size);
    let avail_size = 4 + size as usize * 2 + 2;
    let used = align16(avail + avail_size);
    let used_size = 4 + size as usize * 8 + 2;
    let end = align16(used + used_size);
    (desc, avail, used, end)
}

fn align16(v: usize) -> usize {
    v.div_ceil(16) * 16
}

/// Test harness wiring a device plus two queues in guest memory.
struct Harness {
    mem: Arc<MockMem>,
    device: VirtioMmioNetDevice<RecordBackend, SharedMem>,
    backend: RecordBackend,
    rx_desc: usize,
    rx_avail: usize,
    rx_used: usize,
    tx_desc: usize,
    tx_avail: usize,
    tx_used: usize,
    size: u16,
}

impl Harness {
    fn new() -> Self {
        let size = DEFAULT_QSIZE;
        // RX queue at 0x4000, TX queue at 0x8000.
        let (rx_desc, rx_avail, rx_used, rx_end) = queue_layout(0x4000, size);
        let (tx_desc, tx_avail, tx_used, tx_end) = queue_layout(0x8000, size);
        let mem: Arc<MockMem> = Arc::new(MockMem::new(rx_end.max(tx_end) + 0x1000));
        let (backend, _frames) = RecordBackend::new();
        let device = VirtioMmioNetDevice::new(
            GuestPhysAddr::from(BASE_IPA),
            REGION_LEN,
            backend.clone(),
            VirtioNetConfig::default(),
            SharedMem(mem.clone()),
        )
        .unwrap();
        Self {
            mem,
            device,
            backend,
            rx_desc,
            rx_avail,
            rx_used,
            tx_desc,
            tx_avail,
            tx_used,
            size,
        }
    }

    fn w(&self, reg: usize, val: u32) -> DeviceEvent {
        self.device
            .mmio_write(
                GuestPhysAddr::from(BASE_IPA + reg),
                AccessWidth::Dword,
                val as usize,
            )
            .unwrap()
    }
    fn r(&self, reg: usize) -> u32 {
        self.device
            .mmio_read(GuestPhysAddr::from(BASE_IPA + reg), AccessWidth::Dword)
            .unwrap() as u32
    }

    /// Program one queue (desc/avail/used addresses + ready) via MMIO.
    fn setup_queue(&self, qidx: u16, desc: usize, avail: usize, used: usize) {
        self.w(vc::VIRTIO_MMIO_QUEUE_SEL, qidx as u32);
        self.w(vc::VIRTIO_MMIO_QUEUE_NUM, self.size as u32);
        self.w(vc::VIRTIO_MMIO_QUEUE_DESC_LOW, desc as u32);
        self.w(vc::VIRTIO_MMIO_QUEUE_DESC_HIGH, (desc >> 32) as u32);
        self.w(vc::VIRTIO_MMIO_QUEUE_AVAIL_LOW, avail as u32);
        self.w(vc::VIRTIO_MMIO_QUEUE_AVAIL_HIGH, (avail >> 32) as u32);
        self.w(vc::VIRTIO_MMIO_QUEUE_USED_LOW, used as u32);
        self.w(vc::VIRTIO_MMIO_QUEUE_USED_HIGH, (used >> 32) as u32);
        self.w(vc::VIRTIO_MMIO_QUEUE_READY, 1);
    }

    /// Full guest driver bring-up: ACK, DRIVER, negotiate features, FEATURES_OK,
    /// configure both queues, DRIVER_OK. Status bits are written cumulatively
    /// (the device sets status to the written value, per the VirtIO spec).
    fn bring_up(&self) {
        // Negotiate all advertised features.
        let lo = self.r(vc::VIRTIO_MMIO_DEVICE_FEATURES);
        self.w(vc::VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0);
        self.w(vc::VIRTIO_MMIO_DRIVER_FEATURES, lo);
        let hi = {
            self.w(vc::VIRTIO_MMIO_DEVICE_FEATURES_SEL, 1);
            self.r(vc::VIRTIO_MMIO_DEVICE_FEATURES)
        };
        self.w(vc::VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1);
        self.w(vc::VIRTIO_MMIO_DRIVER_FEATURES, hi);

        self.w(
            vc::VIRTIO_MMIO_STATUS,
            vc::VIRTIO_STATUS_ACKNOWLEDGE
                | vc::VIRTIO_STATUS_DRIVER
                | vc::VIRTIO_STATUS_FEATURES_OK,
        );
        self.setup_queue(0, self.rx_desc, self.rx_avail, self.rx_used);
        self.setup_queue(1, self.tx_desc, self.tx_avail, self.tx_used);
        self.w(
            vc::VIRTIO_MMIO_STATUS,
            vc::VIRTIO_STATUS_ACKNOWLEDGE
                | vc::VIRTIO_STATUS_DRIVER
                | vc::VIRTIO_STATUS_FEATURES_OK
                | vc::VIRTIO_STATUS_DRIVER_OK,
        );
    }

    fn write_desc(&self, table: usize, idx: u16, addr: usize, len: u32, flags: u16, next: u16) {
        let off = table + idx as usize * 16;
        let mut b = [0u8; 16];
        b[0..8].copy_from_slice(&(addr as u64).to_le_bytes());
        b[8..12].copy_from_slice(&len.to_le_bytes());
        b[12..14].copy_from_slice(&flags.to_le_bytes());
        b[14..16].copy_from_slice(&next.to_le_bytes());
        self.mem.put(off, &b);
    }
    fn set_avail(&self, avail: usize, pos: u16, head: u16, idx: u16) {
        let mut b = [0u8; 4];
        b[2..4].copy_from_slice(&idx.to_le_bytes());
        self.mem.put(avail, &b);
        let mut e = [0u8; 2];
        e.copy_from_slice(&head.to_le_bytes());
        self.mem.put(avail + 4 + pos as usize * 2, &e);
    }
    fn used_idx(&self, used: usize) -> u16 {
        u16::from_le_bytes([self.mem.buf[used + 2], self.mem.buf[used + 3]])
    }
    fn used_elem(&self, used: usize, pos: u16) -> (u32, u32) {
        let off = used + 4 + pos as usize * 8;
        let id = u32::from_le_bytes(self.mem.buf[off..off + 4].try_into().unwrap());
        let len = u32::from_le_bytes(self.mem.buf[off + 4..off + 8].try_into().unwrap());
        (id, len)
    }
}

// ---------------------------------------------------------------------------
// Identity and configuration (13.3)
// ---------------------------------------------------------------------------

#[test]
fn device_identity_and_features() {
    let h = Harness::new();
    assert_eq!(h.r(vc::VIRTIO_MMIO_MAGIC_VALUE), vc::MMIO_MAGIC_VALUE);
    assert_eq!(h.r(vc::VIRTIO_MMIO_VERSION), vc::MMIO_VERSION);
    assert_eq!(h.r(vc::VIRTIO_MMIO_DEVICE_ID), 1); // Network
    assert_eq!(h.r(vc::VIRTIO_MMIO_VENDOR_ID), vc::VIRTIO_VENDOR_ID);

    // Advertised features: VERSION_1 (hi) | MAC | STATUS (lo).
    h.w(vc::VIRTIO_MMIO_DEVICE_FEATURES_SEL, 0);
    let lo = h.r(vc::VIRTIO_MMIO_DEVICE_FEATURES);
    h.w(vc::VIRTIO_MMIO_DEVICE_FEATURES_SEL, 1);
    let hi = h.r(vc::VIRTIO_MMIO_DEVICE_FEATURES);
    let feats = lo as u64 | ((hi as u64) << 32);
    assert_eq!(feats, axvirtio_net::AXVIRTIO_NET_FEATURES);
    assert_eq!(
        feats & axvirtio_net::VIRTIO_NET_F_MAC,
        axvirtio_net::VIRTIO_NET_F_MAC
    );
    assert_eq!(
        feats & axvirtio_net::VIRTIO_NET_F_STATUS,
        axvirtio_net::VIRTIO_NET_F_STATUS
    );
}

#[test]
fn mac_and_status_config_reads() {
    let mem = Arc::new(MockMem::new(0x3000));
    let cfg = VirtioNetConfig::new([0x00, 0x11, 0x22, 0x33, 0x44, 0x55]);
    let (backend, _) = RecordBackend::new();
    let dev = VirtioMmioNetDevice::new(
        GuestPhysAddr::from(BASE_IPA),
        REGION_LEN,
        backend,
        cfg,
        (*mem).clone(),
    )
    .unwrap();
    // MAC byte 0 at config offset 0 (byte access).
    let v = dev
        .mmio_read(
            GuestPhysAddr::from(BASE_IPA + vc::VIRTIO_MMIO_CONFIG_OFFSET),
            AccessWidth::Byte,
        )
        .unwrap();
    assert_eq!(v as u8, 0x00);
    // MAC bytes 4..6 as a word at config offset 4.
    let v = dev
        .mmio_read(
            GuestPhysAddr::from(BASE_IPA + vc::VIRTIO_MMIO_CONFIG_OFFSET + 4),
            AccessWidth::Word,
        )
        .unwrap();
    assert_eq!(v as u16, u16::from_le_bytes([0x44, 0x55]));
    // Status (u16) at config offset 6, link up by default.
    let v = dev
        .mmio_read(
            GuestPhysAddr::from(BASE_IPA + vc::VIRTIO_MMIO_CONFIG_OFFSET + 6),
            AccessWidth::Word,
        )
        .unwrap();
    assert_eq!(v as u16, axvirtio_net::VIRTIO_NET_S_LINK_UP);
}

#[test]
fn link_status_change_bumps_generation_and_config_interrupt() {
    let h = Harness::new();
    let gen0 = h.r(vc::VIRTIO_MMIO_CONFIG_GENERATION);
    let ev = h.device.set_link_status(LinkStatus::Down);
    assert_eq!(ev, DeviceEvent::InterruptPending);
    assert!(h.r(vc::VIRTIO_MMIO_INTERRUPT_STATUS) & vc::VIRTIO_MMIO_INT_CONFIG != 0);
    assert_ne!(h.r(vc::VIRTIO_MMIO_CONFIG_GENERATION), gen0);
    // Status field now reports link down.
    let v = h.r(vc::VIRTIO_MMIO_CONFIG_OFFSET + 6);
    assert_eq!(v as u16, 0);
}

#[test]
fn feature_negotiation_rejects_unsupported_bits() {
    let h = Harness::new();
    h.w(
        vc::VIRTIO_MMIO_STATUS,
        vc::VIRTIO_STATUS_ACKNOWLEDGE | vc::VIRTIO_STATUS_DRIVER,
    );
    // Claim an unsupported feature bit in the low word.
    h.w(vc::VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0);
    h.w(vc::VIRTIO_MMIO_DRIVER_FEATURES, 0xffff_ffff);
    h.w(vc::VIRTIO_MMIO_STATUS, vc::VIRTIO_STATUS_FEATURES_OK);
    let status = h.r(vc::VIRTIO_MMIO_STATUS);
    assert_eq!(
        status & vc::VIRTIO_STATUS_FEATURES_OK,
        0,
        "FEATURES_OK must be cleared"
    );
    assert_ne!(status & vc::VIRTIO_STATUS_FAILED, 0, "FAILED must be set");
}

// ---------------------------------------------------------------------------
// TX (13.4, subset)
// ---------------------------------------------------------------------------

#[test]
fn tx_header_split_from_payload_across_descriptors() {
    let h = Harness::new();
    h.bring_up();

    // TX chain: desc0 readable = base header, desc1 readable = payload.
    let payload = [0xde, 0xad, 0xbe, 0xef, 0xc0, 0xfe];
    let hdr_bytes = [0u8; NEGOTIATED_HEADER_SIZE];
    h.write_desc(
        h.tx_desc,
        0,
        h.tx_desc + 0x100,
        NEGOTIATED_HEADER_SIZE as u32,
        vc::VIRTQ_DESC_F_NEXT,
        1,
    );
    h.write_desc(h.tx_desc, 1, h.tx_desc + 0x200, payload.len() as u32, 0, 0);
    h.mem.put(h.tx_desc + 0x100, &hdr_bytes);
    h.mem.put(h.tx_desc + 0x200, &payload);
    h.set_avail(h.tx_avail, 0, 0, 1);

    let ev = h.w(vc::VIRTIO_MMIO_QUEUE_NOTIFY, 1);
    assert_eq!(ev, DeviceEvent::InterruptPending);
    // Backend received exactly the payload (no header).
    let frames = h.backend.frames.lock().unwrap();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0], &payload[..]);
    drop(frames);
    // Used ring: one completion for head 0.
    assert_eq!(h.used_idx(h.tx_used), 1);
    let (id, len) = h.used_elem(h.tx_used, 0);
    assert_eq!(id, 0);
    assert_eq!(len, 0);
}

#[test]
fn tx_version_1_uses_workspace_driver_header() {
    let h = Harness::new();
    h.bring_up();

    // virtio-drivers 0.13.0 emits its 12-byte header after negotiating
    // VERSION_1, even though this device does not advertise MRG_RXBUF.
    let payload = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x52, 0x54];
    let mut combined = vec![0u8; NEGOTIATED_HEADER_SIZE];
    combined.extend_from_slice(&payload);
    h.write_desc(h.tx_desc, 0, h.tx_desc + 0x300, combined.len() as u32, 0, 0);
    h.mem.put(h.tx_desc + 0x300, &combined);
    h.set_avail(h.tx_avail, 0, 0, 1);

    let _ = h.w(vc::VIRTIO_MMIO_QUEUE_NOTIFY, 1);

    let frames = h.backend.frames.lock().unwrap();
    assert_eq!(frames.as_slice(), &[payload.as_slice()]);
}

#[test]
fn tx_unsupported_offload_is_rejected_without_backend_call() {
    let h = Harness::new();
    h.bring_up();

    // Header with a non-zero gso_type requests an unsupported offload.
    let mut hdr_bytes = [0u8; NEGOTIATED_HEADER_SIZE];
    hdr_bytes[1] = 1; // gso_type != NONE
    h.write_desc(
        h.tx_desc,
        0,
        h.tx_desc + 0x100,
        NEGOTIATED_HEADER_SIZE as u32,
        0,
        0,
    );
    h.mem.put(h.tx_desc + 0x100, &hdr_bytes);
    h.set_avail(h.tx_avail, 0, 0, 1);

    let _ = h.w(vc::VIRTIO_MMIO_QUEUE_NOTIFY, 1);
    assert_eq!(
        h.backend.calls(),
        0,
        "backend must not be called for bad header"
    );
    // Head is still consumed and completed with len 0 so the queue advances.
    assert_eq!(h.used_idx(h.tx_used), 1);
}

#[test]
fn tx_writable_descriptor_rejected() {
    let h = Harness::new();
    h.bring_up();
    // A writable-only "TX" buffer is not device-readable -> rejected.
    h.write_desc(
        h.tx_desc,
        0,
        h.tx_desc + 0x100,
        16,
        vc::VIRTQ_DESC_F_WRITE,
        0,
    );
    h.set_avail(h.tx_avail, 0, 0, 1);
    let _ = h.w(vc::VIRTIO_MMIO_QUEUE_NOTIFY, 1);
    assert_eq!(h.backend.calls(), 0);
    assert_eq!(h.used_idx(h.tx_used), 1);
}

#[test]
fn tx_backend_error_still_completes_used() {
    let h = Harness::new();
    h.bring_up();
    h.backend.fail_next();

    let payload = [0xff; 4];
    let mut combined = vec![0u8; NEGOTIATED_HEADER_SIZE];
    combined.extend_from_slice(&payload);
    h.write_desc(h.tx_desc, 0, h.tx_desc + 0x300, combined.len() as u32, 0, 0);
    h.mem.put(h.tx_desc + 0x300, &combined);
    h.set_avail(h.tx_avail, 0, 0, 1);

    let _ = h.w(vc::VIRTIO_MMIO_QUEUE_NOTIFY, 1);
    // Backend refused: nothing recorded ...
    assert_eq!(h.backend.calls(), 0);
    // ... but the head is still completed (len 0) so the guest reclaims the buffer
    // and the queue is not stalled.
    assert_eq!(h.used_idx(h.tx_used), 1);
}

#[test]
fn tx_avail_pre_read_failure_faults_queue_and_stops_drain() {
    let h = Harness::new();
    h.bring_up();
    // Post one valid TX frame first so the TX queue has a non-zero last-consumed
    // index: a silent "empty queue" fallback could otherwise mask the failure
    // by reporting pending == 0.
    let payload = [1, 2, 3, 4];
    let mut frame = vec![0u8; NEGOTIATED_HEADER_SIZE];
    frame.extend_from_slice(&payload);
    h.write_desc(h.tx_desc, 0, h.tx_desc + 0x100, frame.len() as u32, 0, 0);
    h.mem.put(h.tx_desc + 0x100, &frame);
    h.set_avail(h.tx_avail, 0, 0, 1);
    assert_eq!(
        h.w(vc::VIRTIO_MMIO_QUEUE_NOTIFY, 1),
        DeviceEvent::InterruptPending
    );
    assert_eq!(h.backend.calls(), 1, "sanity: one TX frame is transmitted");
    assert_eq!(
        h.used_idx(h.tx_used),
        1,
        "sanity: the TX frame is completed"
    );
    // A second request is now visible (avail.idx ahead of last-consumed index 1).
    h.set_avail(h.tx_avail, 0, 0, 2);
    // Rewrite the TX avail-ring address to a region whose `idx` field (base + 2)
    // is unmapped (well beyond the mock backing). The address write alone keeps
    // the queue ready; the layout check runs on QUEUE_READY, not on address
    // writes.
    h.w(vc::VIRTIO_MMIO_QUEUE_SEL, 1);
    h.w(vc::VIRTIO_MMIO_QUEUE_AVAIL_LOW, 0xffff_fffe);
    h.w(vc::VIRTIO_MMIO_QUEUE_AVAIL_HIGH, 0);

    // The pre-read fails: the queue is faulted and the drain must not silently
    // treat the queue as empty.
    let ev = h.w(vc::VIRTIO_MMIO_QUEUE_NOTIFY, 1);
    assert_eq!(ev, DeviceEvent::None);
    assert_eq!(
        h.backend.calls(),
        1,
        "no TX may be transmitted from a faulted queue"
    );
    // No completion is written and the queue is not consumed.
    assert_eq!(h.used_idx(h.tx_used), 1);
    // The queue is latched into the faulted state until the guest resets the
    // device; the TX path observes it.
    assert!(
        h.device.is_queue_faulted(1),
        "the unreadable avail ring must latch the queue faulted"
    );
    // Every later kick repeats the same rejection instead of re-failing per
    // entry: the queue stays faulted until the guest resets the device.
    assert_eq!(h.w(vc::VIRTIO_MMIO_QUEUE_NOTIFY, 1), DeviceEvent::None);
    assert_eq!(h.used_idx(h.tx_used), 1);
    assert_eq!(h.backend.calls(), 1);
    // A guest reset clears the fault and the queue serves requests again.
    h.w(vc::VIRTIO_MMIO_STATUS, 0);
    assert!(!h.device.is_queue_faulted(1));
}

// ---------------------------------------------------------------------------
// RX (13.5, subset)
// ---------------------------------------------------------------------------

#[test]
fn rx_no_guest_buffer_is_flow_control() {
    let h = Harness::new();
    h.bring_up();
    // No RX buffer posted.
    let outcome = h.device.receive_frame(&[1, 2, 3]).unwrap();
    assert_eq!(outcome, RxOutcome::NoGuestBuffer);
    // Ring untouched.
    assert_eq!(h.used_idx(h.rx_used), 0);
}

#[test]
fn rx_delivers_header_plus_frame() {
    let h = Harness::new();
    h.bring_up();
    let frame = [0xaa, 0xbb, 0xcc, 0xdd];
    // RX buffer: one writable descriptor holding header + frame.
    h.write_desc(
        h.rx_desc,
        0,
        h.rx_desc + 0x100,
        (NEGOTIATED_HEADER_SIZE + frame.len()) as u32,
        vc::VIRTQ_DESC_F_WRITE,
        0,
    );
    h.set_avail(h.rx_avail, 0, 0, 1);

    let outcome = h.device.receive_frame(&frame).unwrap();
    assert_eq!(
        outcome,
        RxOutcome::Delivered {
            frame_len: frame.len(),
            notify: true,
        }
    );

    // Guest memory: 10 zero header bytes followed by the frame.
    let got = h
        .mem
        .peek(h.rx_desc + 0x100, NEGOTIATED_HEADER_SIZE + frame.len());
    assert!(got[..NEGOTIATED_HEADER_SIZE].iter().all(|&b| b == 0));
    assert_eq!(&got[NEGOTIATED_HEADER_SIZE..], &frame[..]);

    // Used: written length is header + frame, and an interrupt is pending.
    let (id, len) = h.used_elem(h.rx_used, 0);
    assert_eq!(id, 0);
    assert_eq!(len as usize, NEGOTIATED_HEADER_SIZE + frame.len());
    assert!(h.r(vc::VIRTIO_MMIO_INTERRUPT_STATUS) & vc::VIRTIO_MMIO_INT_VRING != 0);
}

#[test]
fn rx_no_interrupt_flag_is_reported_to_runtime() {
    let h = Harness::new();
    h.bring_up();
    let frame = [0xaa, 0xbb, 0xcc, 0xdd];
    h.write_desc(
        h.rx_desc,
        0,
        h.rx_desc + 0x100,
        (NEGOTIATED_HEADER_SIZE + frame.len()) as u32,
        vc::VIRTQ_DESC_F_WRITE,
        0,
    );
    h.set_avail(h.rx_avail, 0, 0, 1);
    h.mem
        .put(h.rx_avail, &vc::VIRTQ_AVAIL_F_NO_INTERRUPT.to_le_bytes());

    let outcome = h.device.receive_frame(&frame).unwrap();

    assert_eq!(
        outcome,
        RxOutcome::Delivered {
            frame_len: frame.len(),
            notify: false,
        }
    );
    assert_eq!(h.used_idx(h.rx_used), 1);
    assert_eq!(h.r(vc::VIRTIO_MMIO_INTERRUPT_STATUS), 0);
}

#[test]
fn rx_too_small_buffer_does_not_advance_ring() {
    let h = Harness::new();
    h.bring_up();
    let frame = [0; 32];
    // Buffer holds header but is 1 byte short of header + frame.
    h.write_desc(
        h.rx_desc,
        0,
        h.rx_desc + 0x100,
        (NEGOTIATED_HEADER_SIZE + frame.len() - 1) as u32,
        vc::VIRTQ_DESC_F_WRITE,
        0,
    );
    h.set_avail(h.rx_avail, 0, 0, 1);

    let err = h.device.receive_frame(&frame).unwrap_err();
    assert!(matches!(err, NetError::FrameTooLarge), "got {err:?}");
    // Ring must not have advanced.
    assert_eq!(h.used_idx(h.rx_used), 0);
}

#[test]
fn rx_readable_descriptor_rejected() {
    let h = Harness::new();
    h.bring_up();
    // A non-writable (readable) RX buffer is invalid.
    h.write_desc(h.rx_desc, 0, h.rx_desc + 0x100, 64, 0, 0);
    h.set_avail(h.rx_avail, 0, 0, 1);
    let err = h.device.receive_frame(&[0; 4]).unwrap_err();
    assert!(matches!(err, NetError::InvalidDescriptor), "got {err:?}");
    assert_eq!(h.used_idx(h.rx_used), 0);
}

#[test]
fn rx_avail_pre_read_failure_faults_queue_and_rejects_frames() {
    let h = Harness::new();
    h.bring_up();
    // Rewrite the RX avail-ring address to a region whose `idx` field (base + 2)
    // is unmapped. The address write alone keeps the queue ready; the layout
    // check runs on QUEUE_READY, not on address writes.
    h.w(vc::VIRTIO_MMIO_QUEUE_SEL, 0);
    h.w(vc::VIRTIO_MMIO_QUEUE_AVAIL_LOW, 0xffff_fffe);
    h.w(vc::VIRTIO_MMIO_QUEUE_AVAIL_HIGH, 0);
    // The queue stays faulted: `receive_frame` must return an error instead of
    // silently treating the unreadable ring as empty. (The exact variant is the
    // pre-existing `VirtioError -> NetError` mapping; the guarantee under test
    // is the error itself plus the latch.)
    let err = h.device.receive_frame(&[1, 2, 3]).unwrap_err();
    assert!(
        matches!(err, NetError::Queue(_) | NetError::GuestMemoryFault),
        "got {err:?}"
    );
    assert_eq!(h.used_idx(h.rx_used), 0, "no RX completion may be written");
    // The unreadable avail ring latches the queue faulted; the RX path observes
    // it (the old behavior never latched and only reported flow control).
    assert!(
        h.device.is_queue_faulted(0),
        "the unreadable avail ring must latch the queue faulted"
    );
    // Every later delivery repeats the same rejection.
    let err = h.device.receive_frame(&[4, 5, 6]).unwrap_err();
    assert!(
        matches!(err, NetError::Queue(_) | NetError::GuestMemoryFault),
        "got {err:?}"
    );
    assert_eq!(h.used_idx(h.rx_used), 0);
    // A guest reset clears the fault and the queue accepts deliveries again.
    h.w(vc::VIRTIO_MMIO_STATUS, 0);
    assert!(!h.device.is_queue_faulted(0));
}

// ---------------------------------------------------------------------------
// End-to-end guest sequence (13.6)
// ---------------------------------------------------------------------------

#[test]
fn end_to_end_guest_tx_rx_ack_reset() {
    let h = Harness::new();

    // 1. Identity.
    assert_eq!(h.r(vc::VIRTIO_MMIO_MAGIC_VALUE), vc::MMIO_MAGIC_VALUE);
    assert_eq!(h.r(vc::VIRTIO_MMIO_DEVICE_ID), 1);

    // 2-7. Bring up the guest driver and queues.
    h.bring_up();
    assert_ne!(h.r(vc::VIRTIO_MMIO_STATUS) & vc::VIRTIO_STATUS_DRIVER_OK, 0);
    assert_ne!(
        h.r(vc::VIRTIO_MMIO_STATUS) & vc::VIRTIO_STATUS_FEATURES_OK,
        0
    );

    // 8-9. TX: submit a frame and notify queue 1.
    let payload = [1, 2, 3, 4, 5, 6, 7, 8];
    let hdr = [0u8; NEGOTIATED_HEADER_SIZE];
    let mut combined = Vec::new();
    combined.extend_from_slice(&hdr);
    combined.extend_from_slice(&payload);
    h.write_desc(h.tx_desc, 0, h.tx_desc + 0x300, combined.len() as u32, 0, 0);
    h.mem.put(h.tx_desc + 0x300, &combined);
    h.set_avail(h.tx_avail, 0, 0, 1);
    assert_eq!(
        h.w(vc::VIRTIO_MMIO_QUEUE_NOTIFY, 1),
        DeviceEvent::InterruptPending
    );
    {
        let frames = h.backend.frames.lock().unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], &payload[..]);
    }

    // 10. ACK the TX interrupt.
    assert_ne!(
        h.r(vc::VIRTIO_MMIO_INTERRUPT_STATUS) & vc::VIRTIO_MMIO_INT_VRING,
        0
    );
    h.w(vc::VIRTIO_MMIO_INTERRUPT_ACK, vc::VIRTIO_MMIO_INT_VRING);
    assert_eq!(h.r(vc::VIRTIO_MMIO_INTERRUPT_STATUS), 0);

    // 11-12. RX: post a buffer and deliver a frame from the host.
    let rx_frame = [9, 9, 9, 9];
    h.write_desc(
        h.rx_desc,
        0,
        h.rx_desc + 0x300,
        (NEGOTIATED_HEADER_SIZE + rx_frame.len()) as u32,
        vc::VIRTQ_DESC_F_WRITE,
        0,
    );
    h.set_avail(h.rx_avail, 0, 0, 1);
    let outcome = h.device.receive_frame(&rx_frame).unwrap();
    assert_eq!(
        outcome,
        RxOutcome::Delivered {
            frame_len: rx_frame.len(),
            notify: true,
        }
    );
    let got = h
        .mem
        .peek(h.rx_desc + 0x300, NEGOTIATED_HEADER_SIZE + rx_frame.len());
    assert_eq!(&got[NEGOTIATED_HEADER_SIZE..], &rx_frame[..]);
    assert!(h.r(vc::VIRTIO_MMIO_INTERRUPT_STATUS) & vc::VIRTIO_MMIO_INT_VRING != 0);

    // 13. Reset.
    assert_eq!(h.w(vc::VIRTIO_MMIO_STATUS, 0), DeviceEvent::Reset);
    assert_eq!(h.r(vc::VIRTIO_MMIO_STATUS), 0);
    assert_eq!(h.r(vc::VIRTIO_MMIO_INTERRUPT_STATUS), 0);
}

struct NoopIrqSink;

impl WiredIrqSink for NoopIrqSink {
    fn set_level(&self, _input: ControllerInputId, _asserted: bool) -> IrqResult {
        Ok(())
    }

    fn pulse(&self, _input: ControllerInputId) -> IrqResult {
        Ok(())
    }
}

#[test]
fn managed_device_declares_resources_and_routes_mmio() {
    let mem = Arc::new(MockMem::new(0x1_0000));
    let (backend, _) = RecordBackend::new();
    let model = Arc::new(
        VirtioMmioNetDevice::new(
            GuestPhysAddr::from(BASE_IPA),
            REGION_LEN,
            backend,
            VirtioNetConfig::new([0x02, 0, 0, 0, 0, 1]),
            SharedMem(mem),
        )
        .unwrap(),
    );
    let irq = WiredIrqInput::new(
        InterruptControllerId::new(0),
        ControllerInputId::new(48),
        InterruptTriggerMode::EdgeTriggered,
        Arc::new(NoopIrqSink),
    )
    .connect()
    .unwrap();
    let device = ManagedVirtioNetDevice::new(
        "virtio-net0".into(),
        model,
        irq,
        BASE_IPA as u64,
        REGION_LEN as u64,
        48,
    );

    assert_eq!(
        device.resources(),
        &[
            Resource::MmioRange {
                base: BASE_IPA as u64,
                size: REGION_LEN as u64,
            },
            Resource::IrqLine {
                line: 48,
                trigger: InterruptTriggerMode::EdgeTriggered,
            },
        ]
    );
    let mut context = NoopDeviceContext::new(DeviceId::new(0));
    let value = device
        .read(
            &DeviceAccess::new(
                DeviceVcpuId::new(0),
                BusKind::Mmio,
                BASE_IPA as u64,
                AccessWidth::Dword,
            ),
            &mut context,
        )
        .unwrap();
    assert_eq!(value, vc::MMIO_MAGIC_VALUE as u64);
}
