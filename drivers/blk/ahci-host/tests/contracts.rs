use core::{alloc::Layout, ptr::NonNull};
use std::sync::{Mutex, MutexGuard, OnceLock};

use ahci_host::{AhciConfig, AhciHost};
use dma_api::{DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp};
use mmio_api::{MapError, MmioAddr, MmioOp, MmioRaw};
use rdif_block::ControllerInitEndpoint;

const MMIO_SIZE: usize = 0x1_100;

#[test]
fn discovery_only_masks_controller_and_implemented_port_interrupts() {
    let fixture = Fixture::new();
    fixture.write32(0x00, 0x8000_1f00);
    fixture.write32(0x04, (1 << 31) | (1 << 1) | 1);
    fixture.write32(0x08, 0xa5a5_5a5a);
    fixture.write32(0x0c, 1 << 2);
    fixture.write32(0x100 + 2 * 0x80 + 0x10, 0x55aa_aa55);
    fixture.write32(0x100 + 2 * 0x80 + 0x14, u32::MAX);
    fixture.write32(0x100 + 2 * 0x80 + 0x2c, 0x321);
    fixture.write32(0x100 + 2 * 0x80 + 0x38, 0x4);
    let before = fixture.snapshot();

    let host = fixture.discover();

    let after = fixture.snapshot();
    assert_eq!(fixture.read32(0x04), 1 << 31);
    assert_eq!(fixture.read32(0x100 + 2 * 0x80 + 0x14), 0);
    assert_eq!(fixture.read32(0x08), 0xa5a5_5a5a);
    assert_eq!(fixture.read32(0x100 + 2 * 0x80 + 0x10), 0x55aa_aa55);
    assert_eq!(fixture.read32(0x100 + 2 * 0x80 + 0x2c), 0x321);
    assert_eq!(fixture.read32(0x100 + 2 * 0x80 + 0x38), 0x4);
    for offset in (0..MMIO_SIZE).step_by(4) {
        if matches!(offset, 0x04 | 0x214) {
            continue;
        }
        assert_eq!(
            &after[offset..offset + 4],
            &before[offset..offset + 4],
            "discovery changed non-IRQ register at offset {offset:#x}",
        );
    }
    assert!(matches!(
        host.controller_init_state(),
        ahci_host::ControllerInitState::Discovered
    ));
}

#[test]
fn discovered_hardware_requires_an_irq_bound_initialization_endpoint() {
    let fixture = Fixture::new();
    let mut host = fixture.discover();

    let ControllerInitEndpoint::Pending(initializer) = host.controller_init() else {
        panic!("AHCI discovery must not publish a ready controller");
    };

    assert!(initializer.irq_sources().contains(0));
    assert!(initializer.take_irq_source(0).is_some());
}

#[test]
fn portable_driver_has_no_completion_polling_or_os_scheduler_dependency() {
    let source = concat!(
        include_str!("../src/controller.rs"),
        include_str!("../src/initialization.rs"),
        include_str!("../src/irq.rs"),
        include_str!("../src/lifecycle.rs"),
        include_str!("../src/quarantine.rs"),
        include_str!("../src/queue.rs"),
    );
    for forbidden in [
        concat!("poll_", "completions"),
        concat!("poll_", "request"),
        concat!("spin_", "loop"),
        concat!("thread::", "sleep"),
        concat!("ax_", "task"),
        concat!("ax_", "runtime"),
    ] {
        assert!(
            !source.contains(forbidden),
            "portable AHCI source contains forbidden completion/runtime path: {forbidden}",
        );
    }
}

#[test]
fn live_dma_retention_has_one_named_quarantine_owner() {
    let source = concat!(
        include_str!("../src/controller.rs"),
        include_str!("../src/initialization.rs"),
        include_str!("../src/quarantine.rs"),
        include_str!("../src/queue.rs"),
    );

    for forbidden in ["mem::forget", "core::mem::forget", "Box::leak"] {
        assert!(
            !source.contains(forbidden),
            "AHCI production ownership must not disappear through `{forbidden}`",
        );
    }
    assert!(
        source.contains("AhciDmaQuarantine"),
        "unproven DMA shutdown must move resources into one diagnosable quarantine type",
    );
    assert!(
        source.contains("ManuallyDrop<PortCommandMemory>"),
        "quarantined command memory must retain explicit Rust ownership",
    );
}

#[test]
fn queue_shutdown_cannot_publish_request_ownership() {
    let queue = include_str!("../src/queue.rs");

    assert!(queue.contains("fn shutdown(&mut self) -> Result<(), BlkError>"));
    assert!(!queue.contains("fn shutdown(&mut self,"));
}

struct Fixture {
    _serial: MutexGuard<'static, ()>,
    dma: &'static TestDma,
    mmio: &'static TestMmio,
}

impl Fixture {
    fn new() -> Self {
        let serial = TEST_SERIAL.lock().unwrap();
        let dma = Box::leak(Box::new(TestDma));
        let mmio = TEST_MMIO.get_or_init(TestMmio::new);
        mmio.reset();
        Self {
            _serial: serial,
            dma,
            mmio,
        }
    }

    fn discover(&self) -> AhciHost {
        AhciHost::discover(
            "test-ahci",
            MmioAddr::from(0x1000_u64),
            MMIO_SIZE,
            u64::MAX,
            self.dma,
            self.mmio,
            AhciConfig::legacy_irq(0),
        )
        .expect("fixture discovery must succeed")
    }

    fn write32(&self, offset: usize, value: u32) {
        self.mmio.write32(offset, value);
    }

    fn read32(&self, offset: usize) -> u32 {
        self.mmio.read32(offset)
    }

    fn snapshot(&self) -> Vec<u8> {
        self.mmio.snapshot()
    }
}

static TEST_MMIO: OnceLock<TestMmio> = OnceLock::new();
static TEST_SERIAL: Mutex<()> = Mutex::new(());

struct TestMmio {
    bytes: Mutex<Box<[u8]>>,
}

impl TestMmio {
    fn new() -> Self {
        Self {
            bytes: Mutex::new(vec![0; MMIO_SIZE].into_boxed_slice()),
        }
    }

    fn reset(&self) {
        self.bytes.lock().unwrap().fill(0);
    }

    fn write32(&self, offset: usize, value: u32) {
        self.bytes.lock().unwrap()[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn read32(&self, offset: usize) -> u32 {
        let bytes = self.bytes.lock().unwrap();
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn snapshot(&self) -> Vec<u8> {
        self.bytes.lock().unwrap().to_vec()
    }
}

impl MmioOp for TestMmio {
    fn ioremap(&self, addr: MmioAddr, size: usize) -> Result<MmioRaw, MapError> {
        let mut bytes = self.bytes.lock().unwrap();
        if size > bytes.len() {
            return Err(MapError::Invalid);
        }
        let ptr = NonNull::new(bytes.as_mut_ptr()).ok_or(MapError::Invalid)?;
        // SAFETY: the static fixture retains this allocation for the complete
        // mapped-controller lifetime and serializes test reset operations.
        Ok(unsafe { MmioRaw::new(addr, ptr, size) })
    }

    fn iounmap(&self, _mmio: &MmioRaw) {}
}

struct TestDma;

impl DmaOp for TestDma {
    fn page_size(&self) -> usize {
        4096
    }

    unsafe fn alloc_contiguous(
        &self,
        _constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        allocate(layout)
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        unsafe { std::alloc::dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn alloc_coherent(
        &self,
        _constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        allocate(layout)
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) {
        unsafe { std::alloc::dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn map_streaming(
        &self,
        _constraints: DmaConstraints,
        addr: NonNull<u8>,
        size: core::num::NonZeroUsize,
        _direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        let layout = Layout::from_size_align(size.get(), 1)?;
        Ok(unsafe {
            DmaMapHandle::new(
                addr,
                dma_api::DmaAddr::from(addr.as_ptr() as u64),
                layout,
                None,
            )
        })
    }

    unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
}

fn allocate(layout: Layout) -> Option<DmaAllocHandle> {
    let ptr = NonNull::new(unsafe { std::alloc::alloc_zeroed(layout) })?;
    Some(unsafe { DmaAllocHandle::new(ptr, dma_api::DmaAddr::from(ptr.as_ptr() as u64), layout) })
}
