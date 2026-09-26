use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use core::{
    alloc::Layout,
    cell::UnsafeCell,
    hint::spin_loop,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicU64, Ordering, fence},
};

use rdif_iommu::{
    DmaDomainId, IommuController, IommuDomain, IommuError, IovaWindow, MapPermissions, StreamId,
};

use crate::memory::{PhysicalMemory, PhysicalRegion};

const PAGE_SIZE: usize = 4096;
const IOVA_BITS: u32 = 48;
const CMDQ_BITS: u32 = 8;
const EVTQ_BITS: u32 = 7;
const STREAM_SPLIT: u32 = 8;
// PhysicalRegion addresses u64 words; eight words occupy one 64-byte STE.
const STE_WORDS: usize = 8;
const SPIN_LIMIT: usize = 1_000_000;

const IDR0: usize = 0x0;
const IDR1: usize = 0x4;
const IDR5: usize = 0x14;
const CR0: usize = 0x20;
const CR0ACK: usize = 0x24;
const CR1: usize = 0x28;
const CR2: usize = 0x2c;
const STRTAB_BASE: usize = 0x80;
const STRTAB_BASE_CFG: usize = 0x88;
const CMDQ_BASE: usize = 0x90;
const CMDQ_PROD: usize = 0x98;
const CMDQ_CONS: usize = 0x9c;
const EVTQ_BASE: usize = 0xa0;
const EVTQ_PROD: usize = 0xa8;
const EVTQ_CONS: usize = 0xac;

const CR0_SMMUEN: u32 = 1;
const CR0_EVTQEN: u32 = 1 << 2;
const CR0_CMDQEN: u32 = 1 << 3;
const Q_BASE_RWA: u64 = 1 << 62;
const PHYS_MASK: u64 = (1 << 48) - 1;

static NEXT_DOMAIN_ID: AtomicU64 = AtomicU64::new(1);

struct SpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

struct SpinGuard<'a, T>(&'a SpinLock<T>);

impl<T> SpinLock<T> {
    fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    fn lock(&self) -> SpinGuard<'_, T> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
        SpinGuard(self)
    }

    fn try_lock(&self) -> Option<SpinGuard<'_, T>> {
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| SpinGuard(self))
    }
}

// SAFETY: Exclusive access to T is granted by the acquire/release lock bit.
unsafe impl<T: Send> Send for SpinLock<T> {}
// SAFETY: A shared lock reference only exposes T through its guard.
unsafe impl<T: Send> Sync for SpinLock<T> {}

impl<T> core::ops::Deref for SpinGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: The guard owns the lock for its entire lifetime.
        unsafe { &*self.0.value.get() }
    }
}

impl<T> core::ops::DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: The guard owns the only mutable access.
        unsafe { &mut *self.0.value.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.0.locked.store(false, Ordering::Release);
    }
}

#[derive(Clone, Copy)]
struct Mmio(NonNull<u8>);

// SAFETY: The mapping's lifetime and device exclusivity are constructor
// preconditions; all register accesses occur under the controller lock.
unsafe impl Send for Mmio {}

impl Mmio {
    fn read32(self, offset: usize) -> u32 {
        // SAFETY: Smmu::new requires a live 0x20000-byte MMIO mapping.
        unsafe { core::ptr::read_volatile(self.0.as_ptr().add(offset).cast::<u32>()) }
    }

    fn write32(self, offset: usize, value: u32) {
        // SAFETY: Smmu::new requires a live 0x20000-byte MMIO mapping.
        unsafe { core::ptr::write_volatile(self.0.as_ptr().add(offset).cast::<u32>(), value) }
    }

    fn write64(self, offset: usize, value: u64) {
        // SAFETY: Smmu::new requires a live 0x20000-byte MMIO mapping.
        unsafe { core::ptr::write_volatile(self.0.as_ptr().add(offset).cast::<u64>(), value) }
    }
}

enum StreamTable {
    Linear(PhysicalRegion),
    TwoLevel {
        l1: PhysicalRegion,
        l2: BTreeMap<u32, PhysicalRegion>,
    },
}

impl StreamTable {
    fn physical(&self) -> u64 {
        match self {
            Self::Linear(region) => region.physical(),
            Self::TwoLevel { l1, .. } => l1.physical(),
        }
    }

    fn config(&self, sid_bits: u32) -> u32 {
        match self {
            Self::Linear(_) => sid_bits,
            Self::TwoLevel { .. } => (1 << 16) | (STREAM_SPLIT << 6) | sid_bits,
        }
    }

    fn ensure_ste(
        &mut self,
        sid: u32,
        memory: &'static dyn PhysicalMemory,
        physical_limit: u64,
    ) -> Result<(&PhysicalRegion, usize), IommuError> {
        match self {
            Self::Linear(region) => Ok((region, sid as usize * STE_WORDS)),
            Self::TwoLevel { l1, l2 } => {
                let l1_index = sid >> STREAM_SPLIT;
                if let alloc::collections::btree_map::Entry::Vacant(entry) = l2.entry(l1_index) {
                    let region = allocate(
                        memory,
                        1 << (STREAM_SPLIT + 6),
                        1 << (STREAM_SPLIT + 6),
                        physical_limit,
                    )?;
                    for index in 0..1 << STREAM_SPLIT {
                        region.write_u64(index * STE_WORDS, 1); // Valid abort STE.
                    }
                    let physical = region.physical();
                    entry.insert(region);
                    fence(Ordering::SeqCst);
                    l1.write_u64(l1_index as usize, physical | (STREAM_SPLIT as u64 + 1));
                    fence(Ordering::SeqCst);
                }
                let region = l2.get(&l1_index).ok_or(IommuError::OutOfMemory)?;
                Ok((
                    region,
                    (sid & ((1 << STREAM_SPLIT) - 1)) as usize * STE_WORDS,
                ))
            }
        }
    }
}

struct DomainState {
    id: DmaDomainId,
    asid: u16,
    cd: PhysicalRegion,
    root: u64,
    physical_limit: u64,
    tables: BTreeMap<u64, PhysicalRegion>,
    active: bool,
}

impl DomainState {
    fn table(&self, physical: u64) -> Result<&PhysicalRegion, IommuError> {
        self.tables.get(&physical).ok_or(IommuError::InvalidAddress)
    }

    fn leaf_location(
        &mut self,
        iova: u64,
        memory: &'static dyn PhysicalMemory,
        create: bool,
    ) -> Result<Option<(u64, usize)>, IommuError> {
        let mut table = self.root;
        for shift in [39, 30, 21] {
            let index = ((iova >> shift) & 511) as usize;
            let entry = self.table(table)?.read_u64(index);
            if entry == 0 {
                if !create {
                    return Ok(None);
                }
                let child = allocate(memory, PAGE_SIZE, PAGE_SIZE, self.physical_limit)?;
                let physical = child.physical();
                self.tables.insert(physical, child);
                fence(Ordering::SeqCst);
                self.table(table)?.write_u64(index, physical | 3);
                table = physical;
            } else if entry & 3 == 3 {
                table = entry & (PHYS_MASK & !0xfff);
            } else {
                return Err(IommuError::InvalidAddress);
            }
        }
        Ok(Some((table, ((iova >> 12) & 511) as usize)))
    }
}

struct Hardware {
    mmio: Mmio,
    memory: &'static dyn PhysicalMemory,
    sid_bits: u32,
    oas_bits: u32,
    asid_limit: u16,
    next_asid: u16,
    cmdq_bits: u32,
    evtq_bits: u32,
    cmdq: PhysicalRegion,
    evtq: PhysicalRegion,
    cmd_prod: u32,
    evt_cons: u32,
    streams: StreamTable,
    domains: BTreeMap<u32, DomainState>,
    fault_count: u64,
}

/// A fault reported by EVTQ. `address` is the device-supplied IOVA.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SmmuFault {
    pub event_id: u8,
    pub stream: StreamId,
    pub address: u64,
    pub read: bool,
}

/// Arm SMMUv3 controller for a coherent, 4 KiB-granule Stage 1 system.
pub struct Smmu {
    inner: Arc<SpinLock<Hardware>>,
}

impl Smmu {
    /// # Safety
    /// `mmio` must be a live Device-memory mapping of at least 0x20000 bytes,
    /// exclusive to this driver, and remain mapped for the boot lifetime.
    /// This driver does not support hot-unplug or hard-IRQ reentrancy.
    pub unsafe fn new(
        mmio: NonNull<u8>,
        memory: &'static dyn PhysicalMemory,
    ) -> Result<Self, IommuError> {
        let mmio = Mmio(mmio);
        let idr0 = mmio.read32(IDR0);
        let idr1 = mmio.read32(IDR1);
        let idr5 = mmio.read32(IDR5);
        let sid_bits = idr1 & 63;
        let oas_bits = match idr5 & 7 {
            0 => 32,
            1 => 36,
            2 => 40,
            3 => 42,
            4 => 44,
            5 => 48,
            _ => return Err(IommuError::UnsupportedHardware),
        };
        // Require S1, AArch64 tables, coherent walks, little-endian tables,
        // 4 KiB granules and the QEMU virt maximum SID width.
        if idr0 & (1 << 1) == 0
            || (idr0 >> 2) & 3 != 2
            || idr0 & (1 << 4) == 0
            || (idr0 >> 21) & 3 != 2
            || idr5 & (1 << 4) == 0
            || !(1..=16).contains(&sid_bits)
            || idr1 & ((1 << 30) | (1 << 29)) != 0
        {
            return Err(IommuError::UnsupportedHardware);
        }
        // A live firmware-owned SMMU requires a handover protocol and cannot
        // be reset without first quiescing its devices.
        if mmio.read32(CR0) != 0 {
            return Err(IommuError::UnsupportedHardware);
        }
        let cmdq_bits = CMDQ_BITS.min((idr1 >> 21) & 31);
        let evtq_bits = EVTQ_BITS.min((idr1 >> 16) & 31);
        if cmdq_bits < 2 || evtq_bits < 2 {
            return Err(IommuError::UnsupportedHardware);
        }
        let physical_limit = 1 << oas_bits;
        let cmdq = allocate(memory, 16 << cmdq_bits, PAGE_SIZE, physical_limit)?;
        let evtq = allocate(memory, 32 << evtq_bits, PAGE_SIZE, physical_limit)?;
        let streams = if (idr0 >> 27) & 3 == 1 && sid_bits > STREAM_SPLIT {
            let entries = 1usize << sid_bits.saturating_sub(STREAM_SPLIT);
            let l1 = allocate(
                memory,
                (entries * 8).max(PAGE_SIZE),
                PAGE_SIZE,
                physical_limit,
            )?;
            StreamTable::TwoLevel {
                l1,
                l2: BTreeMap::new(),
            }
        } else {
            let entries = 1usize << sid_bits;
            let region = allocate(memory, entries * 64, PAGE_SIZE, physical_limit)?;
            for index in 0..entries {
                region.write_u64(index * STE_WORDS, 1); // Valid abort STE.
            }
            StreamTable::Linear(region)
        };
        let mut hardware = Hardware {
            mmio,
            memory,
            sid_bits,
            oas_bits,
            asid_limit: if idr0 & (1 << 12) != 0 { u16::MAX } else { 255 },
            next_asid: 1,
            cmdq_bits,
            evtq_bits,
            cmdq,
            evtq,
            cmd_prod: 0,
            evt_cons: 0,
            streams,
            domains: BTreeMap::new(),
            fault_count: 0,
        };
        if let Err(error) = hardware.initialize() {
            mmio.write32(CR0, 0);
            if wait_register(mmio, CR0ACK, 0).is_err() {
                // The SMMU may still access control memory; quarantine it.
                core::mem::forget(hardware);
            }
            return Err(error);
        }
        let inner = Arc::new(SpinLock::new(hardware));
        // The SMMU remains enabled after registration. Without a device-wide
        // stop protocol it is unsafe to free CMDQ, EVTQ, CD or page-table memory
        // when the last software handle disappears. Keep the controller's
        // control memory pinned for the boot lifetime.
        core::mem::forget(inner.clone());
        Ok(Self { inner })
    }

    /// Drain event records from a task context. It never takes the lock from
    /// a hard IRQ and therefore cannot deadlock with a mapping transaction.
    pub fn drain_faults(&self) -> Result<Vec<SmmuFault>, IommuError> {
        let mut guard = self.inner.try_lock().ok_or(IommuError::Busy)?;
        guard.drain_faults()
    }

    /// Count all faults observed so far, including newly queued EVTQ records.
    pub fn fault_count(&self) -> Result<u64, IommuError> {
        let mut guard = self.inner.try_lock().ok_or(IommuError::Busy)?;
        guard.drain_faults()?;
        Ok(guard.fault_count)
    }
}

impl IommuController for Smmu {
    fn bind(&self, stream: StreamId) -> Result<Arc<dyn IommuDomain>, IommuError> {
        let mut hardware = self.inner.lock();
        hardware.bind(stream)?;
        Ok(Arc::new(AttachedDomain {
            stream,
            inner: self.inner.clone(),
        }))
    }
}

struct AttachedDomain {
    stream: StreamId,
    inner: Arc<SpinLock<Hardware>>,
}

impl IommuDomain for AttachedDomain {
    fn id(&self) -> DmaDomainId {
        let hardware = self.inner.lock();
        hardware.domains[&self.stream.0].id
    }

    fn window(&self) -> IovaWindow {
        IovaWindow {
            start: PAGE_SIZE as u64,
            end: 1 << IOVA_BITS,
        }
    }

    fn map_pages(
        &self,
        iova: u64,
        physical: u64,
        len: usize,
        permissions: MapPermissions,
    ) -> Result<(), IommuError> {
        self.inner
            .lock()
            .map_pages(self.stream, iova, physical, len, permissions)
    }

    fn unmap_and_sync(&self, iova: u64, len: usize) -> Result<(), IommuError> {
        self.inner.lock().unmap_and_sync(self.stream, iova, len)
    }
}

fn allocate(
    memory: &'static dyn PhysicalMemory,
    size: usize,
    align: usize,
    physical_limit: u64,
) -> Result<PhysicalRegion, IommuError> {
    let layout = Layout::from_size_align(size, align).map_err(|_| IommuError::InvalidLength)?;
    let region = memory.allocate(layout)?;
    if region.len() < size
        || region.physical() & (align as u64 - 1) != 0
        || region
            .physical()
            .checked_add(size as u64)
            .is_none_or(|end| end > physical_limit)
    {
        return Err(IommuError::InvalidAddress);
    }
    Ok(region)
}

fn wait_register(mmio: Mmio, offset: usize, expected: u32) -> Result<(), IommuError> {
    for _ in 0..SPIN_LIMIT {
        if mmio.read32(offset) == expected {
            return Ok(());
        }
        spin_loop();
    }
    Err(IommuError::CommandTimeout)
}

impl Hardware {
    fn initialize(&mut self) -> Result<(), IommuError> {
        let mmio = self.mmio;
        mmio.write32(CR0, 0);
        wait_register(mmio, CR0ACK, 0)?;
        // Shareable, write-back table and queue accesses; PTM/RECINVSID as Linux.
        mmio.write32(
            CR1,
            (3 << 10) | (1 << 8) | (1 << 6) | (3 << 4) | (1 << 2) | 1,
        );
        mmio.write32(CR2, (1 << 2) | (1 << 1));
        mmio.write64(
            STRTAB_BASE,
            (self.streams.physical() & PHYS_MASK & !63) | Q_BASE_RWA,
        );
        mmio.write32(STRTAB_BASE_CFG, self.streams.config(self.sid_bits));
        mmio.write64(
            CMDQ_BASE,
            (self.cmdq.physical() & PHYS_MASK & !31) | Q_BASE_RWA | u64::from(self.cmdq_bits),
        );
        mmio.write32(CMDQ_PROD, 0);
        mmio.write32(CMDQ_CONS, 0);
        mmio.write32(CR0, CR0_CMDQEN);
        wait_register(mmio, CR0ACK, CR0_CMDQEN)?;
        self.command_and_sync([4, 31])?; // CFGI_ALL
        self.command_and_sync([0x30, 0])?; // TLBI_NSNH_ALL
        mmio.write64(
            EVTQ_BASE,
            (self.evtq.physical() & PHYS_MASK & !31) | Q_BASE_RWA | u64::from(self.evtq_bits),
        );
        // EVTQ is in the second SMMU register page.
        mmio.write32(0x10000 + EVTQ_PROD, 0);
        mmio.write32(0x10000 + EVTQ_CONS, 0);
        mmio.write32(CR0, CR0_CMDQEN | CR0_EVTQEN);
        wait_register(mmio, CR0ACK, CR0_CMDQEN | CR0_EVTQEN)?;
        mmio.write32(CR0, CR0_CMDQEN | CR0_EVTQEN | CR0_SMMUEN);
        wait_register(mmio, CR0ACK, CR0_CMDQEN | CR0_EVTQEN | CR0_SMMUEN)
    }

    fn command(&mut self, words: [u64; 2]) -> Result<(), IommuError> {
        let mask = (1 << (self.cmdq_bits + 1)) - 1;
        for _ in 0..SPIN_LIMIT {
            let cons = self.mmio.read32(CMDQ_CONS);
            let error = (cons >> 24) & 0x7f;
            if error != 0 {
                return Err(IommuError::CommandError(error));
            }
            if self.cmd_prod.wrapping_sub(cons & mask) & mask < 1 << self.cmdq_bits {
                let index = (self.cmd_prod & ((1 << self.cmdq_bits) - 1)) as usize * 2;
                self.cmdq.write_u64(index, words[0]);
                self.cmdq.write_u64(index + 1, words[1]);
                fence(Ordering::SeqCst);
                self.cmd_prod = (self.cmd_prod + 1) & mask;
                self.mmio.write32(CMDQ_PROD, self.cmd_prod);
                return Ok(());
            }
            spin_loop();
        }
        Err(IommuError::CommandTimeout)
    }

    fn command_and_sync(&mut self, words: [u64; 2]) -> Result<(), IommuError> {
        self.command(words)?;
        self.command([0x46 | (2 << 12), 0])?;
        let mask = (1 << (self.cmdq_bits + 1)) - 1;
        for _ in 0..SPIN_LIMIT {
            let cons = self.mmio.read32(CMDQ_CONS);
            let error = (cons >> 24) & 0x7f;
            if error != 0 {
                return Err(IommuError::CommandError(error));
            }
            if cons & mask == self.cmd_prod {
                return Ok(());
            }
            spin_loop();
        }
        Err(IommuError::CommandTimeout)
    }

    fn bind(&mut self, stream: StreamId) -> Result<(), IommuError> {
        if u64::from(stream.0) >= 1 << self.sid_bits {
            return Err(IommuError::InvalidAddress);
        }
        if self.domains.contains_key(&stream.0) {
            return Err(IommuError::AlreadyBound);
        }
        let asid = self.next_asid;
        if asid == 0 || asid > self.asid_limit {
            return Err(IommuError::NoIdentifiers);
        }
        self.next_asid = self.next_asid.wrapping_add(1);
        let physical_limit = 1 << self.oas_bits;
        let root = allocate(self.memory, PAGE_SIZE, PAGE_SIZE, physical_limit)?;
        let root_physical = root.physical();
        let cd = allocate(self.memory, 64, 64, physical_limit)?;
        let ips = match self.oas_bits {
            32 => 0,
            36 => 1,
            40 => 2,
            42 => 3,
            44 => 4,
            _ => 5,
        };
        cd.write_u64(
            0,
            16 | (1 << 8)
                | (1 << 10)
                | (3 << 12)
                | (1 << 30)
                | (1 << 31)
                | ((ips as u64) << 32)
                | (1 << 41)
                | (1 << 45)
                | (1 << 46)
                | (1 << 47)
                | (u64::from(asid) << 48),
        );
        cd.write_u64(1, root_physical);
        cd.write_u64(3, 0xf404_ff44);
        let id = DmaDomainId(NEXT_DOMAIN_ID.fetch_add(1, Ordering::Relaxed));
        let mut tables = BTreeMap::new();
        tables.insert(root_physical, root);
        self.domains.insert(
            stream.0,
            DomainState {
                id,
                asid,
                cd,
                root: root_physical,
                physical_limit,
                tables,
                active: false,
            },
        );
        let result = (|| {
            let (ste_region, ste_word_index) =
                self.streams
                    .ensure_ste(stream.0, self.memory, physical_limit)?;
            let cd_physical = self.domains[&stream.0].cd.physical();
            // Publish CD before STE, and publish the STE's valid/config word last.
            fence(Ordering::SeqCst);
            ste_region.write_u64(ste_word_index + 1, 2 | (1 << 2) | (1 << 4) | (3 << 6));
            ste_region.write_u64(ste_word_index, 1 | (5 << 1) | cd_physical);
            fence(Ordering::SeqCst);
            self.command_and_sync([3 | (u64::from(stream.0) << 32), 1])?;
            self.command_and_sync([5 | (u64::from(stream.0) << 32), 1])?;
            Ok(())
        })();
        if result.is_ok() {
            self.domains
                .get_mut(&stream.0)
                .ok_or(IommuError::InvalidAddress)?
                .active = true;
        }
        result
    }

    fn validate_range(
        &self,
        iova: u64,
        physical: Option<u64>,
        len: usize,
    ) -> Result<(), IommuError> {
        if len == 0 || len & (PAGE_SIZE - 1) != 0 {
            return Err(IommuError::InvalidLength);
        }
        if iova & (PAGE_SIZE as u64 - 1) != 0
            || !(IovaWindow {
                start: PAGE_SIZE as u64,
                end: 1 << IOVA_BITS,
            })
            .contains(iova, len)
        {
            return Err(IommuError::InvalidAddress);
        }
        if let Some(physical) = physical
            && (physical & (PAGE_SIZE as u64 - 1) != 0
                || physical
                    .checked_add(len as u64)
                    .is_none_or(|end| end > 1 << self.oas_bits))
        {
            return Err(IommuError::InvalidAddress);
        }
        Ok(())
    }

    fn map_pages(
        &mut self,
        stream: StreamId,
        iova: u64,
        physical: u64,
        len: usize,
        permissions: MapPermissions,
    ) -> Result<(), IommuError> {
        self.validate_range(iova, Some(physical), len)?;
        if !permissions.can_read() && !permissions.can_write() {
            return Err(IommuError::InvalidPermissions);
        }
        let domain = self
            .domains
            .get_mut(&stream.0)
            .ok_or(IommuError::InvalidAddress)?;
        if !domain.active {
            return Err(IommuError::Busy);
        }
        let pages = len / PAGE_SIZE;
        // Reject overlap before changing any page-table entry.
        for index in 0..pages {
            let addr = iova + (index * PAGE_SIZE) as u64;
            if let Some((table, slot)) = domain.leaf_location(addr, self.memory, false)?
                && domain.table(table)?.read_u64(slot) != 0
            {
                return Err(IommuError::AlreadyMapped);
            }
        }
        let mut mapped = 0;
        let mut failure = None;
        for index in 0..pages {
            let addr = iova + (index * PAGE_SIZE) as u64;
            match domain.leaf_location(addr, self.memory, true) {
                Ok(Some((table, slot))) => {
                    let phys = physical + (index * PAGE_SIZE) as u64;
                    // Ordinary DMA is unprivileged (AP[1:0] bit 0), matching
                    // Linux's S1 IOMMU mapping policy. QEMU currently treats
                    // all requests as privileged but real hardware need not.
                    let mut pte = phys | 3 | (1 << 6) | (1 << 10) | (1 << 11) | (3 << 53);
                    if !permissions.can_write() {
                        pte |= 1 << 7;
                    }
                    // Stage 1 AP cannot express write-only access. A WRITE
                    // request also permits reads, as in Linux's S1 page table.
                    if permissions.is_mmio() {
                        pte |= 2 << 2; // MAIR[2]: Device-nGnRE.
                        pte |= 2 << 8; // Outer shareable.
                    } else {
                        pte |= 1 << 2; // MAIR[1]: Normal write-back.
                        pte |= 3 << 8; // Inner shareable.
                    }
                    domain.table(table)?.write_u64(slot, pte);
                    mapped += 1;
                }
                Ok(None) => unreachable!(),
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        fence(Ordering::SeqCst);
        if let Some(error) = failure {
            let asid = domain.asid;
            for index in 0..mapped {
                let addr = iova + (index * PAGE_SIZE) as u64;
                if let Some((table, slot)) = domain.leaf_location(addr, self.memory, false)? {
                    domain.table(table)?.write_u64(slot, 0);
                }
            }
            fence(Ordering::SeqCst);
            // Even rollback requires sync: a transient translation may be cached.
            if mapped != 0 {
                self.command_and_sync([0x11 | (u64::from(asid) << 48), 0])?;
            }
            return Err(error);
        }
        Ok(())
    }

    fn unmap_and_sync(
        &mut self,
        stream: StreamId,
        iova: u64,
        len: usize,
    ) -> Result<(), IommuError> {
        self.validate_range(iova, None, len)?;
        let domain = self
            .domains
            .get_mut(&stream.0)
            .ok_or(IommuError::InvalidAddress)?;
        if !domain.active {
            return Err(IommuError::Busy);
        }
        let pages = len / PAGE_SIZE;
        for index in 0..pages {
            let addr = iova + (index * PAGE_SIZE) as u64;
            let (table, slot) = domain
                .leaf_location(addr, self.memory, false)?
                .ok_or(IommuError::NotMapped)?;
            if domain.table(table)?.read_u64(slot) == 0 {
                return Err(IommuError::NotMapped);
            }
        }
        for index in 0..pages {
            let addr = iova + (index * PAGE_SIZE) as u64;
            let (table, slot) = domain
                .leaf_location(addr, self.memory, false)?
                .ok_or(IommuError::NotMapped)?;
            domain.table(table)?.write_u64(slot, 0);
        }
        let asid = domain.asid;
        fence(Ordering::SeqCst);
        self.command_and_sync([0x11 | (u64::from(asid) << 48), 0])
    }

    fn drain_faults(&mut self) -> Result<Vec<SmmuFault>, IommuError> {
        let mut faults = Vec::new();
        let mask = (1 << (self.evtq_bits + 1)) - 1;
        let prod = self.mmio.read32(0x10000 + EVTQ_PROD);
        if prod & (1 << 31) != self.evt_cons & (1 << 31) {
            // Match the producer's overflow epoch before returning the
            // diagnostic error. Retain the consumer index so a later call can
            // still drain entries that have not been overwritten.
            self.evt_cons = (self.evt_cons & mask) | (prod & (1 << 31));
            self.mmio.write32(0x10000 + EVTQ_CONS, self.evt_cons);
            return Err(IommuError::QueueOverflow);
        }
        while self.evt_cons & mask != prod & mask {
            let index = (self.evt_cons & ((1 << self.evtq_bits) - 1)) as usize * 4;
            let word0 = self.evtq.read_u64(index);
            let word1 = self.evtq.read_u64(index + 1);
            let word2 = self.evtq.read_u64(index + 2);
            faults.push(SmmuFault {
                event_id: word0 as u8,
                stream: StreamId((word0 >> 32) as u32),
                address: word2,
                read: word1 & (1 << 35) != 0,
            });
            self.fault_count = self.fault_count.saturating_add(1);
            self.evt_cons = (self.evt_cons + 1) & mask;
        }
        fence(Ordering::SeqCst);
        self.mmio.write32(0x10000 + EVTQ_CONS, self.evt_cons);
        Ok(faults)
    }
}
