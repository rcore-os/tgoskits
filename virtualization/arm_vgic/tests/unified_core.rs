use std::sync::{Arc, Mutex};

use arm_vgic::{
    ArmVgicConfig, AssignedSpiConfig, CpuInterfaceState, GicAffinity, GicV3BackendError,
    GicV3VcpuWake, GicVcpuId, GuestMemory, GuestMemoryError, HostGicVersion, IntId, ItsConfig,
    PhysicalInterruptBinding, SoftwareGicV3Backend, SpiId, VgicBackend, VgicBackendCapabilities,
    VgicCore, VgicMmioRegion, VgicResult, VgicV2Config, VgicV3Config,
};
use axdevice_base::{
    ControllerInputId, HostIrqId, InterruptControllerId, InterruptTrigger, ItsId, LpiId,
    MessageInterruptController, MsiDeviceId, MsiEventId, VirtualInterruptController,
};

struct V2Backend;

impl VgicBackend for V2Backend {
    fn capabilities(&self) -> VgicBackendCapabilities {
        VgicBackendCapabilities::new(HostGicVersion::V2, 4, 5, false)
    }

    fn load_cpu_interface(
        &self,
        _vcpu: GicVcpuId,
        _state: &CpuInterfaceState,
    ) -> Result<(), GicV3BackendError> {
        Ok(())
    }

    fn save_cpu_interface(
        &self,
        _vcpu: GicVcpuId,
        _state: &mut CpuInterfaceState,
    ) -> Result<(), GicV3BackendError> {
        Ok(())
    }
}

fn region(base: u64, size: u64) -> VgicMmioRegion {
    VgicMmioRegion::new(base, size).unwrap()
}

#[test]
fn v2_and_v3_require_a_matching_host_backend_version() {
    let v2 = ArmVgicConfig::V2(
        VgicV2Config::new(
            InterruptControllerId::new(0),
            region(0x0800_0000, 0x1_0000),
            region(0x0801_0000, 0x2_000),
            vec![arm_vgic::GicAffinity::new(0, 0, 0, 0)],
        )
        .unwrap(),
    );
    assert!(VgicCore::new(v2.clone(), Arc::new(V2Backend)).is_ok());
    assert!(VgicCore::new(v2, Arc::new(SoftwareGicV3Backend)).is_err());

    let v3 = ArmVgicConfig::V3(
        VgicV3Config::new(
            InterruptControllerId::new(0),
            region(0x0800_0000, 0x1_0000),
            vec![region(0x080a_0000, 0x2_0000)],
            0x2_0000,
            vec![arm_vgic::GicAffinity::new(0, 0, 0, 0)],
        )
        .unwrap(),
    );
    assert!(VgicCore::new(v3.clone(), Arc::new(SoftwareGicV3Backend)).is_ok());
    assert!(VgicCore::new(v3, Arc::new(V2Backend)).is_err());
}

#[test]
fn v2_accepts_distributor_not_aligned_for_gicv3() {
    let config = ArmVgicConfig::V2(
        VgicV2Config::new(
            InterruptControllerId::new(0),
            region(0x2a70_1000, 0x1_0000),
            region(0x2a70_6000, 0x1_0000),
            vec![GicAffinity::new(0, 0, 1, 2)],
        )
        .unwrap(),
    );

    let core = VgicCore::new(config, Arc::new(V2Backend)).unwrap();
    assert!(core.controller().gicv3_config().is_none());
}

#[test]
fn the_dyn_controller_line_updates_the_same_canonical_state() {
    let config = ArmVgicConfig::V3(
        VgicV3Config::new(
            InterruptControllerId::new(9),
            region(0x0800_0000, 0x1_0000),
            vec![region(0x080a_0000, 0x2_0000)],
            0x2_0000,
            vec![arm_vgic::GicAffinity::new(0, 0, 0, 0)],
        )
        .unwrap(),
    );
    let core = Arc::new(VgicCore::new(config, Arc::new(SoftwareGicV3Backend)).unwrap());
    let dyn_controller: Arc<dyn VirtualInterruptController> = core.clone();
    let line = dyn_controller
        .wired_input(ControllerInputId::new(32), InterruptTrigger::EdgeTriggered)
        .unwrap()
        .connect()
        .unwrap();

    line.pulse().unwrap();
    assert_eq!(
        core.controller()
            .interrupt_state(None, IntId::new(32).unwrap())
            .unwrap(),
        arm_vgic::InterruptState::Pending
    );
}

#[test]
fn physical_spi_configuration_requires_identity_mapping() {
    assert!(
        arm_vgic::AssignedSpiConfig::new(
            arm_vgic::SpiId::new(40).unwrap(),
            HostIrqId::new(41),
            0,
            InterruptTrigger::LevelTriggered,
        )
        .is_err()
    );
}

#[test]
fn assigned_spi_binding_rolls_back_before_a_retry() {
    let backend = Arc::new(FailSecondPhysicalBind::default());
    let assigned = [40, 41]
        .map(|intid| {
            AssignedSpiConfig::new(
                SpiId::new(intid).unwrap(),
                HostIrqId::new(intid as usize),
                0,
                InterruptTrigger::LevelTriggered,
            )
            .unwrap()
        })
        .to_vec();
    let config = VgicV3Config::new(
        InterruptControllerId::new(0),
        region(0x0800_0000, 0x1_0000),
        vec![region(0x080a_0000, 0x2_0000)],
        0x2_0000,
        vec![GicAffinity::new(0, 0, 0, 0)],
    )
    .unwrap()
    .with_assigned_spis(assigned)
    .unwrap();
    let core = VgicCore::new(ArmVgicConfig::V3(config), backend.clone()).unwrap();
    let _binding = core.attach_vcpu(0, Arc::new(NoopWake)).unwrap();

    assert!(core.bind_assigned_spis().is_err());
    core.bind_assigned_spis().unwrap();

    let records = backend.records.lock().unwrap();
    assert_eq!(records.bound, vec![40, 41, 40, 41]);
    assert_eq!(records.unbound, vec![40]);
}

struct ZeroMemory;

impl GuestMemory for ZeroMemory {
    fn read(&self, _address: u64, destination: &mut [u8]) -> Result<(), GuestMemoryError> {
        destination.fill(0);
        Ok(())
    }
}

struct NoopWake;

impl GicV3VcpuWake for NoopWake {
    fn wake(&self) -> VgicResult {
        Ok(())
    }
}

#[derive(Default)]
struct FailSecondPhysicalBind {
    records: Mutex<PhysicalBindRecords>,
}

#[derive(Default)]
struct PhysicalBindRecords {
    bound: Vec<u32>,
    unbound: Vec<u32>,
    failed_once: bool,
}

impl VgicBackend for FailSecondPhysicalBind {
    fn load_cpu_interface(
        &self,
        _vcpu: GicVcpuId,
        _state: &CpuInterfaceState,
    ) -> Result<(), GicV3BackendError> {
        Ok(())
    }

    fn save_cpu_interface(
        &self,
        _vcpu: GicVcpuId,
        _state: &mut CpuInterfaceState,
    ) -> Result<(), GicV3BackendError> {
        Ok(())
    }

    fn bind_physical_interrupt(
        &self,
        binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        let mut records = self.records.lock().unwrap();
        records.bound.push(binding.guest().raw());
        if binding.guest().raw() == 41 && !records.failed_once {
            records.failed_once = true;
            return Err(GicV3BackendError::new(
                "bind physical interrupt",
                "injected second binding failure",
            ));
        }
        Ok(())
    }

    fn unbind_physical_interrupt(
        &self,
        binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        self.records
            .lock()
            .unwrap()
            .unbound
            .push(binding.guest().raw());
        Ok(())
    }
}

#[test]
fn optional_dyn_msi_capability_is_present_only_when_configured() {
    let controller_id = InterruptControllerId::new(11);
    let base = VgicV3Config::new(
        controller_id,
        region(0x0800_0000, 0x1_0000),
        vec![region(0x080a_0000, 0x2_0000)],
        0x2_0000,
        vec![arm_vgic::GicAffinity::new(0, 0, 0, 0)],
    )
    .unwrap();
    let without_its = Arc::new(
        VgicCore::new(
            ArmVgicConfig::V3(base.clone()),
            Arc::new(SoftwareGicV3Backend),
        )
        .unwrap(),
    );
    let message: Arc<dyn MessageInterruptController> = without_its;
    assert!(
        message
            .msi_endpoint(
                ItsId::new(0),
                MsiDeviceId::new(1),
                MsiEventId::new(2),
                LpiId::new(8192),
            )
            .is_err()
    );

    let configured = base
        .with_its(vec![ItsConfig::new(
            ItsId::new(3),
            region(0x0808_0000, 0x1_0000),
        )])
        .unwrap();
    let core = Arc::new(
        VgicCore::new_with_guest_memory(
            ArmVgicConfig::V3(configured),
            Arc::new(SoftwareGicV3Backend),
            Some(Arc::new(ZeroMemory)),
        )
        .unwrap(),
    );
    let message: Arc<dyn MessageInterruptController> = core;
    let endpoint = message
        .msi_endpoint(
            ItsId::new(3),
            MsiDeviceId::new(1),
            MsiEventId::new(2),
            LpiId::new(8192),
        )
        .unwrap();
    assert_eq!(endpoint.message().its(), ItsId::new(3));
    assert_eq!(endpoint.message().lpi(), LpiId::new(8192));
}
