use std::{
    collections::BTreeMap,
    sync::{Arc, Barrier, Mutex},
};

use arm_vgic::{
    ArmVgicConfig, CpuInterfaceState, GicAffinity, GicV3BackendError, GicV3VcpuWake, GicVcpuId,
    HostGicVersion, IntId, InterruptState, VgicBackend, VgicBackendCapabilities, VgicCore,
    VgicDeviceSet, VgicMmioRegion, VgicResult, VgicV2Config,
};
use axdevice_base::{
    BusKind, DeviceAccess, DeviceId, DeviceVcpuId, InterruptControllerId, InterruptTrigger,
    NoopDeviceContext,
};
use axvm_types::AccessWidth;

mod support;

const GICD_CTLR: u64 = 0x0000;
const GICD_ISENABLER: u64 = 0x0100;
const GICD_ICENABLER: u64 = 0x0180;
const GICD_ISPENDR: u64 = 0x0200;
const GICD_IPRIORITYR: u64 = 0x0400;
const GICD_ITARGETSR: u64 = 0x0800;
const GICD_CPENDSGIR: u64 = 0x0f10;
const GICD_SPENDSGIR: u64 = 0x0f20;
const GICC_CTLR: u64 = 0x0000;
const GICC_PMR: u64 = 0x0004;
const GICC_BPR: u64 = 0x0008;
const GICC_IAR: u64 = 0x000c;
const GICC_EOIR: u64 = 0x0010;
const GICC_RPR: u64 = 0x0014;
const GICC_HPPIR: u64 = 0x0018;
const GICC_DIR: u64 = 0x1000;

/// GIC INTID reported for an acknowledge that no delivery may satisfy.
const GIC_SPURIOUS_INTID: u64 = 1023;
/// Idle running priority of the GICv2 CPU interface.
const GIC_IDLE_RUNNING_PRIORITY: u64 = 0xff;

#[derive(Default)]
struct TestBackend {
    interfaces: Mutex<BTreeMap<GicVcpuId, CpuInterfaceState>>,
    retired: Mutex<Vec<(GicVcpuId, IntId)>>,
}

impl TestBackend {
    fn loaded_intids(&self, vcpu: usize) -> Vec<IntId> {
        self.interfaces
            .lock()
            .unwrap()
            .get(&GicVcpuId::new(vcpu))
            .map(|state| {
                state
                    .list_registers()
                    .iter()
                    .flatten()
                    .map(|entry| entry.intid())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn loaded_states(&self, vcpu: GicVcpuId) -> Vec<InterruptState> {
        self.interfaces.lock().unwrap()[&vcpu]
            .list_registers()
            .iter()
            .flatten()
            .map(|entry| entry.state())
            .collect()
    }

    fn simulate_guest_state(&self, vcpu: GicVcpuId, state: InterruptState) {
        let mut interfaces = self.interfaces.lock().unwrap();
        for entry in interfaces
            .get_mut(&vcpu)
            .unwrap()
            .list_registers_mut()
            .iter_mut()
            .flatten()
        {
            entry.set_state(state);
        }
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
        self.interfaces.lock().unwrap().insert(vcpu, state.clone());
        Ok(())
    }

    fn save_cpu_interface(
        &self,
        vcpu: GicVcpuId,
        state: &mut CpuInterfaceState,
    ) -> Result<(), GicV3BackendError> {
        let interfaces = self.interfaces.lock().unwrap();
        let observed = &interfaces[&vcpu];
        state
            .list_registers_mut()
            .copy_from_slice(observed.list_registers());
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
fn v2_mmio_cpu_interface_uses_explicit_accessor_without_current_vcpu() {
    let (core, _) = core();
    let _vcpu0 = core.attach_vcpu(0, Arc::new(Wake)).unwrap();
    let _vcpu1 = core.attach_vcpu(1, Arc::new(Wake)).unwrap();
    let devices = VgicDeviceSet::new(Arc::new(core)).unwrap();
    let cpu_interface = &devices.devices()[1];

    let write_pmr = |vcpu_id, value| {
        let mut context = NoopDeviceContext::new(DeviceId::new(0));
        cpu_interface
            .write(
                &DeviceAccess::new(
                    DeviceVcpuId::new(vcpu_id),
                    BusKind::Mmio,
                    0x0801_0000 + GICC_PMR,
                    AccessWidth::Dword,
                ),
                value,
                &mut context,
            )
            .unwrap();
    };
    let read_pmr = |vcpu_id| {
        let mut context = NoopDeviceContext::new(DeviceId::new(0));
        cpu_interface
            .read(
                &DeviceAccess::new(
                    DeviceVcpuId::new(vcpu_id),
                    BusKind::Mmio,
                    0x0801_0000 + GICC_PMR,
                    AccessWidth::Dword,
                ),
                &mut context,
            )
            .unwrap()
    };

    write_pmr(0, 0x11);
    write_pmr(1, 0x22);
    assert_eq!(read_pmr(0), 0x11);
    assert_eq!(read_pmr(1), 0x22);
}

#[test]
fn v2_mmio_cpu_interface_keeps_banked_state_isolated_across_host_threads() {
    let (core, _) = core();
    let _vcpu0 = core.attach_vcpu(0, Arc::new(Wake)).unwrap();
    let _vcpu1 = core.attach_vcpu(1, Arc::new(Wake)).unwrap();
    let devices = VgicDeviceSet::new(Arc::new(core)).unwrap();
    let cpu_interface = devices.devices()[1].clone();
    let barrier = Arc::new(Barrier::new(2));

    let workers = [(0, 0x11), (1, 0x22)].map(|(vcpu_id, value)| {
        let cpu_interface = cpu_interface.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            for _ in 0..256 {
                let mut context = NoopDeviceContext::new(DeviceId::new(0));
                cpu_interface
                    .write(
                        &DeviceAccess::new(
                            DeviceVcpuId::new(vcpu_id),
                            BusKind::Mmio,
                            0x0801_0000 + GICC_PMR,
                            AccessWidth::Dword,
                        ),
                        value,
                        &mut context,
                    )
                    .unwrap();
                assert_eq!(
                    cpu_interface
                        .read(
                            &DeviceAccess::new(
                                DeviceVcpuId::new(vcpu_id),
                                BusKind::Mmio,
                                0x0801_0000 + GICC_PMR,
                                AccessWidth::Dword,
                            ),
                            &mut context,
                        )
                        .unwrap(),
                    value
                );
            }
        })
    });

    for worker in workers {
        worker.join().unwrap();
    }
}

#[test]
fn v2_mmio_distributor_uses_explicit_accessor_for_banked_state() {
    let (core, _) = core();
    let _vcpu0 = core.attach_vcpu(0, Arc::new(Wake)).unwrap();
    let _vcpu1 = core.attach_vcpu(1, Arc::new(Wake)).unwrap();
    let devices = VgicDeviceSet::new(Arc::new(core)).unwrap();
    let distributor = &devices.devices()[0];

    let write_enabled = |vcpu_id, bit| {
        let mut context = NoopDeviceContext::new(DeviceId::new(0));
        distributor
            .write(
                &DeviceAccess::new(
                    DeviceVcpuId::new(vcpu_id),
                    BusKind::Mmio,
                    0x0800_0000 + GICD_ISENABLER,
                    AccessWidth::Dword,
                ),
                1u64 << bit,
                &mut context,
            )
            .unwrap();
    };
    let read_enabled = |vcpu_id| {
        let mut context = NoopDeviceContext::new(DeviceId::new(0));
        distributor
            .read(
                &DeviceAccess::new(
                    DeviceVcpuId::new(vcpu_id),
                    BusKind::Mmio,
                    0x0800_0000 + GICD_ISENABLER,
                    AccessWidth::Dword,
                ),
                &mut context,
            )
            .unwrap()
    };

    write_enabled(0, 27);
    write_enabled(1, 30);
    assert_eq!(read_enabled(0) & ((1 << 27) | (1 << 30)), 1 << 27);
    assert_eq!(read_enabled(1) & ((1 << 27) | (1 << 30)), 1 << 30);
}

#[test]
fn v2_distributor_clear_enable_and_cpu_target_share_canonical_state() {
    let (core, backend) = core();
    let core = Arc::new(core);
    let vcpu0 = core.attach_vcpu(0, Arc::new(Wake)).unwrap();
    let vcpu1 = core.attach_vcpu(1, Arc::new(Wake)).unwrap();
    let devices = VgicDeviceSet::new(core.clone()).unwrap();
    let distributor = &devices.devices()[0];
    let spi = 40u32;
    let write_distributor = |address, width, value| {
        let mut context = NoopDeviceContext::new(DeviceId::new(0));
        distributor
            .write(
                &DeviceAccess::new(DeviceVcpuId::new(0), BusKind::Mmio, address, width),
                value,
                &mut context,
            )
            .unwrap();
    };

    write_distributor(0x0800_0000 + GICD_CTLR, AccessWidth::Dword, 1);
    write_distributor(
        0x0800_0000 + GICD_ITARGETSR + u64::from(spi),
        AccessWidth::Byte,
        0b10,
    );
    write_distributor(
        0x0800_0000 + GICD_ISENABLER + 4,
        AccessWidth::Dword,
        1 << (spi - 32),
    );
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
    write_distributor(
        0x0800_0000 + GICD_ICENABLER + 4,
        AccessWidth::Dword,
        1 << (spi - 32),
    );
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
fn v2_clearing_loaded_sgi_sources_withdraws_pending_but_preserves_activation() {
    for hardware_state in [
        InterruptState::Pending,
        InterruptState::Active,
        InterruptState::ActivePending,
    ] {
        let (core, backend) = core();
        let binding = core.attach_vcpu(0, Arc::new(Wake)).unwrap();
        let _source = core.attach_vcpu(1, Arc::new(Wake)).unwrap();
        let vcpu = GicVcpuId::new(0);
        let sgi = 3u32;
        let intid = IntId::new(sgi).unwrap();

        core.write_v2_distributor(vcpu, GICD_CTLR, AccessWidth::Dword, 1)
            .unwrap();
        core.write_v2_cpu_interface(vcpu, GICC_PMR, AccessWidth::Dword, 0xff)
            .unwrap();
        core.write_v2_cpu_interface(vcpu, GICC_CTLR, AccessWidth::Dword, 1)
            .unwrap();
        core.write_v2_distributor(
            vcpu,
            GICD_SPENDSGIR + u64::from(sgi),
            AccessWidth::Byte,
            0b10,
        )
        .unwrap();
        binding.load().unwrap();
        assert_eq!(backend.loaded_intids(0), vec![intid]);
        backend.simulate_guest_state(vcpu, hardware_state);

        core.write_v2_distributor(
            vcpu,
            GICD_CPENDSGIR + u64::from(sgi),
            AccessWidth::Byte,
            0b10,
        )
        .unwrap();
        binding.save().unwrap();
        binding.load().unwrap();
        let expected = if hardware_state == InterruptState::Pending {
            vec![]
        } else {
            vec![InterruptState::Active]
        };
        assert_eq!(
            backend.loaded_states(vcpu),
            expected,
            "CPENDSGIR must remove pending without losing a guest activation ({hardware_state:?})"
        );
        binding.save().unwrap();

        if hardware_state != InterruptState::Pending {
            binding.deactivate_saved(intid).unwrap();
            binding.load().unwrap();
            assert!(backend.loaded_intids(0).is_empty());
            binding.save().unwrap();
        }
        assert_eq!(
            core.controller()
                .interrupt_state(Some(vcpu), intid)
                .unwrap(),
            InterruptState::Inactive
        );
    }
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

/// Enables one guest-owned SPI on `vcpu` with a fixed priority through the
/// public GICv2 Distributor registers.
fn configure_v2_spi(core: &VgicCore, vcpu: GicVcpuId, spi: u32, priority: u8) {
    core.write_v2_distributor(vcpu, GICD_CTLR, AccessWidth::Dword, 1)
        .unwrap();
    core.write_v2_distributor(
        vcpu,
        GICD_ITARGETSR + u64::from(spi),
        AccessWidth::Byte,
        0b01,
    )
    .unwrap();
    core.write_v2_distributor(
        vcpu,
        GICD_IPRIORITYR + u64::from(spi),
        AccessWidth::Byte,
        u64::from(priority),
    )
    .unwrap();
    core.write_v2_distributor(
        vcpu,
        GICD_ISENABLER + u64::from(spi / 32 * 4),
        AccessWidth::Dword,
        1u64 << (spi % 32),
    )
    .unwrap();
}

/// Enables the GICv2 CPU interface with an explicit binary point and PMR.
fn enable_v2_cpu_interface(core: &VgicCore, vcpu: GicVcpuId, binary_point: u8, priority_mask: u8) {
    core.write_v2_cpu_interface(vcpu, GICC_PMR, AccessWidth::Dword, u64::from(priority_mask))
        .unwrap();
    core.write_v2_cpu_interface(vcpu, GICC_BPR, AccessWidth::Dword, u64::from(binary_point))
        .unwrap();
    core.write_v2_cpu_interface(vcpu, GICC_CTLR, AccessWidth::Dword, 1)
        .unwrap();
}

/// Latches one SPI pending through the public GICv2 Distributor register.
fn pend_v2_spi(core: &VgicCore, vcpu: GicVcpuId, spi: u32) {
    core.write_v2_distributor(
        vcpu,
        GICD_ISPENDR + u64::from(spi / 32 * 4),
        AccessWidth::Dword,
        1u64 << (spi % 32),
    )
    .unwrap();
}

fn v2_iar(core: &VgicCore, vcpu: GicVcpuId) -> u64 {
    core.read_v2_cpu_interface(vcpu, GICC_IAR, AccessWidth::Dword)
        .unwrap()
}

fn v2_rpr(core: &VgicCore, vcpu: GicVcpuId) -> u64 {
    core.read_v2_cpu_interface(vcpu, GICC_RPR, AccessWidth::Dword)
        .unwrap()
}

fn v2_eoi(core: &VgicCore, vcpu: GicVcpuId, iar: u64) {
    core.write_v2_cpu_interface(vcpu, GICC_EOIR, AccessWidth::Dword, iar)
        .unwrap();
}

/// A pending delivery that does not strictly raise the group priority must not
/// be acknowledged while a higher-priority delivery is active. It stays
/// pending and becomes acknowledgeable again once the active one is EOIed.
#[test]
fn v2_acknowledge_requires_a_strictly_higher_group_priority() {
    let (core, _) = core();
    let _binding = core.attach_vcpu(0, Arc::new(Wake)).unwrap();
    let vcpu = GicVcpuId::new(0);
    let active = 40u32;
    let equal = 41u32;
    let lower = 42u32;

    configure_v2_spi(&core, vcpu, active, 0x40);
    configure_v2_spi(&core, vcpu, equal, 0x40);
    configure_v2_spi(&core, vcpu, lower, 0x60);
    // Binary point 0 makes the whole byte the group priority, so this test
    // does not depend on the subpriority split.
    enable_v2_cpu_interface(&core, vcpu, 0, 0xff);

    pend_v2_spi(&core, vcpu, active);
    let iar = v2_iar(&core, vcpu);
    assert_eq!(iar & 0x3ff, u64::from(active));
    assert_eq!(v2_rpr(&core, vcpu), 0x40);
    assert_eq!(
        core.controller()
            .interrupt_state(Some(vcpu), IntId::new(active).unwrap())
            .unwrap(),
        InterruptState::Active
    );

    // An equal group priority cannot preempt the running delivery.
    pend_v2_spi(&core, vcpu, equal);
    assert_eq!(v2_iar(&core, vcpu), GIC_SPURIOUS_INTID);
    assert_eq!(v2_rpr(&core, vcpu), 0x40);
    assert_eq!(
        core.controller()
            .interrupt_state(Some(vcpu), IntId::new(equal).unwrap())
            .unwrap(),
        InterruptState::Pending
    );

    // A strictly lower group priority cannot preempt either, even though the
    // PMR allows it, and the higher-priority pending delivery is still the one
    // reported to the guest.
    pend_v2_spi(&core, vcpu, lower);
    assert_eq!(v2_iar(&core, vcpu), GIC_SPURIOUS_INTID);
    assert_eq!(
        core.read_v2_cpu_interface(vcpu, GICC_HPPIR, AccessWidth::Dword)
            .unwrap(),
        u64::from(equal)
    );
    assert_eq!(
        core.controller()
            .interrupt_state(Some(vcpu), IntId::new(lower).unwrap())
            .unwrap(),
        InterruptState::Pending
    );

    // EOIing the active delivery re-opens acknowledgement for the pending
    // higher-priority one, then the remaining lower-priority one.
    v2_eoi(&core, vcpu, iar);
    assert_eq!(v2_rpr(&core, vcpu), GIC_IDLE_RUNNING_PRIORITY);
    assert_eq!(
        core.controller()
            .interrupt_state(Some(vcpu), IntId::new(active).unwrap())
            .unwrap(),
        InterruptState::Inactive
    );
    let equal_iar = v2_iar(&core, vcpu);
    assert_eq!(equal_iar & 0x3ff, u64::from(equal));
    assert_eq!(v2_rpr(&core, vcpu), 0x40);
    v2_eoi(&core, vcpu, equal_iar);
    let lower_iar = v2_iar(&core, vcpu);
    assert_eq!(lower_iar & 0x3ff, u64::from(lower));
    assert_eq!(v2_rpr(&core, vcpu), 0x60);
    v2_eoi(&core, vcpu, lower_iar);
    assert_eq!(v2_rpr(&core, vcpu), GIC_IDLE_RUNNING_PRIORITY);
}

/// A strictly higher group priority nests on top of the running delivery and
/// unwinds in order as each layer is EOIed.
#[test]
fn v2_acknowledge_nests_a_strictly_higher_group_priority() {
    let (core, _) = core();
    let _binding = core.attach_vcpu(0, Arc::new(Wake)).unwrap();
    let vcpu = GicVcpuId::new(0);
    let outer = 44u32;
    let inner = 45u32;

    configure_v2_spi(&core, vcpu, outer, 0x40);
    configure_v2_spi(&core, vcpu, inner, 0x20);
    enable_v2_cpu_interface(&core, vcpu, 0, 0xff);

    pend_v2_spi(&core, vcpu, outer);
    let outer_iar = v2_iar(&core, vcpu);
    assert_eq!(outer_iar & 0x3ff, u64::from(outer));
    assert_eq!(v2_rpr(&core, vcpu), 0x40);

    pend_v2_spi(&core, vcpu, inner);
    let inner_iar = v2_iar(&core, vcpu);
    assert_eq!(inner_iar & 0x3ff, u64::from(inner));
    assert_eq!(v2_rpr(&core, vcpu), 0x20);

    v2_eoi(&core, vcpu, inner_iar);
    assert_eq!(v2_rpr(&core, vcpu), 0x40);
    assert_eq!(
        core.controller()
            .interrupt_state(Some(vcpu), IntId::new(inner).unwrap())
            .unwrap(),
        InterruptState::Inactive
    );
    v2_eoi(&core, vcpu, outer_iar);
    assert_eq!(v2_rpr(&core, vcpu), GIC_IDLE_RUNNING_PRIORITY);
}

/// The binary point decides how many low priority bits are subpriority, so a
/// candidate that differs only in subpriority cannot preempt. This asserts the
/// ARM split convention: bits [BinaryPoint:0] are subpriority and the
/// remaining high bits are the group priority.
#[test]
fn v2_binary_point_masks_subpriority_bits_from_preemption() {
    let (core, _) = core();
    let _binding = core.attach_vcpu(0, Arc::new(Wake)).unwrap();
    let vcpu = GicVcpuId::new(0);
    let running = 46u32;
    let candidate = 47u32;

    configure_v2_spi(&core, vcpu, running, 0x82);
    configure_v2_spi(&core, vcpu, candidate, 0x80);
    // Binary point 1: the low two bits are subpriority, so 0x80 and 0x82 share the
    // 0x80 group priority and the candidate may not preempt.
    enable_v2_cpu_interface(&core, vcpu, 1, 0xff);

    pend_v2_spi(&core, vcpu, running);
    let running_iar = v2_iar(&core, vcpu);
    assert_eq!(running_iar & 0x3ff, u64::from(running));
    pend_v2_spi(&core, vcpu, candidate);
    assert_eq!(v2_iar(&core, vcpu), GIC_SPURIOUS_INTID);
    assert_eq!(v2_rpr(&core, vcpu), 0x80);
    v2_eoi(&core, vcpu, running_iar);
    let candidate_iar = v2_iar(&core, vcpu);
    assert_eq!(
        candidate_iar & 0x3ff,
        u64::from(candidate),
        "the blocked subpriority candidate must be acknowledgeable once the stack is idle"
    );
    v2_eoi(&core, vcpu, candidate_iar);
    assert_eq!(v2_rpr(&core, vcpu), GIC_IDLE_RUNNING_PRIORITY);

    // With binary point 0 only bit zero is subpriority, so the same pair of
    // priorities does preempt.
    let running2 = 48u32;
    let candidate2 = 49u32;
    configure_v2_spi(&core, vcpu, running2, 0x82);
    configure_v2_spi(&core, vcpu, candidate2, 0x80);
    enable_v2_cpu_interface(&core, vcpu, 0, 0xff);

    pend_v2_spi(&core, vcpu, running2);
    assert_eq!(v2_iar(&core, vcpu) & 0x3ff, u64::from(running2));
    pend_v2_spi(&core, vcpu, candidate2);
    assert_eq!(v2_iar(&core, vcpu) & 0x3ff, u64::from(candidate2));
    assert_eq!(v2_rpr(&core, vcpu), 0x80);
}

/// A real CPU-interface save/load round trip with a non-empty active stack must
/// preserve the nesting order, observed through GICC_RPR and EOI.
#[test]
fn v2_save_load_preserves_a_nonempty_active_priority_stack() {
    let (core, _) = core();
    let binding = core.attach_vcpu(0, Arc::new(Wake)).unwrap();
    let vcpu = GicVcpuId::new(0);
    let outer = 50u32;
    let inner = 51u32;

    configure_v2_spi(&core, vcpu, outer, 0x40);
    configure_v2_spi(&core, vcpu, inner, 0x20);
    enable_v2_cpu_interface(&core, vcpu, 0, 0xff);

    pend_v2_spi(&core, vcpu, outer);
    let outer_iar = v2_iar(&core, vcpu);
    assert_eq!(outer_iar & 0x3ff, u64::from(outer));
    pend_v2_spi(&core, vcpu, inner);
    let inner_iar = v2_iar(&core, vcpu);
    assert_eq!(inner_iar & 0x3ff, u64::from(inner));
    assert_eq!(v2_rpr(&core, vcpu), 0x20);

    // Two active layers are live across a save/load round trip.
    binding.load().unwrap();
    binding.save().unwrap();
    binding.load().unwrap();
    assert_eq!(
        v2_rpr(&core, vcpu),
        0x20,
        "the innermost active priority must stay on top across save/load"
    );

    v2_eoi(&core, vcpu, inner_iar);
    assert_eq!(v2_rpr(&core, vcpu), 0x40);
    v2_eoi(&core, vcpu, outer_iar);
    assert_eq!(v2_rpr(&core, vcpu), GIC_IDLE_RUNNING_PRIORITY);
}
