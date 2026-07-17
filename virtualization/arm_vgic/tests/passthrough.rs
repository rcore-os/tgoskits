use std::sync::{Arc, Mutex};

use arm_vgic::{
    CpuInterfaceState, EventId, GicAffinity, GicV3Backend, GicV3BackendError, GicV3Config,
    GicV3Controller, GicV3MmioRegion, GicV3Mode, GicV3VcpuWake, GicVcpuId, IntId, ItsDeviceId,
    LpiId, PhysicalInterruptBinding, PhysicalInterruptConfiguration, PhysicalIrqId,
    PhysicalMsiBinding, PpiId, Priority, PrivateInterruptMask, PrivateInterruptState, SgiId,
    SgiTarget, SpiId, TriggerMode, VgicError, VgicResult,
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
    assert_eq!(records.sgis.len(), 1);
    assert_eq!(records.sgis[0].2, vec![GicAffinity::new(0, 0, 0, 1)]);
    drop(records);

    drop(binding0);
    drop(binding1);
    drop(controller);
    let records = backend.records.lock().unwrap();
    assert_eq!(records.unbound_interrupts, records.bound_interrupts);
    assert_eq!(records.unbound_msi, records.bound_msi);
}

#[test]
fn passthrough_redistributor_masks_host_owned_ppis_in_mixed_writes() {
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
    assert_eq!(enabled & (host_timer | guest_timer), guest_timer);
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
fn passthrough_vcpu_binding_switches_guest_private_interrupt_state() {
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

    binding.load().unwrap();
    binding.synchronize().unwrap();
    binding.save().unwrap();

    let records = backend.records.lock().unwrap();
    assert_eq!(records.private_loads.len(), 1);
    assert_eq!(records.private_synchronizations.len(), 1);
    assert_eq!(records.private_saves.len(), 1);
    assert!(records.private_loads[0].2.enabled_mask() & (1 << guest_timer.raw()) != 0);
    drop(records);

    let pending = controller
        .read_redistributor(GicVcpuId::new(0), GICR_ISPENDR0, AccessWidth::Dword)
        .unwrap();
    assert_ne!(pending & (1 << guest_timer.raw()), 0);
}

#[test]
fn passthrough_sgi_waits_until_the_target_vcpu_is_loaded() {
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
    assert!(backend.records.lock().unwrap().sgis.is_empty());

    binding1.load().unwrap();
    let records = backend.records.lock().unwrap();
    let loaded = &records.private_loads[0].2;
    assert_ne!(loaded.pending_mask() & (1 << sgi.raw()), 0);
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
fn passthrough_vcpu_binding_never_uses_virtual_list_registers() {
    let backend = Arc::new(PhysicalBackend::default());
    let controller = GicV3Controller::new(config(), backend.clone()).unwrap();
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));

    binding.load().unwrap();
    binding.synchronize().unwrap();
    binding.save().unwrap();

    let records = backend.records.lock().unwrap();
    assert_eq!(records.cpu_interface_loads, 0);
    assert_eq!(records.cpu_interface_saves, 0);
}

#[test]
fn passthrough_spi_enable_tracks_guest_register_writes_not_vcpu_load() {
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
fn passthrough_vcpu_reload_does_not_rewrite_distributor_spi_state() {
    let backend = Arc::new(PhysicalBackend::default());
    let controller = GicV3Controller::new(config(), backend.clone()).unwrap();
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let spi = SpiId::new(40).unwrap();
    controller
        .bind_physical_spi(spi, PhysicalIrqId::new(1040), GicVcpuId::new(0))
        .unwrap();
    backend
        .records
        .lock()
        .unwrap()
        .configured_interrupts
        .clear();

    binding.load().unwrap();
    binding.save().unwrap();
    binding.load().unwrap();

    assert!(
        backend
            .records
            .lock()
            .unwrap()
            .configured_interrupts
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
    const GICD_ISENABLER1: u64 = 0x104;

    let backend = Arc::new(PhysicalBackend::default());
    let controller = GicV3Controller::new(config(), backend.clone()).unwrap();
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let spi = SpiId::new(40).unwrap();
    controller
        .bind_physical_spi(spi, PhysicalIrqId::new(1040), GicVcpuId::new(0))
        .unwrap();
    binding.load().unwrap();
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
fn passthrough_updates_only_assigned_physical_spi_configuration() {
    const GICD_ISPENDR1: u64 = 0x204;
    const GICD_IPRIORITYR10: u64 = 0x428;
    const GICD_ICFGR2: u64 = 0xc08;

    let backend = Arc::new(PhysicalBackend::default());
    let controller = GicV3Controller::new(config(), backend.clone()).unwrap();
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let guest_spi = SpiId::new(40).unwrap();
    let host_spi = SpiId::new(41).unwrap();
    controller
        .bind_physical_spi(guest_spi, PhysicalIrqId::new(1040), GicVcpuId::new(0))
        .unwrap();
    let physical_binding = backend.records.lock().unwrap().bound_interrupts[0];
    binding.load().unwrap();
    backend
        .records
        .lock()
        .unwrap()
        .configured_interrupts
        .clear();

    controller
        .write_distributor(GICD_IPRIORITYR10, AccessWidth::Dword, 0x8080_1020)
        .unwrap();
    controller
        .write_distributor(
            GICD_ICFGR2,
            AccessWidth::Dword,
            (1 << ((guest_spi.raw() % 16) * 2 + 1)) | (1 << ((host_spi.raw() % 16) * 2 + 1)),
        )
        .unwrap();
    controller
        .write_distributor(
            GICD_ISPENDR1,
            AccessWidth::Dword,
            (1 << (guest_spi.raw() - 32)) | (1 << (host_spi.raw() - 32)),
        )
        .unwrap();

    let configured = backend
        .records
        .lock()
        .unwrap()
        .configured_interrupts
        .clone();
    assert!(!configured.is_empty());
    assert!(
        configured
            .iter()
            .all(|(binding, _)| *binding == physical_binding)
    );
    assert_eq!(
        configured.last().copied(),
        Some((
            physical_binding,
            PhysicalInterruptConfiguration::new(
                true,
                false,
                Priority::new(0x20),
                TriggerMode::Edge,
            ),
        ))
    );
}

#[test]
fn passthrough_repeated_set_pending_writes_retrigger_physical_spi() {
    const GICD_ISPENDR1: u64 = 0x204;

    let backend = Arc::new(PhysicalBackend::default());
    let controller = GicV3Controller::new(config(), backend.clone()).unwrap();
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let spi = SpiId::new(40).unwrap();
    controller
        .bind_physical_spi(spi, PhysicalIrqId::new(1040), GicVcpuId::new(0))
        .unwrap();
    binding.load().unwrap();
    backend
        .records
        .lock()
        .unwrap()
        .configured_interrupts
        .clear();

    let pending_bit = 1 << (spi.raw() - 32);
    controller
        .write_distributor(GICD_ISPENDR1, AccessWidth::Dword, pending_bit)
        .unwrap();
    controller
        .write_distributor(GICD_ISPENDR1, AccessWidth::Dword, pending_bit)
        .unwrap();

    let configured = backend
        .records
        .lock()
        .unwrap()
        .configured_interrupts
        .clone();
    assert_eq!(configured.len(), 2);
    assert!(configured.iter().all(|(_, state)| state.pending()));
}

fn config() -> GicV3Config {
    let guest_timer = PrivateInterruptMask::SGIS
        .with(IntId::Ppi(PpiId::new(27).unwrap()))
        .unwrap();
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
    .with_passthrough_private_interrupts(guest_timer)
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
    bound_interrupts: Vec<PhysicalInterruptBinding>,
    enabled_interrupts: Vec<(PhysicalInterruptBinding, bool)>,
    configured_interrupts: Vec<(PhysicalInterruptBinding, PhysicalInterruptConfiguration)>,
    levels: Vec<(PhysicalInterruptBinding, bool)>,
    pulses: Vec<PhysicalInterruptBinding>,
    bound_msi: Vec<PhysicalMsiBinding>,
    signaled_msi: Vec<PhysicalMsiBinding>,
    sgis: Vec<(GicVcpuId, SgiId, Vec<GicAffinity>)>,
    unbound_interrupts: Vec<PhysicalInterruptBinding>,
    unbound_msi: Vec<PhysicalMsiBinding>,
    private_loads: Vec<(GicVcpuId, PrivateInterruptMask, PrivateInterruptState)>,
    private_saves: Vec<(GicVcpuId, PrivateInterruptMask, PrivateInterruptState)>,
    private_synchronizations: Vec<(GicVcpuId, PrivateInterruptMask, PrivateInterruptState)>,
    fail_next_enable: bool,
}

impl GicV3Backend for PhysicalBackend {
    fn load_cpu_interface(
        &self,
        _vcpu: GicVcpuId,
        _state: &CpuInterfaceState,
    ) -> Result<(), GicV3BackendError> {
        self.records.lock().unwrap().cpu_interface_loads += 1;
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

    fn load_physical_private_interrupts(
        &self,
        vcpu: GicVcpuId,
        owned: PrivateInterruptMask,
        guest: &PrivateInterruptState,
    ) -> Result<PrivateInterruptState, GicV3BackendError> {
        self.records
            .lock()
            .unwrap()
            .private_loads
            .push((vcpu, owned, guest.clone()));
        let mut host = PrivateInterruptState::new();
        host.set_enabled(IntId::Ppi(PpiId::new(26).unwrap()), true)
            .unwrap();
        Ok(host)
    }

    fn save_physical_private_interrupts(
        &self,
        vcpu: GicVcpuId,
        owned: PrivateInterruptMask,
        guest: &mut PrivateInterruptState,
        _host: &PrivateInterruptState,
    ) -> Result<(), GicV3BackendError> {
        guest
            .set_pending(IntId::Ppi(PpiId::new(27).unwrap()), true)
            .unwrap();
        self.records
            .lock()
            .unwrap()
            .private_saves
            .push((vcpu, owned, guest.clone()));
        Ok(())
    }

    fn synchronize_physical_private_interrupts(
        &self,
        vcpu: GicVcpuId,
        owned: PrivateInterruptMask,
        guest: &mut PrivateInterruptState,
    ) -> Result<(), GicV3BackendError> {
        self.records
            .lock()
            .unwrap()
            .private_synchronizations
            .push((vcpu, owned, guest.clone()));
        Ok(())
    }

    fn update_physical_private_interrupts(
        &self,
        _vcpu: GicVcpuId,
        _owned: PrivateInterruptMask,
        _guest: &PrivateInterruptState,
    ) -> Result<(), GicV3BackendError> {
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

    fn configure_physical_interrupt(
        &self,
        binding: PhysicalInterruptBinding,
        configuration: PhysicalInterruptConfiguration,
    ) -> Result<(), GicV3BackendError> {
        self.records
            .lock()
            .unwrap()
            .configured_interrupts
            .push((binding, configuration));
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

    fn send_physical_sgi(
        &self,
        source: GicVcpuId,
        sgi: SgiId,
        targets: &[GicAffinity],
    ) -> Result<(), GicV3BackendError> {
        self.records
            .lock()
            .unwrap()
            .sgis
            .push((source, sgi, targets.to_vec()));
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
