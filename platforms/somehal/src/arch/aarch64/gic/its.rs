use alloc::{
    alloc::{alloc_zeroed, dealloc},
    collections::BTreeMap,
    format,
    vec::Vec,
};
use core::{
    alloc::Layout,
    ptr::NonNull,
    sync::atomic::{AtomicU64, Ordering},
};

use arm_gic_driver::v3::{Affinity, GITS_TRANSLATER_OFFSET, Its, ItsCommand, ItsTableType};
use ax_kspin::SpinRaw as Mutex;
use irq_framework::{HwIrq, IrqError, IrqId};
use rdif_msi::{
    Interface, Msi, MsiAllocation, MsiDeviceId, MsiEventId, MsiMessage, MsiProviderId, MsiRequest,
    MsiReservationRequest, MsiVector, MsiVectorIndex,
};
use rdrive::{DeviceId, module_driver, probe::OnProbeError, register::ProbeFdt};
use someboot::DCacheOp;

use crate::common::ioremap;

pub(super) const LPI_INTID_BASE: u32 = 8192;
const LPI_ID_BITS: u8 = 16;
const LPI_COUNT: usize = 1 << LPI_ID_BITS;
const LPI_INTID_LIMIT: u32 = (1 << LPI_ID_BITS) - 1;
const LPI_PROPERTY_BYTES: usize = LPI_COUNT;
const LPI_PENDING_BYTES_PER_RD: usize = LPI_COUNT / 8;
const LPI_DEFAULT_PRIORITY: u8 = 0xa0;
const COMMAND_QUEUE_ENTRIES: usize = 256;
const MIN_DEVICE_EVENTS: u32 = 32;
const MAX_DEVICE_ID_BITS: u8 = 16;
const DEFAULT_COLLECTION_ID: u16 = 0;
const INVALID_DEVICE_ID: u64 = u64::MAX;

static LPI_OWNER: Mutex<BTreeMap<u32, DeviceId>> = Mutex::new(BTreeMap::new());
static PRIMARY_ITS: AtomicU64 = AtomicU64::new(INVALID_DEVICE_ID);

module_driver!(
    name: "GICv3 ITS",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::MSI,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["arm,gic-v3-its"],
            on_probe: probe_its
        }
    ],
);

fn probe_its(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, dev) = probe.into_parts();
    let reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.name())))?;
    let size = reg.size.unwrap_or((GITS_TRANSLATER_OFFSET + 8) as u64) as usize;
    let mmio = ioremap(reg.address, size)
        .map_err(|err| OnProbeError::other(format!("failed to map ITS: {err:?}")))?;
    let its = unsafe { Its::new(mmio.as_ptr().into(), reg.address) };

    if !its.supports_physical_lpis() {
        return Err(OnProbeError::Unsupported(
            "GIC ITS does not support physical LPIs",
        ));
    }

    let gicr_phys_base = super::v3::primary_gicr_phys_base()
        .ok_or_else(|| OnProbeError::other("GICv3 redistributor base is not available for ITS"))?;
    let provider_id = dev.descriptor.device_id();
    let provider = GicItsProvider::new(provider_id, its, mmio, gicr_phys_base)?;
    PRIMARY_ITS.store(u64::from(provider_id), Ordering::Release);
    dev.register(Msi::new(MsiProviderId(u64::from(provider_id)), provider));
    Ok(())
}

fn with_gic<R>(
    f: impl FnOnce(&mut arm_gic_driver::v3::Gic) -> Result<R, OnProbeError>,
) -> Result<R, OnProbeError> {
    let gic = rdrive::get_one::<rdif_intc::Intc>()
        .ok_or_else(|| OnProbeError::other("GICv3 interrupt controller is not registered"))?;
    let mut gic = gic
        .lock()
        .map_err(|_| OnProbeError::other("failed to lock GICv3 interrupt controller"))?;
    let gic = gic
        .typed_mut::<arm_gic_driver::v3::Gic>()
        .ok_or_else(|| OnProbeError::other("primary interrupt controller is not GICv3"))?;
    f(gic)
}

pub(super) fn set_lpi_enabled(irq: IrqId, enabled: bool) -> Result<(), IrqError> {
    let owner = LPI_OWNER
        .lock()
        .get(&irq.hwirq.0)
        .copied()
        .or_else(|| match PRIMARY_ITS.load(Ordering::Acquire) {
            INVALID_DEVICE_ID => None,
            raw => Some(DeviceId::from(raw)),
        })
        .ok_or(IrqError::Unsupported)?;
    let msi = rdrive::get::<Msi>(owner).map_err(|_| IrqError::Unsupported)?;
    let mut msi = msi.try_lock().map_err(|_| IrqError::Busy)?;
    let provider = msi
        .typed_mut::<GicItsProvider>()
        .ok_or(IrqError::Unsupported)?;
    provider.set_lpi_enabled_by_intid(irq.hwirq.0, enabled)
}

struct GicItsProvider {
    owner: DeviceId,
    its: Its,
    _mmio: mmio_api::MmioRaw,
    command_queue: CommandQueue,
    property_table: AlignedMemory,
    _pending_tables: AlignedMemory,
    next_lpi: u32,
    devices: BTreeMap<u32, ItsDevice>,
    lpis: BTreeMap<u32, LpiRoute>,
    translations: BTreeMap<(u32, u32), u32>,
    default_collection: ItsCollection,
    collections: BTreeMap<usize, ItsCollection>,
    next_collection: usize,
    collection_capacity: usize,
    gicr_phys_base: u64,
    uses_physical_collection_target: bool,
    gic_domain: irq_framework::IrqDomainId,
    _msi_domain: irq_framework::IrqDomainId,
    msix_domain: irq_framework::IrqDomainId,
    itt_entry_size: usize,
}

impl GicItsProvider {
    fn new(
        owner: DeviceId,
        its: Its,
        mmio: mmio_api::MmioRaw,
        gicr_phys_base: u64,
    ) -> Result<Self, OnProbeError> {
        let gic_domain = crate::irq::domain_by_kind_fast(crate::irq::IrqDomainKind::AArch64Gic)
            .ok_or_else(|| OnProbeError::other("AArch64 GIC IRQ domain is not registered"))?;
        let msi_domain = crate::irq::alloc_child_irq_domain(
            owner,
            gic_domain,
            crate::irq::IrqDomainKind::MsiParent,
        )
        .map_err(|err| {
            OnProbeError::other(format!("failed to allocate ITS MSI IRQ domain: {err:?}"))
        })?;
        let msix_domain = crate::irq::alloc_child_irq_domain(
            owner,
            msi_domain,
            crate::irq::IrqDomainKind::PciMsix,
        )
        .map_err(|err| {
            OnProbeError::other(format!("failed to allocate PCI MSI-X IRQ domain: {err:?}"))
        })?;
        let property_table = AlignedMemory::new(LPI_PROPERTY_BYTES, 4096)
            .ok_or_else(|| OnProbeError::other("failed to allocate LPI property table"))?;
        property_table.fill(LPI_DEFAULT_PRIORITY);
        property_table.clean();

        let rd_count = with_gic(|gic| Ok(gic.redistributor_count().max(1)))?;
        let pending_stride = align_up(LPI_PENDING_BYTES_PER_RD, 4096);
        let pending_tables = AlignedMemory::new(pending_stride * rd_count, 65536)
            .ok_or_else(|| OnProbeError::other("failed to allocate LPI pending tables"))?;
        pending_tables.clean();

        let uses_physical_collection_target = its.uses_physical_collection_target();
        let (rd_count, collection_target) = with_gic(|gic| {
            if !gic.supports_lpis() {
                return Err(OnProbeError::Unsupported(
                    "GICv3 distributor does not support LPIs",
                ));
            }
            gic.init_lpi_tables(
                property_table.phys(),
                LPI_ID_BITS,
                pending_tables.phys(),
                pending_stride,
            )
            .map_err(|err| {
                OnProbeError::other(format!("failed to initialize GICR LPI tables: {err}"))
            })?;
            let target = gic
                .collection_target_for_affinity(
                    gicr_phys_base,
                    Affinity::current(),
                    uses_physical_collection_target,
                )
                .ok_or_else(|| {
                    OnProbeError::other("current CPU has no Redistributor collection target")
                })?;
            Ok((gic.redistributor_count().max(1), target))
        })?;

        its.disable();
        let command_queue = CommandQueue::new(COMMAND_QUEUE_ENTRIES)
            .ok_or_else(|| OnProbeError::other("failed to allocate ITS command queue"))?;
        its.init_command_queue(command_queue.phys(), command_queue.bytes());

        program_baser_table(&its, ItsTableType::Device, MAX_DEVICE_ID_BITS)?;
        let collection_capacity = rd_count
            .checked_add(1)
            .and_then(usize::checked_next_power_of_two)
            .ok_or_else(|| OnProbeError::other("ITS collection count overflows"))?;
        let collection_id_bits = collection_capacity.ilog2() as u8;
        program_baser_table(&its, ItsTableType::Collection, collection_id_bits)?;
        its.enable();

        let mut provider = Self {
            owner,
            its,
            _mmio: mmio,
            command_queue,
            property_table,
            _pending_tables: pending_tables,
            next_lpi: LPI_INTID_BASE,
            devices: BTreeMap::new(),
            lpis: BTreeMap::new(),
            translations: BTreeMap::new(),
            default_collection: ItsCollection {
                id: DEFAULT_COLLECTION_ID,
                target: collection_target,
            },
            collections: BTreeMap::new(),
            next_collection: 1,
            collection_capacity,
            gicr_phys_base,
            uses_physical_collection_target,
            gic_domain,
            _msi_domain: msi_domain,
            msix_domain,
            itt_entry_size: 16,
        };
        provider.itt_entry_size = provider.its.itt_entry_size().max(8);
        provider
            .send_command(ItsCommand::mapc(
                DEFAULT_COLLECTION_ID,
                collection_target,
                true,
            ))
            .map_err(|err| OnProbeError::other(format!("failed to send ITS MAPC: {err:?}")))?;
        provider
            .send_command(ItsCommand::sync(collection_target))
            .map_err(|err| OnProbeError::other(format!("failed to send ITS SYNC: {err:?}")))?;
        Ok(provider)
    }

    fn ensure_device(&mut self, device: MsiDeviceId, required_events: u32) -> Result<(), IrqError> {
        if let Some(state) = self.devices.get(&device.0) {
            return if required_events <= state.event_capacity {
                Ok(())
            } else {
                Err(IrqError::NoMemory)
            };
        }
        let event_capacity = required_events
            .max(MIN_DEVICE_EVENTS)
            .checked_next_power_of_two()
            .ok_or(IrqError::NoMemory)?;
        let itt_bytes = self.itt_entry_size * event_capacity as usize;
        let itt = AlignedMemory::new(itt_bytes, 256).ok_or(IrqError::NoMemory)?;
        itt.clean();
        self.send_command(ItsCommand::mapd(device.0, itt.phys(), event_capacity, true))?;
        self.send_command(ItsCommand::sync(self.default_collection.target))?;
        self.devices.insert(
            device.0,
            ItsDevice {
                _itt: itt,
                event_capacity,
                next_event: 0,
            },
        );
        Ok(())
    }

    fn ensure_collection(
        &mut self,
        affinity: irq_framework::IrqAffinity,
    ) -> Result<ItsCollection, IrqError> {
        let irq_framework::IrqAffinity::Fixed(cpu) = affinity else {
            return Ok(self.default_collection);
        };
        if let Some(collection) = self.collections.get(&cpu.0).copied() {
            return Ok(collection);
        }
        if self.next_collection >= self.collection_capacity {
            return Err(IrqError::NoMemory);
        }
        let id = u16::try_from(self.next_collection).map_err(|_| IrqError::NoMemory)?;
        let target = self.collection_target_for_cpu(cpu.0)?;
        let collection = ItsCollection { id, target };
        self.send_command(ItsCommand::mapc(id, target, true))?;
        if let Err(error) = self.send_command(ItsCommand::sync(target)) {
            let _ = self.send_command(ItsCommand::mapc(id, target, false));
            return Err(error);
        }
        self.next_collection += 1;
        self.collections.insert(cpu.0, collection);
        Ok(collection)
    }

    fn collection_target_for_cpu(&self, cpu: usize) -> Result<u64, IrqError> {
        let gic = rdrive::get_one::<rdif_intc::Intc>().ok_or(IrqError::Unsupported)?;
        let mut gic = gic.lock().map_err(|_| IrqError::Busy)?;
        let gic = gic
            .typed_mut::<arm_gic_driver::v3::Gic>()
            .ok_or(IrqError::Unsupported)?;
        let affinity = Affinity::from_mpidr(super::hardware_cpu_id(cpu) as u64);
        gic.collection_target_for_affinity(
            self.gicr_phys_base,
            affinity,
            self.uses_physical_collection_target,
        )
        .ok_or(IrqError::InvalidCpu)
    }

    fn next_lpi_candidate(&mut self) -> Result<u32, IrqError> {
        let lpi = self.next_lpi;
        if lpi > LPI_INTID_LIMIT {
            return Err(IrqError::NoMemory);
        }
        self.next_lpi += 1;
        Ok(lpi)
    }

    fn reserve_lpi(&self, lpi: u32) -> Result<(), IrqError> {
        if !(LPI_INTID_BASE..=LPI_INTID_LIMIT).contains(&lpi) {
            return Err(IrqError::InvalidIrq);
        }
        let mut owners = LPI_OWNER.lock();
        if owners.contains_key(&lpi) {
            return Err(IrqError::Busy);
        }
        owners.insert(lpi, self.owner);
        Ok(())
    }

    fn release_lpi(&self, lpi: u32) {
        let mut owners = LPI_OWNER.lock();
        if owners.get(&lpi) == Some(&self.owner) {
            owners.remove(&lpi);
        }
    }

    fn install_translation(&mut self, request: TranslationRequest) -> Result<MsiVector, IrqError> {
        if request.parent_irq != IrqId::new(self.gic_domain, HwIrq(request.lpi))
            || self
                .translations
                .contains_key(&(request.device.0, request.event.0))
            || self.lpis.contains_key(&request.lpi)
        {
            return Err(IrqError::InvalidIrq);
        }
        let required_events = request.event.0.checked_add(1).ok_or(IrqError::NoMemory)?;
        self.ensure_device(request.device, required_events)?;
        let collection = self.ensure_collection(request.affinity)?;
        self.reserve_lpi(request.lpi)?;

        let result = self.install_reserved_translation(request, collection);
        if result.is_err() {
            let _ = self.send_command(ItsCommand::discard(request.device.0, request.event.0));
            let _ = self.send_command(ItsCommand::sync(collection.target));
            self.release_lpi(request.lpi);
        }
        result
    }

    fn install_reserved_translation(
        &mut self,
        request: TranslationRequest,
        collection: ItsCollection,
    ) -> Result<MsiVector, IrqError> {
        self.set_property_enabled(request.lpi, false)?;
        self.send_command(ItsCommand::mapti(
            request.device.0,
            request.event.0,
            request.lpi,
            collection.id,
        ))?;
        self.send_command(ItsCommand::sync(collection.target))?;

        let leaf_irq = IrqId::new(self.msix_domain, HwIrq(request.lpi - LPI_INTID_BASE));
        crate::irq::map_irq_route(request.parent_irq, leaf_irq)?;
        self.translations
            .insert((request.device.0, request.event.0), request.lpi);
        self.lpis.insert(
            request.lpi,
            LpiRoute {
                device: request.device,
                event: request.event,
                leaf_irq,
                collection,
            },
        );
        if let Some(device) = self.devices.get_mut(&request.device.0) {
            device.next_event = device.next_event.max(request.event.0.saturating_add(1));
        }
        Ok(MsiVector::with_parent(
            request.index,
            request.event,
            leaf_irq,
            request.parent_irq,
        ))
    }

    fn remove_translation(&mut self, vector: &MsiVector) -> Result<(), IrqError> {
        let intid = vector.parent_irq.hwirq.0;
        let route = *self.lpis.get(&intid).ok_or(IrqError::InvalidIrq)?;
        if route.leaf_irq != vector.irq || route.event != vector.event {
            return Err(IrqError::InvalidIrq);
        }
        self.set_property_enabled(intid, false)?;
        self.send_command(ItsCommand::discard(route.device.0, route.event.0))?;
        self.send_command(ItsCommand::sync(route.collection.target))?;
        crate::irq::unmap_irq_route(vector.parent_irq, vector.irq)?;
        self.translations.remove(&(route.device.0, route.event.0));
        self.lpis.remove(&intid);
        self.release_lpi(intid);
        Ok(())
    }

    fn send_command(&mut self, command: ItsCommand) -> Result<(), IrqError> {
        self.command_queue.push(&self.its, command)
    }

    fn set_lpi_enabled_by_intid(&mut self, intid: u32, enabled: bool) -> Result<(), IrqError> {
        let route = *self.lpis.get(&intid).ok_or(IrqError::InvalidIrq)?;
        self.set_property_enabled(intid, enabled)?;
        self.send_command(ItsCommand::inv(route.device.0, route.event.0))?;
        self.send_command(ItsCommand::sync(route.collection.target))
    }

    fn set_property_enabled(&self, intid: u32, enabled: bool) -> Result<(), IrqError> {
        let offset = intid
            .checked_sub(LPI_INTID_BASE)
            .ok_or(IrqError::InvalidIrq)? as usize;
        if offset >= LPI_PROPERTY_BYTES {
            return Err(IrqError::InvalidIrq);
        }
        let value = LPI_DEFAULT_PRIORITY | u8::from(enabled);
        unsafe {
            self.property_table
                .ptr()
                .as_ptr()
                .add(offset)
                .write_volatile(value)
        };
        self.property_table.clean_range(offset, 1);
        Ok(())
    }
}

impl rdif_msi::DriverGeneric for GicItsProvider {
    fn name(&self) -> &str {
        "gic-v3-its"
    }
}

impl Interface for GicItsProvider {
    fn allocate_vectors(&mut self, request: &MsiRequest) -> Result<Vec<MsiVector>, IrqError> {
        let count = request.vector_count;
        if count == 0 {
            return Err(IrqError::InvalidIrq);
        }
        let device = request.device;
        let mut vectors = Vec::with_capacity(usize::from(count));
        for index in 0..count {
            let event = MsiEventId(
                self.devices
                    .get(&device.0)
                    .map(|state| state.next_event)
                    .unwrap_or(0),
            );
            let vector = loop {
                let lpi = match self.next_lpi_candidate() {
                    Ok(lpi) => lpi,
                    Err(error) => {
                        self.rollback_vectors(&vectors);
                        return Err(error);
                    }
                };
                let translation = TranslationRequest {
                    device,
                    index: MsiVectorIndex(index),
                    event,
                    lpi,
                    parent_irq: IrqId::new(self.gic_domain, HwIrq(lpi)),
                    affinity: request.affinity,
                };
                match self.install_translation(translation) {
                    Ok(vector) => break vector,
                    Err(IrqError::Busy) => continue,
                    Err(error) => {
                        self.rollback_vectors(&vectors);
                        return Err(error);
                    }
                }
            };
            vectors.push(vector);
        }
        Ok(vectors)
    }

    fn reserve_vector(&mut self, request: &MsiReservationRequest) -> Result<MsiVector, IrqError> {
        self.install_translation(TranslationRequest {
            device: request.device(),
            index: request.index(),
            event: request.event(),
            lpi: request.parent_irq().hwirq.0,
            parent_irq: request.parent_irq(),
            affinity: request.requested_affinity(),
        })
    }

    fn compose_message(&self, vector: &MsiVector) -> Result<MsiMessage, IrqError> {
        Ok(MsiMessage::new(
            self.its.translater_address(),
            vector.event.0,
        ))
    }

    fn set_vector_enabled(&mut self, vector: &MsiVector, enabled: bool) -> Result<(), IrqError> {
        self.set_lpi_enabled_by_intid(vector.parent_irq.hwirq.0, enabled)
    }

    fn set_vector_affinity(
        &mut self,
        _vector: &MsiVector,
        affinity: irq_framework::IrqAffinity,
    ) -> Result<(), IrqError> {
        match affinity {
            irq_framework::IrqAffinity::Any => Ok(()),
            irq_framework::IrqAffinity::Fixed { .. } => Err(IrqError::Unsupported),
        }
    }

    fn free_vectors(&mut self, allocation: MsiAllocation) -> Result<(), IrqError> {
        for vector in allocation.vectors() {
            let intid = vector.parent_irq.hwirq.0;
            let route = *self.lpis.get(&intid).ok_or(IrqError::InvalidIrq)?;
            if route.device != allocation.device()
                || route.event != vector.event
                || route.leaf_irq != vector.irq
            {
                return Err(IrqError::InvalidIrq);
            }
        }
        for vector in allocation.vectors() {
            self.remove_translation(vector)?;
        }
        Ok(())
    }
}

impl GicItsProvider {
    fn rollback_vectors(&mut self, vectors: &[MsiVector]) {
        for vector in vectors.iter().rev() {
            let _ = self.remove_translation(vector);
        }
    }
}

struct ItsDevice {
    _itt: AlignedMemory,
    event_capacity: u32,
    next_event: u32,
}

#[derive(Clone, Copy)]
struct ItsCollection {
    id: u16,
    target: u64,
}

#[derive(Clone, Copy)]
struct TranslationRequest {
    device: MsiDeviceId,
    index: MsiVectorIndex,
    event: MsiEventId,
    lpi: u32,
    parent_irq: IrqId,
    affinity: irq_framework::IrqAffinity,
}

#[derive(Clone, Copy)]
struct LpiRoute {
    device: MsiDeviceId,
    event: MsiEventId,
    leaf_irq: IrqId,
    collection: ItsCollection,
}

struct CommandQueue {
    mem: AlignedMemory,
    entries: usize,
    write_index: usize,
}

impl CommandQueue {
    fn new(entries: usize) -> Option<Self> {
        let mem = AlignedMemory::new(
            entries.checked_mul(core::mem::size_of::<ItsCommand>())?,
            4096,
        )?;
        Some(Self {
            mem,
            entries,
            write_index: 0,
        })
    }

    fn bytes(&self) -> usize {
        self.mem.len()
    }

    fn phys(&self) -> u64 {
        self.mem.phys()
    }

    fn push(&mut self, its: &Its, command: ItsCommand) -> Result<(), IrqError> {
        let next = (self.write_index + 1) % self.entries;
        let next_offset = next * core::mem::size_of::<ItsCommand>();
        let mut retries = 0;
        while its.creadr_offset() == next_offset {
            retries += 1;
            if retries > 1_000_000 {
                return Err(IrqError::Timeout);
            }
            core::hint::spin_loop();
        }

        let offset = self.write_index * core::mem::size_of::<ItsCommand>();
        unsafe {
            self.mem
                .ptr()
                .as_ptr()
                .add(offset)
                .cast::<ItsCommand>()
                .write_volatile(command);
        }
        self.mem
            .clean_range(offset, core::mem::size_of::<ItsCommand>());
        self.write_index = next;
        its.write_cwriter(next_offset);

        retries = 0;
        while its.creadr_offset() != next_offset {
            retries += 1;
            if retries > 1_000_000 {
                return Err(IrqError::Timeout);
            }
            core::hint::spin_loop();
        }
        Ok(())
    }
}

struct AlignedMemory {
    ptr: NonNull<u8>,
    len: usize,
    layout: Layout,
}

unsafe impl Send for AlignedMemory {}

impl AlignedMemory {
    fn new(len: usize, align: usize) -> Option<Self> {
        let len = len.max(1);
        let layout = Layout::from_size_align(len, align).ok()?;
        let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
        Some(Self { ptr, len, layout })
    }

    fn ptr(&self) -> NonNull<u8> {
        self.ptr
    }

    fn len(&self) -> usize {
        self.len
    }

    fn phys(&self) -> u64 {
        someboot::mem::virt_to_phys(self.ptr.as_ptr()) as u64
    }

    fn fill(&self, value: u8) {
        unsafe { core::ptr::write_bytes(self.ptr.as_ptr(), value, self.len) };
    }

    fn clean(&self) {
        self.clean_range(0, self.len);
    }

    fn clean_range(&self, offset: usize, len: usize) {
        if offset >= self.len {
            return;
        }
        let len = len.min(self.len - offset);
        someboot::mem::dcache_range(
            DCacheOp::Clean,
            unsafe { self.ptr.as_ptr().add(offset) },
            len,
        );
    }
}

impl Drop for AlignedMemory {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr.as_ptr(), self.layout) };
    }
}

fn program_baser_table(
    its: &Its,
    table_type: ItsTableType,
    max_entries_log2: u8,
) -> Result<(), OnProbeError> {
    for index in 0..8 {
        if its.baser_type(index) != Some(table_type) {
            continue;
        }
        let entry_size = its.baser_entry_size(index).max(8);
        let entries = 1usize << usize::from(max_entries_log2);
        let bytes = entry_size
            .checked_mul(entries)
            .ok_or_else(|| OnProbeError::other("ITS BASER table size overflow"))?;
        let table = AlignedMemory::new(bytes, 4096)
            .ok_or_else(|| OnProbeError::other("failed to allocate ITS BASER table"))?;
        table.clean();
        its.program_baser(
            index,
            Its::baser_value(table_type, table.phys(), table.len(), entry_size),
        );
        core::mem::forget(table);
        return Ok(());
    }
    Err(OnProbeError::Unsupported(
        "required ITS BASER table is not implemented",
    ))
}

const fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}
