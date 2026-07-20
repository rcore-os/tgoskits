use std::sync::{Arc, Mutex};

use arm_vgic::{
    CpuInterfaceState, EventId, GicAffinity, GicV3Backend, GicV3BackendError, GicV3Config,
    GicV3Controller, GicV3MmioRegion, GicV3Mode, GicV3VcpuWake, GicVcpuId, IntId, ItsDeviceId,
    ListRegisterBacking, LpiId, PhysicalInterruptBinding, PhysicalIrqId, PhysicalMsiBinding, PpiId,
    SgiId, SgiTarget, SpiId, TriggerMode, VgicError, VgicResult,
};
use axvm_types::AccessWidth;

#[test]
fn passthrough_delivery_uses_only_owned_physical_routes() {
    let backend = Arc::new(PhysicalBackend::default());
    let controller = GicV3Controller::new(config(), backend.clone()).unwrap();
    let binding0 = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let binding1 = attach(&controller, 1, GicAffinity::new(0, 0, 0, 1));
    let spi = SpiId::new(40).unwrap();

    assert!(matches!(
        controller.configure_spi_input(spi, TriggerMode::Level),
        Err(VgicError::Unsupported { .. })
    ));
    controller
        .bind_physical_spi(spi, PhysicalIrqId::new(1040), GicVcpuId::new(1))
        .unwrap();
    controller
        .configure_spi_input(spi, TriggerMode::Level)
        .unwrap();
    controller.set_spi_level(spi, true).unwrap();
    controller.pulse_spi(spi).unwrap();

    let device = ItsDeviceId::new(12);
    let event = EventId::new(7);
    controller
        .bind_physical_msi(device, event, LpiId::new(9000).unwrap(), GicVcpuId::new(1))
        .unwrap();
    controller.signal_msi(device, event).unwrap();
    binding1.load().unwrap();
    controller
        .send_sgi(
            GicVcpuId::new(0),
            SgiId::new(2).unwrap(),
            SgiTarget::Affinities(vec![GicAffinity::new(0, 0, 0, 1)]),
        )
        .unwrap();
    binding1.synchronize().unwrap();
    binding1.save().unwrap();

    assert_eq!(
        controller
            .software_pending_count(GicVcpuId::new(1))
            .unwrap(),
        0
    );
    assert!(matches!(
        controller.write_its(0, AccessWidth::Dword, 1),
        Err(VgicError::Unsupported { .. })
    ));
    let records = backend.records.lock().unwrap();
    assert_eq!(records.bound_interrupts.len(), 1);
    assert_eq!(records.levels, vec![(records.bound_interrupts[0], true)]);
    assert_eq!(records.pulses, records.bound_interrupts);
    assert_eq!(records.bound_msi.len(), 1);
    assert_eq!(records.signaled_msi, records.bound_msi);
    drop(records);

    drop(binding0);
    drop(binding1);
    drop(controller);
    let records = backend.records.lock().unwrap();
    assert_eq!(records.unbound_interrupts, records.bound_interrupts);
    assert_eq!(records.unbound_msi, records.bound_msi);
}

#[test]
fn passthrough_redistributor_is_vm_local_and_never_aliases_host_ppis() {
    const GICR_SGI_BASE: u64 = 0x1_0000;
    const GICR_ISENABLER0: u64 = GICR_SGI_BASE + 0x100;

    let backend = Arc::new(PhysicalBackend::default());
    let controller = GicV3Controller::new(config(), backend).unwrap();
    let _binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let host_timer = 1u64 << 26;
    let guest_timer = 1u64 << 27;

    controller
        .write_redistributor(
            GicVcpuId::new(0),
            GICR_ISENABLER0,
            AccessWidth::Dword,
            host_timer | guest_timer,
        )
        .unwrap();

    let enabled = controller
        .read_redistributor(GicVcpuId::new(0), GICR_ISENABLER0, AccessWidth::Dword)
        .unwrap();
    assert_eq!(
        enabled & (host_timer | guest_timer),
        host_timer | guest_timer
    );
}

#[test]
fn passthrough_distributor_masks_unassigned_spis_in_mixed_writes() {
    const GICD_ISENABLER1: u64 = 0x104;

    let backend = Arc::new(PhysicalBackend::default());
    let controller = GicV3Controller::new(config(), backend).unwrap();
    let _binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let guest_spi = SpiId::new(40).unwrap();
    let host_spi = SpiId::new(41).unwrap();
    controller
        .bind_physical_spi(guest_spi, PhysicalIrqId::new(1040), GicVcpuId::new(0))
        .unwrap();

    let guest_bit = 1u64 << (guest_spi.raw() - 32);
    let host_bit = 1u64 << (host_spi.raw() - 32);
    controller
        .write_distributor(GICD_ISENABLER1, AccessWidth::Dword, guest_bit | host_bit)
        .unwrap();

    let enabled = controller
        .read_distributor(GICD_ISENABLER1, AccessWidth::Dword)
        .unwrap();
    assert_eq!(enabled & (guest_bit | host_bit), guest_bit);
}

#[test]
fn passthrough_sgis_and_ppis_use_virtual_list_registers() {
    const GICR_SGI_BASE: u64 = 0x1_0000;
    const GICR_ISENABLER0: u64 = GICR_SGI_BASE + 0x100;
    const GICR_ISPENDR0: u64 = GICR_SGI_BASE + 0x200;

    let backend = Arc::new(PhysicalBackend::default());
    let controller = GicV3Controller::new(config(), backend.clone()).unwrap();
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let guest_timer = IntId::Ppi(PpiId::new(27).unwrap());
    controller
        .write_redistributor(
            GicVcpuId::new(0),
            GICR_ISENABLER0,
            AccessWidth::Dword,
            1 << guest_timer.raw(),
        )
        .unwrap();
    controller
        .write_redistributor(
            GicVcpuId::new(0),
            GICR_ISPENDR0,
            AccessWidth::Dword,
            1 << guest_timer.raw(),
        )
        .unwrap();

    binding.load().unwrap();

    let records = backend.records.lock().unwrap();
    let loaded = records.loaded_cpu_interfaces.last().unwrap();
    assert!(loaded.list_registers().iter().flatten().any(|entry| {
        entry.intid() == guest_timer && entry.backing() == ListRegisterBacking::Software
    }));
    drop(records);

    let pending = controller
        .read_redistributor(GicVcpuId::new(0), GICR_ISPENDR0, AccessWidth::Dword)
        .unwrap();
    assert_ne!(pending & (1 << guest_timer.raw()), 0);
}

#[test]
fn passthrough_sgi_is_queued_until_the_target_vcpu_is_loaded() {
    let backend = Arc::new(PhysicalBackend::default());
    let controller = GicV3Controller::new(config(), backend.clone()).unwrap();
    let _binding0 = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let binding1 = attach(&controller, 1, GicAffinity::new(0, 0, 0, 1));
    let sgi = SgiId::new(2).unwrap();

    controller
        .send_sgi(
            GicVcpuId::new(0),
            sgi,
            SgiTarget::Affinities(vec![GicAffinity::new(0, 0, 0, 1)]),
        )
        .unwrap();
    assert_eq!(
        controller
            .software_pending_count(GicVcpuId::new(1))
            .unwrap(),
        1
    );

    binding1.load().unwrap();
    let records = backend.records.lock().unwrap();
    let loaded = records.loaded_cpu_interfaces.last().unwrap();
    assert!(
        loaded
            .list_registers()
            .iter()
            .flatten()
            .any(|entry| entry.intid() == IntId::Sgi(sgi))
    );
}

#[test]
fn passthrough_rejects_missing_affinity_and_duplicate_ownership() {
    let backend = Arc::new(PhysicalBackend::default());
    let controller = GicV3Controller::new(config(), backend).unwrap();
    let _binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let spi = SpiId::new(41).unwrap();

    assert!(matches!(
        controller.bind_physical_spi(spi, PhysicalIrqId::new(1), GicVcpuId::new(1)),
        Err(VgicError::ResourceNotFound { .. })
    ));
    controller
        .bind_physical_spi(spi, PhysicalIrqId::new(1), GicVcpuId::new(0))
        .unwrap();
    assert!(matches!(
        controller.bind_physical_spi(spi, PhysicalIrqId::new(2), GicVcpuId::new(0)),
        Err(VgicError::ResourceConflict { .. })
    ));
    assert!(matches!(
        controller.bind_physical_spi(
            SpiId::new(42).unwrap(),
            PhysicalIrqId::new(1),
            GicVcpuId::new(0)
        ),
        Err(VgicError::ResourceConflict { .. })
    ));

    controller
        .bind_physical_msi(
            ItsDeviceId::new(7),
            EventId::new(1),
            LpiId::new(9000).unwrap(),
            GicVcpuId::new(0),
        )
        .unwrap();
    assert!(matches!(
        controller.bind_physical_msi(
            ItsDeviceId::new(7),
            EventId::new(2),
            LpiId::new(9000).unwrap(),
            GicVcpuId::new(0)
        ),
        Err(VgicError::ResourceConflict { .. })
    ));
    assert!(matches!(
        controller.signal_msi(ItsDeviceId::new(99), EventId::new(1)),
        Err(VgicError::Unsupported { .. })
    ));
}

#[test]
fn passthrough_vcpu_binding_uses_the_virtual_cpu_interface() {
    let backend = Arc::new(PhysicalBackend::default());
    let controller = GicV3Controller::new(config(), backend.clone()).unwrap();
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));

    binding.load().unwrap();
    binding.synchronize().unwrap();
    binding.save().unwrap();

    let records = backend.records.lock().unwrap();
    assert_eq!(records.cpu_interface_loads, 2);
    assert_eq!(records.cpu_interface_saves, 2);
}

#[test]
fn passthrough_physical_spi_is_delivered_by_a_hardware_backed_lr() {
    const GICD_CTLR: u64 = 0;
    const GICD_ISENABLER1: u64 = 0x104;

    let backend = Arc::new(PhysicalBackend::default());
    let controller = GicV3Controller::new(config(), backend.clone()).unwrap();
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let spi = SpiId::new(40).unwrap();
    let physical = PhysicalIrqId::new(40);
    controller
        .bind_physical_spi(spi, physical, GicVcpuId::new(0))
        .unwrap();

    binding.load().unwrap();
    controller
        .write_distributor(GICD_ISENABLER1, AccessWidth::Dword, 1 << (spi.raw() - 32))
        .unwrap();
    controller
        .write_distributor(GICD_CTLR, AccessWidth::Dword, 1 << 1)
        .unwrap();
    binding.save().unwrap();

    controller.forward_physical_spi(spi).unwrap();
    binding.load().unwrap();

    let records = backend.records.lock().unwrap();
    let entry = records
        .loaded_cpu_interfaces
        .last()
        .unwrap()
        .list_registers()
        .iter()
        .flatten()
        .next()
        .copied()
        .unwrap();
    assert_eq!(entry.intid(), IntId::Spi(spi));
    assert_eq!(entry.backing(), ListRegisterBacking::Physical(physical));
}

#[test]
fn passthrough_spi_enable_tracks_guest_register_writes_not_vcpu_load() {
    const GICD_CTLR: u64 = 0;
    const GICD_ISENABLER1: u64 = 0x104;
    const GICD_ICENABLER1: u64 = 0x184;

    let backend = Arc::new(PhysicalBackend::default());
    let controller = GicV3Controller::new(config(), backend.clone()).unwrap();
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let spi = SpiId::new(40).unwrap();
    controller
        .bind_physical_spi(spi, PhysicalIrqId::new(1040), GicVcpuId::new(0))
        .unwrap();
    let physical_binding = backend.records.lock().unwrap().bound_interrupts[0];

    binding.load().unwrap();
    controller
        .write_distributor(GICD_CTLR, AccessWidth::Dword, 1 << 1)
        .unwrap();
    controller
        .write_distributor(GICD_ISENABLER1, AccessWidth::Dword, 1 << (spi.raw() - 32))
        .unwrap();
    binding.save().unwrap();
    binding.load().unwrap();
    controller
        .write_distributor(GICD_ICENABLER1, AccessWidth::Dword, 1 << (spi.raw() - 32))
        .unwrap();
    binding.save().unwrap();

    let enabled_interrupts = backend.records.lock().unwrap().enabled_interrupts.clone();
    assert_eq!(
        enabled_interrupts,
        vec![(physical_binding, true), (physical_binding, false)]
    );
}

#[test]
fn passthrough_spi_enable_is_gated_by_the_distributor() {
    const GICD_CTLR: u64 = 0;
    const GICD_ISENABLER1: u64 = 0x104;

    let backend = Arc::new(PhysicalBackend::default());
    let controller = GicV3Controller::new(config(), backend.clone()).unwrap();
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let spi = SpiId::new(40).unwrap();
    controller
        .bind_physical_spi(spi, PhysicalIrqId::new(1040), GicVcpuId::new(0))
        .unwrap();
    let physical_binding = backend.records.lock().unwrap().bound_interrupts[0];

    binding.load().unwrap();
    controller
        .write_distributor(GICD_ISENABLER1, AccessWidth::Dword, 1 << (spi.raw() - 32))
        .unwrap();
    assert!(
        backend
            .records
            .lock()
            .unwrap()
            .enabled_interrupts
            .is_empty()
    );

    controller
        .write_distributor(GICD_CTLR, AccessWidth::Dword, 1 << 1)
        .unwrap();
    controller
        .write_distributor(GICD_CTLR, AccessWidth::Dword, 0)
        .unwrap();

    assert_eq!(
        backend.records.lock().unwrap().enabled_interrupts,
        vec![(physical_binding, true), (physical_binding, false)]
    );
}

#[test]
fn passthrough_vcpu_reload_does_not_rewrite_distributor_spi_state() {
    let backend = Arc::new(PhysicalBackend::default());
    let controller = GicV3Controller::new(config(), backend.clone()).unwrap();
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let spi = SpiId::new(40).unwrap();
    controller
        .bind_physical_spi(spi, PhysicalIrqId::new(1040), GicVcpuId::new(0))
        .unwrap();
    backend.records.lock().unwrap().enabled_interrupts.clear();

    binding.load().unwrap();
    binding.save().unwrap();
    binding.load().unwrap();

    assert!(
        backend
            .records
            .lock()
            .unwrap()
            .enabled_interrupts
            .is_empty(),
        "vCPU context switches must not restore Distributor state from a stale software snapshot"
    );
}

#[test]
fn passthrough_vcpu_save_keeps_owned_distributor_spi_enabled() {
    const GICD_ISENABLER1: u64 = 0x104;

    let backend = Arc::new(PhysicalBackend::default());
    let controller = GicV3Controller::new(config(), backend.clone()).unwrap();
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let spi = SpiId::new(40).unwrap();
    controller
        .bind_physical_spi(spi, PhysicalIrqId::new(1040), GicVcpuId::new(0))
        .unwrap();

    binding.load().unwrap();
    controller
        .write_distributor(GICD_ISENABLER1, AccessWidth::Dword, 1 << (spi.raw() - 32))
        .unwrap();
    backend.records.lock().unwrap().enabled_interrupts.clear();

    binding.save().unwrap();

    assert!(
        backend
            .records
            .lock()
            .unwrap()
            .enabled_interrupts
            .is_empty(),
        "Distributor SPI state belongs to the VM device lease, not a vCPU run slice"
    );
}

#[test]
fn passthrough_spi_enable_failure_restores_disabled_hardware_and_software_state() {
    const GICD_CTLR: u64 = 0;
    const GICD_ISENABLER1: u64 = 0x104;

    let backend = Arc::new(PhysicalBackend::default());
    let controller = GicV3Controller::new(config(), backend.clone()).unwrap();
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let spi = SpiId::new(40).unwrap();
    controller
        .bind_physical_spi(spi, PhysicalIrqId::new(1040), GicVcpuId::new(0))
        .unwrap();
    binding.load().unwrap();
    controller
        .write_distributor(GICD_CTLR, AccessWidth::Dword, 1 << 1)
        .unwrap();
    backend.records.lock().unwrap().fail_next_enable = true;

    assert!(matches!(
        controller.write_distributor(GICD_ISENABLER1, AccessWidth::Dword, 1 << (spi.raw() - 32),),
        Err(VgicError::Backend { .. })
    ));

    let (physical_binding, enabled_interrupts) = {
        let records = backend.records.lock().unwrap();
        (
            records.bound_interrupts[0],
            records.enabled_interrupts.clone(),
        )
    };
    assert_eq!(
        enabled_interrupts,
        vec![(physical_binding, true), (physical_binding, false)]
    );
    assert_eq!(
        controller
            .read_distributor(GICD_ISENABLER1, AccessWidth::Dword)
            .unwrap()
            & (1 << (spi.raw() - 32)),
        0
    );
}

#[test]
fn passthrough_guest_state_never_reconfigures_an_inflight_physical_spi() {
    const GICD_CTLR: u64 = 0;
    const GICD_ISENABLER1: u64 = 0x104;
    const GICD_ISPENDR1: u64 = 0x204;
    const GICD_ICPENDR1: u64 = 0x284;
    const GICD_ISACTIVER1: u64 = 0x304;
    const GICD_ICACTIVER1: u64 = 0x384;
    const GICD_IPRIORITYR10: u64 = 0x428;
    const GICD_ICFGR2: u64 = 0xc08;

    let backend = Arc::new(PhysicalBackend::default());
    let controller = GicV3Controller::new(config(), backend.clone()).unwrap();
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let guest_spi = SpiId::new(40).unwrap();
    controller
        .bind_physical_spi(guest_spi, PhysicalIrqId::new(1040), GicVcpuId::new(0))
        .unwrap();
    binding.load().unwrap();
    let guest_bit = 1 << (guest_spi.raw() - 32);
    controller
        .write_distributor(GICD_CTLR, AccessWidth::Dword, 1 << 1)
        .unwrap();
    controller
        .write_distributor(GICD_ISENABLER1, AccessWidth::Dword, guest_bit)
        .unwrap();
    controller.forward_physical_spi(guest_spi).unwrap();
    binding.synchronize().unwrap();
    {
        let mut records = backend.records.lock().unwrap();
        records.enabled_interrupts.clear();
    }

    controller
        .write_distributor(GICD_IPRIORITYR10, AccessWidth::Dword, 0x8080_1020)
        .unwrap();
    controller
        .write_distributor(
            GICD_ICFGR2,
            AccessWidth::Dword,
            1 << ((guest_spi.raw() % 16) * 2 + 1),
        )
        .unwrap();
    controller
        .write_distributor(GICD_ISPENDR1, AccessWidth::Dword, guest_bit)
        .unwrap();
    controller
        .write_distributor(GICD_ISACTIVER1, AccessWidth::Dword, guest_bit)
        .unwrap();
    controller
        .write_distributor(GICD_ICPENDR1, AccessWidth::Dword, guest_bit)
        .unwrap();
    controller
        .write_distributor(GICD_ICACTIVER1, AccessWidth::Dword, guest_bit)
        .unwrap();

    let enabled_interrupts = backend.records.lock().unwrap().enabled_interrupts.clone();
    assert!(
        enabled_interrupts.is_empty(),
        "guest virtual state must not mask or rewrite the physical active lifecycle owned by a \
         hardware-backed LR"
    );
}

#[test]
fn passthrough_repeated_set_pending_writes_stay_vm_local() {
    const GICD_ISPENDR1: u64 = 0x204;

    let backend = Arc::new(PhysicalBackend::default());
    let controller = GicV3Controller::new(config(), backend.clone()).unwrap();
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let spi = SpiId::new(40).unwrap();
    controller
        .bind_physical_spi(spi, PhysicalIrqId::new(1040), GicVcpuId::new(0))
        .unwrap();
    binding.load().unwrap();
    backend.records.lock().unwrap().enabled_interrupts.clear();

    let pending_bit = 1 << (spi.raw() - 32);
    controller
        .write_distributor(GICD_ISPENDR1, AccessWidth::Dword, pending_bit)
        .unwrap();
    controller
        .write_distributor(GICD_ISPENDR1, AccessWidth::Dword, pending_bit)
        .unwrap();

    assert!(
        backend
            .records
            .lock()
            .unwrap()
            .enabled_interrupts
            .is_empty()
    );
}

fn config() -> GicV3Config {
    GicV3Config::new(
        GicV3Mode::Passthrough,
        GicV3MmioRegion::new(0x0800_0000, 0x1_0000).unwrap(),
        GicV3MmioRegion::new(0x080a_0000, 0x4_0000).unwrap(),
        0x2_0000,
        2,
    )
    .unwrap()
    .with_spi_count(32)
    .unwrap()
    .with_its(GicV3MmioRegion::new(0x0808_0000, 0x2_0000).unwrap())
    .unwrap()
}

fn attach(
    controller: &GicV3Controller,
    raw_vcpu: usize,
    affinity: GicAffinity,
) -> arm_vgic::GicV3VcpuBinding {
    controller
        .attach_vcpu(GicVcpuId::new(raw_vcpu), affinity, Arc::new(NoopWake))
        .unwrap()
}

struct NoopWake;

impl GicV3VcpuWake for NoopWake {
    fn wake(&self) -> VgicResult {
        Ok(())
    }
}

#[derive(Default)]
struct PhysicalBackend {
    records: Mutex<PhysicalRecords>,
}

#[derive(Default)]
struct PhysicalRecords {
    cpu_interface_loads: usize,
    cpu_interface_saves: usize,
    loaded_cpu_interfaces: Vec<CpuInterfaceState>,
    bound_interrupts: Vec<PhysicalInterruptBinding>,
    enabled_interrupts: Vec<(PhysicalInterruptBinding, bool)>,
    levels: Vec<(PhysicalInterruptBinding, bool)>,
    pulses: Vec<PhysicalInterruptBinding>,
    bound_msi: Vec<PhysicalMsiBinding>,
    signaled_msi: Vec<PhysicalMsiBinding>,
    unbound_interrupts: Vec<PhysicalInterruptBinding>,
    unbound_msi: Vec<PhysicalMsiBinding>,
    fail_next_enable: bool,
}

impl GicV3Backend for PhysicalBackend {
    fn load_cpu_interface(
        &self,
        _vcpu: GicVcpuId,
        state: &CpuInterfaceState,
    ) -> Result<(), GicV3BackendError> {
        let mut records = self.records.lock().unwrap();
        records.cpu_interface_loads += 1;
        records.loaded_cpu_interfaces.push(state.clone());
        Ok(())
    }

    fn save_cpu_interface(
        &self,
        _vcpu: GicVcpuId,
        _state: &mut CpuInterfaceState,
    ) -> Result<(), GicV3BackendError> {
        self.records.lock().unwrap().cpu_interface_saves += 1;
        Ok(())
    }

    fn bind_physical_interrupt(
        &self,
        binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        self.records.lock().unwrap().bound_interrupts.push(binding);
        Ok(())
    }

    fn set_physical_interrupt_enabled(
        &self,
        binding: PhysicalInterruptBinding,
        enabled: bool,
    ) -> Result<(), GicV3BackendError> {
        let mut records = self.records.lock().unwrap();
        records.enabled_interrupts.push((binding, enabled));
        if enabled && core::mem::take(&mut records.fail_next_enable) {
            return Err(GicV3BackendError::new(
                "enable physical interrupt",
                "injected activation failure",
            ));
        }
        Ok(())
    }

    fn set_physical_interrupt_level(
        &self,
        binding: PhysicalInterruptBinding,
        asserted: bool,
    ) -> Result<(), GicV3BackendError> {
        self.records
            .lock()
            .unwrap()
            .levels
            .push((binding, asserted));
        Ok(())
    }

    fn pulse_physical_interrupt(
        &self,
        binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        self.records.lock().unwrap().pulses.push(binding);
        Ok(())
    }

    fn bind_physical_msi(&self, binding: PhysicalMsiBinding) -> Result<(), GicV3BackendError> {
        self.records.lock().unwrap().bound_msi.push(binding);
        Ok(())
    }

    fn signal_physical_msi(&self, binding: PhysicalMsiBinding) -> Result<(), GicV3BackendError> {
        self.records.lock().unwrap().signaled_msi.push(binding);
        Ok(())
    }

    fn unbind_physical_interrupt(
        &self,
        binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        self.records
            .lock()
            .unwrap()
            .unbound_interrupts
            .push(binding);
        Ok(())
    }

    fn unbind_physical_msi(&self, binding: PhysicalMsiBinding) -> Result<(), GicV3BackendError> {
        self.records.lock().unwrap().unbound_msi.push(binding);
        Ok(())
    }
}
