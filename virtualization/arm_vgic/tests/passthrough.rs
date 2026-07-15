use std::sync::{Arc, Mutex};

use arm_vgic::{
    CpuInterfaceState, EventId, GicAffinity, GicV3Backend, GicV3BackendError, GicV3Config,
    GicV3Controller, GicV3MmioRegion, GicV3Mode, GicV3VcpuWake, GicVcpuId, ItsDeviceId, LpiId,
    PhysicalInterruptBinding, PhysicalIrqId, PhysicalMsiBinding, SgiId, SgiTarget, SpiId,
    TriggerMode, VgicError, VgicResult,
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
    controller
        .send_sgi(
            GicVcpuId::new(0),
            SgiId::new(2).unwrap(),
            SgiTarget::Affinities(vec![GicAffinity::new(0, 0, 0, 1)]),
        )
        .unwrap();

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
    bound_interrupts: Vec<PhysicalInterruptBinding>,
    levels: Vec<(PhysicalInterruptBinding, bool)>,
    pulses: Vec<PhysicalInterruptBinding>,
    bound_msi: Vec<PhysicalMsiBinding>,
    signaled_msi: Vec<PhysicalMsiBinding>,
    sgis: Vec<(GicVcpuId, SgiId, Vec<GicAffinity>)>,
    unbound_interrupts: Vec<PhysicalInterruptBinding>,
    unbound_msi: Vec<PhysicalMsiBinding>,
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

    fn bind_physical_interrupt(
        &self,
        binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        self.records.lock().unwrap().bound_interrupts.push(binding);
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
