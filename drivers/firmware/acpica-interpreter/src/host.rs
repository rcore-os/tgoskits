//! Offline-only, simulated hardware backend for authored AML tests.
//! Never maps host physical memory or accesses host ports/PCI.
use std::{
    boxed::Box,
    collections::BTreeMap,
    ffi::c_void,
    string::String,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
    vec::Vec,
};

use crate::{
    BAD_PARAMETER, LIMIT, OK, Status,
    backend::{Backend, BackendRegistration, PciId, Work},
};
static START: OnceLock<Instant> = OnceLock::new();
type SimulatedRegions = Mutex<BTreeMap<(u64, usize), Box<[u64]>>>;

struct FirmwareTable {
    storage: Box<[u64]>,
    len: usize,
    signature: [u8; 4],
}

impl FirmwareTable {
    fn from_bytes(bytes: &[u8]) -> Self {
        debug_assert!(bytes.len() >= 36);
        let signature = bytes[..4].try_into().unwrap();
        let storage = bytes
            .chunks(size_of::<u64>())
            .map(|chunk| {
                let mut word = [0; size_of::<u64>()];
                word[..chunk.len()].copy_from_slice(chunk);
                u64::from_ne_bytes(word)
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            storage,
            len: bytes.len(),
            signature,
        }
    }

    fn address(&self) -> u64 {
        self.storage.as_ptr() as u64
    }
}

pub struct OfflineBackend {
    root: u64,
    tables: Vec<FirmwareTable>,
    regions: SimulatedRegions,
    active: Arc<AtomicUsize>,
}
fn table(signature: &[u8; 4], mut bytes: Vec<u8>) -> Box<[u8]> {
    bytes[..4].copy_from_slice(signature);
    let len = bytes.len() as u32;
    bytes[4..8].copy_from_slice(&len.to_le_bytes());
    bytes[8] = 6;
    bytes[10..16].copy_from_slice(b"TKTEST");
    bytes[16..24].copy_from_slice(b"OFFLINE ");
    bytes[9] = 0;
    bytes[9] = 0u8.wrapping_sub(bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b)));
    bytes.into_boxed_slice()
}
impl OfflineBackend {
    pub fn from_tables(aml: Vec<Box<[u8]>>) -> Result<Self, std::io::Error> {
        if aml.iter().any(|table| table.len() < 36) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "ACPI SDT is shorter than its header",
            ));
        }
        let mut aml = aml
            .into_iter()
            .map(|table| FirmwareTable::from_bytes(&table))
            .collect::<Vec<_>>();
        let dsdt = aml
            .iter()
            .find(|table| &table.signature == b"DSDT")
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "DSDT missing"))?;
        let mut fadt = std::vec![0;276];
        fadt[140..148].copy_from_slice(&dsdt.address().to_le_bytes());
        fadt[112..116].copy_from_slice(&(1u32 << 20).to_le_bytes());
        let fadt = FirmwareTable::from_bytes(&table(b"FACP", fadt));
        let addresses = std::iter::once(fadt.address()).chain(
            aml.iter()
                .filter(|table| &table.signature == b"SSDT")
                .map(FirmwareTable::address),
        );
        let mut xsdt = std::vec![0;36];
        for address in addresses {
            xsdt.extend_from_slice(&address.to_le_bytes());
        }
        let xsdt = FirmwareTable::from_bytes(&table(b"XSDT", xsdt));
        let mut rsdp = std::vec![0u8;36];
        rsdp[..8].copy_from_slice(b"RSD PTR ");
        rsdp[15] = 2;
        rsdp[20..24].copy_from_slice(&36u32.to_le_bytes());
        rsdp[24..32].copy_from_slice(&xsdt.address().to_le_bytes());
        rsdp[8] = 0u8.wrapping_sub(rsdp[..20].iter().fold(0u8, |a, b| a.wrapping_add(*b)));
        rsdp[32] = 0u8.wrapping_sub(rsdp.iter().fold(0u8, |a, b| a.wrapping_add(*b)));
        let rsdp = FirmwareTable::from_bytes(&rsdp);
        let root = rsdp.address();
        aml.extend([fadt, xsdt, rsdp]);
        Ok(Self {
            root,
            tables: aml,
            regions: Mutex::new(BTreeMap::new()),
            active: Arc::new(AtomicUsize::new(0)),
        })
    }
    pub fn register(self) -> &'static BackendRegistration {
        Box::leak(Box::new(BackendRegistration(Box::leak(Box::new(self)))))
    }
}
// SAFETY: only owned immutable firmware buffers or private zeroed simulation
// buffers are mapped; no real hardware access. Deferred callbacks capture an
// Arc-owned active counter, so accounting remains live even if this backend is
// dropped before work completes.
unsafe impl Backend for OfflineBackend {
    fn root_pointer(&self) -> u64 {
        self.root
    }
    fn map(&self, address: u64, size: usize) -> *mut c_void {
        if size == 0 || size > 1024 * 1024 || address.checked_add(size as u64).is_none() {
            return std::ptr::null_mut();
        }
        for table in &self.tables {
            let start = table.address();
            let end = address + size as u64;
            let Some(table_end) = start.checked_add(table.len as u64) else {
                continue;
            };
            if address >= start && end <= table_end {
                return address as *mut c_void;
            }
        }
        // Unknown SystemMemory operation regions are *simulated*. This is not
        // evidence of actual EC/NVS/MMIO behaviour or successful native _INI.
        let mut regions = self.regions.lock().unwrap();
        if regions.len() >= 4096 {
            return std::ptr::null_mut();
        }
        let data = regions
            .entry((address, size))
            .or_insert_with(|| std::vec![0u64;size.div_ceil(8)].into_boxed_slice());
        data.as_mut_ptr().cast()
    }
    unsafe fn unmap(&self, _p: *mut c_void, _size: usize) {}
    fn physical_address(&self, p: *mut c_void) -> Result<u64, Status> {
        Ok(p as u64)
    }
    fn readable(&self, _p: *mut c_void, _size: usize) -> bool {
        false
    }
    fn writable(&self, _p: *mut c_void, _size: usize) -> bool {
        false
    }
    fn timer_100ns(&self) -> u64 {
        START.get_or_init(Instant::now).elapsed().as_nanos() as u64 / 100
    }
    fn sleep(&self, millis: u64) -> bool {
        std::thread::sleep(Duration::from_millis(millis.min(100)));
        true
    }
    fn stall(&self, micros: u32) {
        std::thread::sleep(Duration::from_micros(u64::from(micros).min(100_000)));
    }
    fn thread_id(&self) -> u64 {
        std::thread_local! {static ID:usize={static NEXT:AtomicUsize=AtomicUsize::new(1);NEXT.fetch_add(1,Ordering::Relaxed)};}
        ID.with(|id| *id as u64)
    }
    fn irq_save(&self) -> usize {
        0
    }
    fn irq_restore(&self, _flags: usize) {}
    unsafe fn execute(&self, _kind: u32, function: Work, context: *mut c_void) -> Status {
        let active = Arc::clone(&self.active);
        active.fetch_add(1, Ordering::AcqRel);
        let context = context as usize;
        match std::thread::Builder::new().spawn(move || {
            // SAFETY: caller upholds the execute contract: ACPICA owns
            // callback/context until completion; `active` is independently owned.
            unsafe {
                function(context as *mut c_void);
            }
            active.fetch_sub(1, Ordering::Release);
        }) {
            Ok(_) => OK,
            Err(_) => {
                self.active.fetch_sub(1, Ordering::Release);
                LIMIT
            }
        }
    }
    fn quiesce(&self) {
        self.wait_events();
    }
    fn wait_events(&self) {
        while self.active.load(Ordering::Acquire) != 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    unsafe fn remove_irq(&self, _irq: u32, _handler: crate::backend::IrqHandler) -> Status {
        // Offline mode never installs IRQ handlers; ACPICA still requests
        // removal during termination when hardware setup was skipped.
        OK
    }
    fn read_port(&self, _address: u16, width: u32) -> Result<u32, Status> {
        if matches!(width, 8 | 16 | 32) {
            Ok(0)
        } else {
            Err(BAD_PARAMETER)
        }
    }
    fn write_port(&self, _a: u16, _w: u32, _v: u32) -> Status {
        OK
    }
    fn read_pci(&self, _id: PciId, _reg: u32, _width: u32) -> Result<u64, Status> {
        Ok(0)
    }
    fn write_pci(&self, _id: PciId, _reg: u32, _width: u32, _value: u64) -> Status {
        OK
    }
    unsafe fn ec_access(
        &self,
        function: u32,
        address: u64,
        width: u32,
        value: &mut u64,
        _context: *mut c_void,
    ) -> Status {
        if function > 1 || address > u8::MAX.into() || width != 8 {
            return crate::SUPPORT;
        }
        if function == 0 {
            *value = 0x42;
        }
        OK
    }
    fn log(&self, message: &[u8]) {
        std::eprintln!("{}", String::from_utf8_lossy(message));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Receiver, Sender, channel};

    use super::*;

    #[test]
    fn firmware_tables_have_acpi_alignment() {
        let table = include_bytes!("../tests/synthetic.aml")
            .to_vec()
            .into_boxed_slice();
        let backend = OfflineBackend::from_tables(std::vec![table]).unwrap();
        assert!((backend.root as usize).is_multiple_of(align_of::<u64>()));
        assert!(
            backend
                .tables
                .iter()
                .all(|table| (table.address() as usize).is_multiple_of(align_of::<u64>()))
        );
    }

    #[test]
    fn short_sdt_is_rejected_before_signature_read() {
        let short = std::vec![b"DS".to_vec().into_boxed_slice()];
        assert_eq!(
            OfflineBackend::from_tables(short).err().unwrap().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    struct WorkContext {
        started: Sender<()>,
        release: Receiver<()>,
        finished: Sender<()>,
    }

    unsafe extern "C" fn delayed_work(context: *mut c_void) {
        // SAFETY: the test transfers one Box to this callback and keeps it
        // alive until this single invocation takes ownership.
        let context = unsafe { Box::from_raw(context.cast::<WorkContext>()) };
        context.started.send(()).unwrap();
        context.release.recv().unwrap();
        context.finished.send(()).unwrap();
    }

    #[test]
    fn deferred_work_finishes_after_backend_drop() {
        let data = include_bytes!("../tests/synthetic.aml")
            .to_vec()
            .into_boxed_slice();
        let backend = OfflineBackend::from_tables(std::vec![data]).unwrap();
        let active = Arc::clone(&backend.active);
        let (started_tx, started_rx) = channel();
        let (release_tx, release_rx) = channel();
        let (finished_tx, finished_rx) = channel();
        let context = Box::new(WorkContext {
            started: started_tx,
            release: release_rx,
            finished: finished_tx,
        });
        let context = Box::into_raw(context);

        // SAFETY: the boxed context is transferred to the callback, and its
        // channels keep it valid until the callback takes and drops the Box.
        let status = unsafe { backend.execute(0, delayed_work, context.cast()) };
        if status != OK {
            // SAFETY: an error means execute did not retain the callback/context.
            unsafe { drop(Box::from_raw(context)) };
        }
        assert_eq!(status, OK);
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        drop(backend);
        release_tx.send(()).unwrap();
        finished_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        while active.load(Ordering::Acquire) != 0 {
            assert!(Instant::now() < deadline, "deferred work was not reclaimed");
            std::thread::yield_now();
        }
    }
}
