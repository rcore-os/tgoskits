use std::sync::{Arc, Mutex};

use arm_vgic::{
    ArmVgicConfig, CpuInterfaceState, GicAffinity, GicV3BackendError, GicV3VcpuWake, GicVcpuId,
    HostGicVersion, IntId, InterruptState, VgicBackend, VgicBackendCapabilities, VgicCore,
    VgicMmioRegion, VgicResult, VgicV2Config,
};
use axdevice_base::{InterruptControllerId, InterruptTrigger};
use axvm_types::AccessWidth;

const GICD_CTLR: u64 = 0x0000;
const GICD_ISENABLER: u64 = 0x0100;
const GICD_ICENABLER: u64 = 0x0180;
const GICD_ITARGETSR: u64 = 0x0800;
const GICC_CTLR: u64 = 0x0000;
const GICC_PMR: u64 = 0x0004;
const GICC_IAR: u64 = 0x000c;
const GICC_EOIR: u64 = 0x0010;
const GICC_HPPIR: u64 = 0x0018;
const GICC_DIR: u64 = 0x1000;

#[derive(Default)]
struct TestBackend {
    loaded: Mutex<Vec<(usize, Vec<IntId>)>>,
    retired: Mutex<Vec<(GicVcpuId, IntId)>>,
}

impl TestBackend {
    fn loaded_intids(&self, vcpu: usize) -> Vec<IntId> {
        self.loaded
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find_map(|(loaded_vcpu, intids)| (*loaded_vcpu == vcpu).then(|| intids.clone()))
            .unwrap_or_default()
    }

    fn retired_interrupts(&self) -> Vec<(GicVcpuId, IntId)> {
        self.retired.lock().unwrap().clone()
    }
}

impl VgicBackend for TestBackend {
    fn capabilities(&self) -> VgicBackendCapabilities {
        VgicBackendCapabilities::new(HostGicVersion::V2, 4, 5, false)
    }

    fn load_cpu_interface(
        &self,
        vcpu: GicVcpuId,
        state: &CpuInterfaceState,
    ) -> Result<(), GicV3BackendError> {
        self.loaded.lock().unwrap().push((
            vcpu.raw(),
            state
                .list_registers()
                .iter()
                .flatten()
                .map(|entry| entry.intid())
                .collect(),
        ));
        Ok(())
    }

    fn save_cpu_interface(
        &self,
        _vcpu: GicVcpuId,
        _state: &mut CpuInterfaceState,
    ) -> Result<(), GicV3BackendError> {
        Ok(())
    }

    fn retire_emulated_interrupt(
        &self,
        vcpu: GicVcpuId,
        intid: IntId,
    ) -> Result<(), GicV3BackendError> {
        self.retired.lock().unwrap().push((vcpu, intid));
        Ok(())
    }
}

struct Wake;

impl GicV3VcpuWake for Wake {
    fn wake(&self) -> VgicResult {
        Ok(())
    }
}

fn region(base: u64, size: u64) -> VgicMmioRegion {
    VgicMmioRegion::new(base, size).unwrap()
}

fn core() -> (VgicCore, Arc<TestBackend>) {
    let backend = Arc::new(TestBackend::default());
    let config = VgicV2Config::new(
        InterruptControllerId::new(0),
        region(0x0800_0000, 0x1_0000),
        region(0x0801_0000, 0x2_000),
        vec![GicAffinity::new(0, 0, 0, 0), GicAffinity::new(0, 0, 0, 1)],
    )
    .unwrap()
    .with_spi_count(32)
    .unwrap();
    (
        VgicCore::new(ArmVgicConfig::V2(config), backend.clone()).unwrap(),
        backend,
    )
}

#[test]
fn v2_distributor_clear_enable_and_cpu_target_share_canonical_state() {
    let (core, backend) = core();
    let vcpu0 = core.attach_vcpu(0, Arc::new(Wake)).unwrap();
    let vcpu1 = core.attach_vcpu(1, Arc::new(Wake)).unwrap();
    let spi = 40u32;

    core.write_v2_distributor(GicVcpuId::new(0), GICD_CTLR, AccessWidth::Dword, 1)
        .unwrap();
    core.write_v2_distributor(
        GicVcpuId::new(0),
        GICD_ITARGETSR + u64::from(spi),
        AccessWidth::Byte,
        0b10,
    )
    .unwrap();
    core.write_v2_distributor(
        GicVcpuId::new(0),
        GICD_ISENABLER + 4,
        AccessWidth::Dword,
        1 << (spi - 32),
    )
    .unwrap();
    core.controller()
        .configure_spi_input(
            arm_vgic::SpiId::new(spi).unwrap(),
            arm_vgic::TriggerMode::Edge,
        )
        .unwrap();
    core.controller()
        .pulse_spi(arm_vgic::SpiId::new(spi).unwrap())
        .unwrap();
    vcpu0.load().unwrap();
    vcpu1.load().unwrap();

    assert!(backend.loaded_intids(0).is_empty());
    assert_eq!(backend.loaded_intids(1), vec![IntId::new(spi).unwrap()]);

    vcpu0.save().unwrap();
    vcpu1.save().unwrap();
    core.write_v2_distributor(
        GicVcpuId::new(0),
        GICD_ICENABLER + 4,
        AccessWidth::Dword,
        1 << (spi - 32),
    )
    .unwrap();
    assert_eq!(
        core.read_v2_distributor(GicVcpuId::new(0), GICD_ISENABLER + 4, AccessWidth::Dword,)
            .unwrap()
            & (1 << (spi - 32)),
        0
    );
}

#[test]
fn v2_cpu_interface_acknowledge_eoi_and_dir_obey_eoi_mode() {
    let (core, backend) = core();
    let _binding = core.attach_vcpu(0, Arc::new(Wake)).unwrap();
    let vcpu = GicVcpuId::new(0);
    let ppi = 27u32;

    core.write_v2_distributor(vcpu, GICD_CTLR, AccessWidth::Dword, 1)
        .unwrap();
    core.write_v2_distributor(vcpu, GICD_ISENABLER, AccessWidth::Dword, 1 << ppi)
        .unwrap();
    core.write_v2_cpu_interface(vcpu, GICC_PMR, AccessWidth::Dword, 0xff)
        .unwrap();
    core.write_v2_cpu_interface(vcpu, GICC_CTLR, AccessWidth::Dword, 1)
        .unwrap();
    core.inject(0, ppi, InterruptTrigger::EdgeTriggered)
        .unwrap();
    assert_eq!(
        core.read_v2_cpu_interface(vcpu, GICC_HPPIR, AccessWidth::Dword)
            .unwrap(),
        u64::from(ppi)
    );

    let iar = core
        .read_v2_cpu_interface(vcpu, GICC_IAR, AccessWidth::Dword)
        .unwrap();
    assert_eq!(iar & 0x3ff, u64::from(ppi));
    assert_eq!(
        core.controller()
            .interrupt_state(Some(vcpu), IntId::new(ppi).unwrap())
            .unwrap(),
        InterruptState::Active
    );
    core.write_v2_cpu_interface(vcpu, GICC_EOIR, AccessWidth::Dword, iar)
        .unwrap();
    assert_eq!(
        core.controller()
            .interrupt_state(Some(vcpu), IntId::new(ppi).unwrap())
            .unwrap(),
        InterruptState::Inactive
    );
    assert_eq!(
        backend.retired_interrupts(),
        vec![(vcpu, IntId::new(ppi).unwrap())]
    );

    core.inject(0, ppi, InterruptTrigger::EdgeTriggered)
        .unwrap();
    core.write_v2_cpu_interface(vcpu, GICC_CTLR, AccessWidth::Dword, 1 | (1 << 9))
        .unwrap();
    let iar = core
        .read_v2_cpu_interface(vcpu, GICC_IAR, AccessWidth::Dword)
        .unwrap();
    core.write_v2_cpu_interface(vcpu, GICC_EOIR, AccessWidth::Dword, iar)
        .unwrap();
    assert_eq!(
        core.controller()
            .interrupt_state(Some(vcpu), IntId::new(ppi).unwrap())
            .unwrap(),
        InterruptState::Active
    );
    core.write_v2_cpu_interface(vcpu, GICC_DIR, AccessWidth::Dword, iar)
        .unwrap();
    assert_eq!(
        core.controller()
            .interrupt_state(Some(vcpu), IntId::new(ppi).unwrap())
            .unwrap(),
        InterruptState::Inactive
    );
    assert_eq!(
        backend.retired_interrupts(),
        vec![
            (vcpu, IntId::new(ppi).unwrap()),
            (vcpu, IntId::new(ppi).unwrap())
        ]
    );
}

#[test]
fn v2_trapped_iar_acknowledges_a_pending_list_register() {
    let (core, _) = core();
    let binding = core.attach_vcpu(0, Arc::new(Wake)).unwrap();
    let vcpu = GicVcpuId::new(0);
    let ppi = 30u32;

    core.write_v2_distributor(vcpu, GICD_CTLR, AccessWidth::Dword, 1)
        .unwrap();
    core.write_v2_distributor(vcpu, GICD_ISENABLER, AccessWidth::Dword, 1 << ppi)
        .unwrap();
    core.write_v2_cpu_interface(vcpu, GICC_PMR, AccessWidth::Dword, 0xff)
        .unwrap();
    core.write_v2_cpu_interface(vcpu, GICC_CTLR, AccessWidth::Dword, 1)
        .unwrap();
    core.inject(0, ppi, InterruptTrigger::EdgeTriggered)
        .unwrap();

    binding.load().unwrap();
    binding.save().unwrap();

    let iar = core
        .read_v2_cpu_interface(vcpu, GICC_IAR, AccessWidth::Dword)
        .unwrap();
    assert_eq!(iar & 0x3ff, u64::from(ppi));
    assert_eq!(
        core.controller()
            .interrupt_state(Some(vcpu), IntId::new(ppi).unwrap())
            .unwrap(),
        InterruptState::Active
    );
}

#[test]
fn v2_invalid_intid_access_is_raz_wi_instead_of_panicking() {
    let (core, _) = core();
    let _binding = core.attach_vcpu(0, Arc::new(Wake)).unwrap();

    assert_eq!(
        core.read_v2_distributor(GicVcpuId::new(0), GICD_ISENABLER + 0x7c, AccessWidth::Dword,)
            .unwrap(),
        0
    );
    core.write_v2_distributor(
        GicVcpuId::new(0),
        GICD_ISENABLER + 0x7c,
        AccessWidth::Dword,
        u64::MAX,
    )
    .unwrap();

    let vcpu = GicVcpuId::new(0);
    core.write_v2_cpu_interface(vcpu, GICC_EOIR, AccessWidth::Dword, 1023)
        .unwrap();
    core.write_v2_cpu_interface(vcpu, GICC_DIR, AccessWidth::Dword, 1020)
        .unwrap();
}

#[test]
fn v2_eoi_for_a_non_running_intid_is_write_ignored() {
    let (core, _) = core();
    let _binding = core.attach_vcpu(0, Arc::new(Wake)).unwrap();
    let vcpu = GicVcpuId::new(0);
    let ppi = 27u32;

    core.write_v2_distributor(vcpu, GICD_CTLR, AccessWidth::Dword, 1)
        .unwrap();
    core.write_v2_distributor(vcpu, GICD_ISENABLER, AccessWidth::Dword, 1 << ppi)
        .unwrap();
    core.write_v2_cpu_interface(vcpu, GICC_PMR, AccessWidth::Dword, 0xff)
        .unwrap();
    core.write_v2_cpu_interface(vcpu, GICC_CTLR, AccessWidth::Dword, 1)
        .unwrap();
    core.inject(0, ppi, InterruptTrigger::EdgeTriggered)
        .unwrap();
    let iar = core
        .read_v2_cpu_interface(vcpu, GICC_IAR, AccessWidth::Dword)
        .unwrap();

    core.write_v2_cpu_interface(vcpu, GICC_EOIR, AccessWidth::Dword, ppi as u64 - 1)
        .unwrap();
    assert_eq!(
        core.controller()
            .interrupt_state(Some(vcpu), IntId::new(ppi).unwrap())
            .unwrap(),
        InterruptState::Active
    );
    core.write_v2_cpu_interface(vcpu, GICC_EOIR, AccessWidth::Dword, iar)
        .unwrap();
}
