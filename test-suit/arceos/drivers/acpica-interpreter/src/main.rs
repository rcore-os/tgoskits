#![no_std]
#![no_main]

extern crate ax_std as std;

use core::{
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};
use std::{boxed::Box, sync::Arc, thread, vec, vec::Vec};

use acpica_interpreter::{
    ALREADY_EXISTS, Engine, Mode, NO_MEMORY, OK, SUPPORT, Status, aml_error_count,
    backend::{Backend, BackendRegistration, IrqHandler, PciId, Work},
};
use ax_hal::mem::{VirtAddr, virt_to_phys};

static NOTIFY_ENTERED: AtomicBool = AtomicBool::new(false);
static RELEASE_NOTIFY: AtomicBool = AtomicBool::new(false);
static NOTIFY_PATH_MATCH: AtomicBool = AtomicBool::new(false);
static NOTIFY_VALUE: AtomicUsize = AtomicUsize::new(0);
static QUIESCE_ARMED: AtomicBool = AtomicBool::new(false);
static QUIESCE_ENTERED: AtomicBool = AtomicBool::new(false);

struct FirmwareTable {
    storage: Box<[u64]>,
    len: usize,
    physical: u64,
}

impl FirmwareTable {
    fn new(bytes: &[u8]) -> Self {
        let storage = bytes
            .chunks(size_of::<u64>())
            .map(|chunk| {
                let mut word = [0; size_of::<u64>()];
                word[..chunk.len()].copy_from_slice(chunk);
                u64::from_ne_bytes(word)
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let virtual_address = VirtAddr::from_ptr_of(storage.as_ptr());
        let physical = virt_to_phys(virtual_address).as_usize() as u64;
        Self {
            storage,
            len: bytes.len(),
            physical,
        }
    }

    fn virtual_base(&self) -> *mut u8 {
        self.storage.as_ptr().cast::<u8>().cast_mut()
    }
}

fn sdt(signature: &[u8; 4], mut bytes: Vec<u8>) -> Vec<u8> {
    assert!(bytes.len() >= 36);
    bytes[..4].copy_from_slice(signature);
    let length = bytes.len() as u32;
    bytes[4..8].copy_from_slice(&length.to_le_bytes());
    bytes[8] = 6;
    bytes[10..16].copy_from_slice(b"TKQEMU");
    bytes[16..24].copy_from_slice(b"ACPICASE");
    bytes[9] = 0;
    bytes[9] = 0u8.wrapping_sub(bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)));
    bytes
}

struct GuestBackend {
    root: u64,
    tables: Vec<FirmwareTable>,
    active_work: Arc<AtomicUsize>,
}

impl GuestBackend {
    fn synthetic() -> Self {
        let dsdt = FirmwareTable::new(include_bytes!(
            "../../../../../drivers/firmware/acpica-interpreter/tests/synthetic.aml"
        ));

        let mut fadt = vec![0; 276];
        fadt[112..116].copy_from_slice(&(1u32 << 20).to_le_bytes());
        fadt[140..148].copy_from_slice(&dsdt.physical.to_le_bytes());
        let fadt = FirmwareTable::new(&sdt(b"FACP", fadt));

        let mut xsdt = vec![0; 36];
        xsdt.extend_from_slice(&fadt.physical.to_le_bytes());
        let xsdt = FirmwareTable::new(&sdt(b"XSDT", xsdt));

        let mut rsdp = vec![0u8; 36];
        rsdp[..8].copy_from_slice(b"RSD PTR ");
        rsdp[15] = 2;
        rsdp[20..24].copy_from_slice(&36u32.to_le_bytes());
        rsdp[24..32].copy_from_slice(&xsdt.physical.to_le_bytes());
        rsdp[8] = 0u8.wrapping_sub(
            rsdp[..20]
                .iter()
                .fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
        );
        rsdp[32] = 0u8.wrapping_sub(rsdp.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)));
        let rsdp = FirmwareTable::new(&rsdp);
        let root = rsdp.physical;

        Self {
            root,
            tables: vec![dsdt, fadt, xsdt, rsdp],
            active_work: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn work_count(&self) -> usize {
        self.active_work.load(Ordering::Acquire)
    }

    fn drain_work(&self) {
        while self.active_work.load(Ordering::Acquire) != 0 {
            thread::yield_now();
        }
    }

    fn table_contains_virtual(&self, address: *mut c_void, size: usize) -> bool {
        let start = address as usize;
        let Some(end) = start.checked_add(size) else {
            return false;
        };
        self.tables.iter().any(|table| {
            let base = table.virtual_base() as usize;
            base.checked_add(table.len)
                .is_some_and(|table_end| start >= base && end <= table_end)
        })
    }
}

// SAFETY: this test backend maps only aligned, immutable ACPI tables allocated
// by ArceOS. Unknown physical mappings and every hardware I/O path fail closed;
// deferred work runs as an ArceOS task and is drained before teardown.
unsafe impl Backend for GuestBackend {
    fn root_pointer(&self) -> u64 {
        self.root
    }

    fn map(&self, address: u64, size: usize) -> *mut c_void {
        let Ok(size) = u64::try_from(size) else {
            return core::ptr::null_mut();
        };
        let Some(end) = address.checked_add(size) else {
            return core::ptr::null_mut();
        };
        if size == 0 {
            return core::ptr::null_mut();
        }
        for table in &self.tables {
            let Some(table_end) = table.physical.checked_add(table.len as u64) else {
                continue;
            };
            if address >= table.physical && end <= table_end {
                let offset = (address - table.physical) as usize;
                return table.virtual_base().wrapping_add(offset).cast();
            }
        }
        core::ptr::null_mut()
    }

    unsafe fn unmap(&self, _address: *mut c_void, _size: usize) {}

    fn physical_address(&self, address: *mut c_void) -> Result<u64, Status> {
        let pointer = address as usize;
        self.tables
            .iter()
            .find_map(|table| {
                let base = table.virtual_base() as usize;
                let end = base.checked_add(table.len)?;
                (pointer >= base && pointer < end)
                    .then(|| table.physical.checked_add((pointer - base) as u64))
                    .flatten()
            })
            .ok_or(SUPPORT)
    }

    fn readable(&self, address: *mut c_void, size: usize) -> bool {
        self.table_contains_virtual(address, size)
    }

    fn writable(&self, _address: *mut c_void, _size: usize) -> bool {
        false
    }

    fn timer_100ns(&self) -> u64 {
        ax_hal::time::monotonic_time_nanos() / 100
    }

    fn sleep(&self, millis: u64) -> bool {
        thread::sleep(Duration::from_millis(millis));
        true
    }

    fn stall(&self, micros: u32) {
        ax_hal::time::busy_wait(Duration::from_micros(u64::from(micros)));
    }

    fn thread_id(&self) -> u64 {
        thread::current().id().as_u64().get()
    }

    fn irq_save(&self) -> usize {
        let was_enabled = ax_hal::cpu::interrupt::irqs_enabled();
        ax_hal::cpu::interrupt::disable_irqs();
        usize::from(was_enabled)
    }

    fn irq_restore(&self, flags: usize) {
        if flags != 0 {
            ax_hal::cpu::interrupt::enable_irqs();
        }
    }

    unsafe fn execute(&self, _kind: u32, function: Work, context: *mut c_void) -> Status {
        let active = Arc::clone(&self.active_work);
        active.fetch_add(1, Ordering::AcqRel);
        let context = context as usize;
        match thread::Builder::new().spawn(move || {
            // SAFETY: ACPICA retains the callback context until this deferred
            // task returns; its ownership is covered by Engine teardown.
            unsafe { function(context as *mut c_void) };
            active.fetch_sub(1, Ordering::Release);
        }) {
            Ok(_) => OK,
            Err(_) => {
                self.active_work.fetch_sub(1, Ordering::Release);
                NO_MEMORY
            }
        }
    }

    fn wait_events(&self) {
        self.drain_work();
    }

    fn quiesce(&self) {
        if QUIESCE_ARMED.swap(false, Ordering::AcqRel) {
            QUIESCE_ENTERED.store(true, Ordering::Release);
        }
        self.drain_work();
    }

    unsafe fn remove_irq(&self, _irq: u32, _handler: IrqHandler) -> Status {
        // Offline initialization skips SCI registration, but ACPICA calls the
        // OSL removal hook unconditionally during termination.
        OK
    }

    fn read_port(&self, _address: u16, _width: u32) -> Result<u32, Status> {
        Err(SUPPORT)
    }

    fn write_port(&self, _address: u16, _width: u32, _value: u32) -> Status {
        SUPPORT
    }

    fn read_pci(&self, _id: PciId, _reg: u32, _width: u32) -> Result<u64, Status> {
        Err(SUPPORT)
    }

    fn write_pci(&self, _id: PciId, _reg: u32, _width: u32, _value: u64) -> Status {
        SUPPORT
    }

    fn log(&self, message: &[u8]) {
        if let Ok(message) = core::str::from_utf8(message) {
            std::println!("ACPICA: {message}");
        }
    }
}

fn notification(path: &str, value: u32) {
    NOTIFY_ENTERED.store(true, Ordering::Release);
    while !RELEASE_NOTIFY.load(Ordering::Acquire) {
        thread::yield_now();
    }
    NOTIFY_PATH_MATCH.store(path == "\\_SB_.PWRB", Ordering::Release);
    NOTIFY_VALUE.store(value as usize, Ordering::Release);
}

#[unsafe(no_mangle)]
fn main() {
    let backend: &'static GuestBackend = Box::leak(Box::new(GuestBackend::synthetic()));
    let registration: &'static BackendRegistration =
        Box::leak(Box::new(BackendRegistration(backend)));

    // SAFETY: ArceOS has initialized its real allocator, scheduler, timer and
    // interrupt state. The backend exposes only the aligned authored tables,
    // and this fixture has no hardware operation regions or side-effecting I/O.
    let mut engine = unsafe { Engine::initialize(registration, Mode::Offline) }
        .expect("ACPICA offline guest initialization");

    let mut found_dsdt = false;
    for index in 0..engine.table_count().expect("ACPICA table count") {
        found_dsdt |= engine
            .table(index)
            .expect("ACPICA owned table copy")
            .starts_with(b"DSDT");
    }
    assert!(found_dsdt, "ACPICA_TEST_FAIL synthetic DSDT was not loaded");
    assert_eq!(engine.hardware_id("\\_SB.PWRB").unwrap(), "PNP0C0C");

    engine.install_notify(notification).unwrap();
    engine
        .initialize_objects()
        .expect("ACPICA _INI initialization");
    assert_eq!(engine.integer("\\_SB.PWRB.ICNT").unwrap(), 1);
    let repeated_initialization = engine.initialize_objects();
    let initialization_count = engine.integer("\\_SB.PWRB.ICNT").unwrap();
    assert_eq!(
        (repeated_initialization, initialization_count),
        (Err(ALREADY_EXISTS), 1),
        "ACPICA_TEST_FAIL firmware _INI must not be repeated"
    );

    let raw_resources = engine.resources("\\_SB.PWRB", false).unwrap();
    let resources = acpica_interpreter::resources::parse(&raw_resources).unwrap();
    assert_eq!(resources.io, [(0x62, 1), (0x66, 1)]);
    assert_eq!(resources.irqs.len(), 1);
    assert_eq!(resources.irqs[0].numbers, [9]);

    engine.evaluate("\\_SB.PWRB.TEST", &[]).unwrap();
    let release = thread::Builder::new()
        .spawn(|| {
            while !(NOTIFY_ENTERED.load(Ordering::Acquire)
                && QUIESCE_ENTERED.load(Ordering::Acquire))
            {
                thread::yield_now();
            }
            RELEASE_NOTIFY.store(true, Ordering::Release);
        })
        .expect("deferred notification release task");
    QUIESCE_ARMED.store(true, Ordering::Release);
    drop(engine);
    release.join().expect("notification release task");

    assert!(NOTIFY_PATH_MATCH.load(Ordering::Acquire));
    assert_eq!(NOTIFY_VALUE.load(Ordering::Acquire), 0x80);
    assert_eq!(backend.work_count(), 0);
    assert_eq!(
        aml_error_count(),
        0,
        "ACPICA_TEST_FAIL unexpected ACPICA error"
    );
    std::println!("ACPICA_QEMU_INTEGRATION_OK");
}
