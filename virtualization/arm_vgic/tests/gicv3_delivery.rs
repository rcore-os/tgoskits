use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use arm_vgic::{
    CpuInterfaceState, GicAffinity, GicV3Backend, GicV3BackendError, GicV3Config, GicV3Controller,
    GicV3HardwareCapabilities, GicV3MmioRegion, GicV3SpiOwnership, GicV3VcpuWake, GicVcpuId, IntId,
    InterruptState, PpiId, SgiId, SgiTarget, SpiId, TriggerMode, VgicError, VgicResult,
};
use axvm_types::AccessWidth;

const GICD_CTLR: u64 = 0x0000;
const GICD_TYPER: u64 = 0x0004;
const GICD_ISENABLER: u64 = 0x0100;
const GICD_IPRIORITYR: u64 = 0x0400;
const GICD_IROUTER: u64 = 0x6000;
const GIC_PIDR2: u64 = 0xffe8;
const GICR_CTLR: u64 = 0x0000;
const GICR_TYPER: u64 = 0x0008;
const GICR_PROPBASER: u64 = 0x0070;
const GICR_PENDBASER: u64 = 0x0078;
const GICR_SGI_BASE: u64 = 0x1_0000;
const ICH_HCR_UIE: u64 = 1 << 1;
const ICH_HCR_LRENPIE: u64 = 1 << 2;
const ICH_HCR_NPIE: u64 = 1 << 3;
const ICH_HCR_TDIR: u64 = 1 << 14;

#[test]
fn physical_distributor_capabilities_are_not_fabricated_for_the_guest() {
    let capabilities = GicV3HardwareCapabilities::from_distributor_typer(0x0f).unwrap();
    let config = GicV3Config::new(
        GicV3SpiOwnership::Explicit,
        GicV3MmioRegion::new(0x0800_0000, 0x1_0000).unwrap(),
        GicV3MmioRegion::new(0x080a_0000, 0x2_0000).unwrap(),
        0x2_0000,
        1,
    )
    .unwrap()
    .with_hardware_capabilities(capabilities)
    .unwrap();
    let controller = GicV3Controller::new(config, Arc::new(TestBackend::default())).unwrap();

    let typer = controller
        .read_distributor(GICD_TYPER, AccessWidth::Dword)
        .unwrap();
    assert_eq!(typer & (1 << 24), 0, "A3V must follow the physical GIC");
    assert_eq!(typer & (1 << 26), 0, "RSS must follow the physical GIC");
    assert_eq!((typer & 0x1f) + 1, 16, "RK3568 exposes 480 SPIs");
}

#[test]
fn checked_mmio_rejects_bad_accesses_and_preserves_raz_wi() {
    let (controller, _) = controller(2, 2);
    let _vcpu0 = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let _vcpu1 = attach(&controller, 1, GicAffinity::new(0, 0, 1, 2));

    assert!(matches!(
        controller.read_distributor(GICD_CTLR, AccessWidth::Qword),
        Err(VgicError::InvalidAccess { .. })
    ));
    assert!(matches!(
        controller.read_distributor(0x1_0000, AccessWidth::Dword),
        Err(VgicError::InvalidAccess { .. })
    ));
    assert_eq!(
        controller
            .read_distributor(0x1000, AccessWidth::Dword)
            .unwrap(),
        0
    );
    let distributor_control = controller
        .read_distributor(GICD_CTLR, AccessWidth::Dword)
        .unwrap();
    assert_ne!(distributor_control & (1 << 4), 0);
    assert_eq!(distributor_control & (1 << 5), 0);
    let distributor_type = controller
        .read_distributor(GICD_TYPER, AccessWidth::Dword)
        .unwrap();
    assert_eq!(((distributor_type >> 19) & 0x1f) + 1, 16);
    assert_ne!(distributor_type & (1 << 24), 0);
    assert_eq!(
        controller
            .read_distributor(GIC_PIDR2, AccessWidth::Dword)
            .unwrap(),
        0x3b
    );

    let first_typer = controller
        .read_redistributor(GicVcpuId::new(0), 0x8, AccessWidth::Qword)
        .unwrap();
    let last_typer = controller
        .read_redistributor(GicVcpuId::new(1), 0x8, AccessWidth::Qword)
        .unwrap();
    assert_eq!(first_typer & (1 << 4), 0);
    assert_ne!(last_typer & (1 << 4), 0);
    assert_eq!(last_typer >> 32, 0x0000_0102);
    assert_eq!(
        controller
            .read_redistributor(GicVcpuId::new(0), GIC_PIDR2, AccessWidth::Dword)
            .unwrap(),
        0x3b
    );
}

#[test]
fn redistributor_lpi_registers_are_raz_wi_without_an_its() {
    let (controller, _) = controller(1, 2);
    let vcpu = GicVcpuId::new(0);
    let _binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));

    controller
        .write_redistributor(vcpu, GICR_CTLR, AccessWidth::Dword, 1)
        .unwrap();
    controller
        .write_redistributor(vcpu, GICR_PROPBASER, AccessWidth::Qword, 0x1234_5000)
        .unwrap();
    controller
        .write_redistributor(vcpu, GICR_PENDBASER, AccessWidth::Qword, 0x5678_9000)
        .unwrap();

    assert_eq!(
        controller
            .read_redistributor(vcpu, GICR_CTLR, AccessWidth::Dword)
            .unwrap(),
        0
    );
    assert_eq!(
        controller
            .read_redistributor(vcpu, GICR_PROPBASER, AccessWidth::Qword)
            .unwrap(),
        0
    );
    assert_eq!(
        controller
            .read_redistributor(vcpu, GICR_PENDBASER, AccessWidth::Qword)
            .unwrap(),
        0
    );
    assert_eq!(
        controller
            .read_redistributor(vcpu, GICR_TYPER, AccessWidth::Qword)
            .unwrap()
            & 1,
        0
    );
}

#[test]
fn spi_route_delivers_to_the_selected_redistributor() {
    let (controller, backend) = controller(2, 2);
    let vcpu0 = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let vcpu1 = attach(&controller, 1, GicAffinity::new(0, 0, 0, 1));
    let spi = SpiId::new(32).unwrap();

    enable_spi(&controller, spi);
    controller
        .write_distributor(
            GICD_IROUTER + u64::from(spi.raw()) * 8,
            AccessWidth::Qword,
            1,
        )
        .unwrap();
    controller
        .configure_spi_input(spi, TriggerMode::Edge)
        .unwrap();
    controller.pulse_spi(spi).unwrap();
    vcpu0.load().unwrap();
    vcpu1.load().unwrap();

    assert!(backend.loaded_intids(0).is_empty());
    assert_eq!(backend.loaded_intids(1), vec![IntId::Spi(spi)]);
}

#[test]
fn rerouting_a_pending_spi_removes_the_old_redistributor_delivery() {
    let (controller, backend) = controller(2, 2);
    let vcpu0 = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let vcpu1 = attach(&controller, 1, GicAffinity::new(0, 0, 0, 1));
    let spi = SpiId::new(32).unwrap();

    enable_spi(&controller, spi);
    controller
        .configure_spi_input(spi, TriggerMode::Edge)
        .unwrap();
    controller.pulse_spi(spi).unwrap();
    controller
        .write_distributor(
            GICD_IROUTER + u64::from(spi.raw()) * 8,
            AccessWidth::Qword,
            1,
        )
        .unwrap();
    vcpu0.load().unwrap();
    vcpu1.load().unwrap();

    assert!(backend.loaded_intids(0).is_empty());
    assert_eq!(backend.loaded_intids(1), vec![IntId::Spi(spi)]);
}

#[test]
fn spi_refill_preserves_the_distributor_priority() {
    let (controller, backend) = controller(1, 1);
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let spi = SpiId::new(32).unwrap();

    controller
        .write_distributor(
            GICD_IPRIORITYR + u64::from(spi.raw()),
            AccessWidth::Byte,
            0x20,
        )
        .unwrap();
    enable_spi(&controller, spi);
    controller
        .configure_spi_input(spi, TriggerMode::Edge)
        .unwrap();
    controller.pulse_spi(spi).unwrap();
    binding.load().unwrap();

    assert_eq!(backend.loaded_priorities(0), vec![0x20]);
}

#[test]
fn software_pending_refill_selects_the_highest_priority_interrupt() {
    let (controller, backend) = controller(1, 1);
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let low_priority = SpiId::new(32).unwrap();
    let high_priority = SpiId::new(33).unwrap();

    for (spi, priority) in [(low_priority, 0xa0), (high_priority, 0x20)] {
        controller
            .write_distributor(
                GICD_IPRIORITYR + u64::from(spi.raw()),
                AccessWidth::Byte,
                priority,
            )
            .unwrap();
        enable_spi(&controller, spi);
        controller
            .configure_spi_input(spi, TriggerMode::Edge)
            .unwrap();
    }
    controller.pulse_spi(low_priority).unwrap();
    controller.pulse_spi(high_priority).unwrap();

    binding.load().unwrap();
    assert_eq!(backend.loaded_intids(0), vec![IntId::Spi(high_priority)]);
}

#[test]
fn sgi_affinity_and_ppi_delivery_are_vcpu_private() {
    let (controller, backend) = controller(2, 2);
    let affinity0 = GicAffinity::new(0, 0, 0, 0);
    let affinity1 = GicAffinity::new(0, 0, 0, 1);
    let vcpu0 = attach(&controller, 0, affinity0);
    let vcpu1 = attach(&controller, 1, affinity1);

    let ppi = PpiId::new(30).unwrap();
    controller
        .write_redistributor(
            GicVcpuId::new(1),
            GICR_SGI_BASE + GICD_ISENABLER,
            AccessWidth::Dword,
            1 << ppi.raw(),
        )
        .unwrap();
    controller
        .set_ppi_level(GicVcpuId::new(1), ppi, true)
        .unwrap();
    controller
        .send_sgi(
            GicVcpuId::new(0),
            SgiId::new(3).unwrap(),
            SgiTarget::Affinities(vec![affinity1]),
        )
        .unwrap();
    vcpu0.load().unwrap();
    vcpu1.load().unwrap();

    assert!(backend.loaded_intids(0).is_empty());
    assert_eq!(
        backend.loaded_intids(1),
        vec![IntId::Ppi(ppi), IntId::Sgi(SgiId::new(3).unwrap())]
    );
}

#[test]
fn lr_exhaustion_queues_and_refills_without_repeating_completed_edges() {
    let (controller, backend) = controller(1, 1);
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let first = SpiId::new(32).unwrap();
    let second = SpiId::new(33).unwrap();
    enable_spi(&controller, first);
    enable_spi(&controller, second);
    controller
        .configure_spi_input(first, TriggerMode::Edge)
        .unwrap();
    controller
        .configure_spi_input(second, TriggerMode::Edge)
        .unwrap();

    controller.pulse_spi(first).unwrap();
    controller.pulse_spi(second).unwrap();
    binding.load().unwrap();
    assert_eq!(backend.loaded_intids(0), vec![IntId::Spi(first)]);
    assert_eq!(
        controller
            .software_pending_count(GicVcpuId::new(0))
            .unwrap(),
        1
    );
    assert_ne!(backend.loaded_hcr(0) & ICH_HCR_UIE, 0);
    assert_ne!(backend.loaded_hcr(0) & ICH_HCR_NPIE, 0);

    backend.complete_all(0);
    binding.synchronize().unwrap();
    assert_eq!(backend.loaded_intids(0), vec![IntId::Spi(second)]);
    assert_eq!(
        controller
            .software_pending_count(GicVcpuId::new(0))
            .unwrap(),
        0
    );
    assert_eq!(backend.loaded_hcr(0) & ICH_HCR_UIE, 0);

    backend.complete_all(0);
    binding.synchronize().unwrap();
    assert!(backend.loaded_intids(0).is_empty());
    assert_eq!(
        controller
            .interrupt_state(None, IntId::Spi(second))
            .unwrap(),
        InterruptState::Inactive
    );
}

#[test]
fn active_lr_spills_for_a_higher_priority_pending_interrupt() {
    let (controller, backend) = controller(1, 1);
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let low_priority = SpiId::new(32).unwrap();
    let high_priority = SpiId::new(33).unwrap();

    for (spi, priority) in [(low_priority, 0xa0), (high_priority, 0x20)] {
        controller
            .write_distributor(
                GICD_IPRIORITYR + u64::from(spi.raw()),
                AccessWidth::Byte,
                priority,
            )
            .unwrap();
        enable_spi(&controller, spi);
        controller
            .configure_spi_input(spi, TriggerMode::Edge)
            .unwrap();
    }

    controller.pulse_spi(low_priority).unwrap();
    binding.load().unwrap();
    backend.activate_all(0);
    binding.save().unwrap();

    controller.pulse_spi(high_priority).unwrap();
    binding.load().unwrap();

    assert_eq!(backend.loaded_intids(0), vec![IntId::Spi(high_priority)]);
    let hcr = backend.loaded_hcr(0);
    assert_ne!(hcr & ICH_HCR_UIE, 0);
    assert_ne!(hcr & ICH_HCR_LRENPIE, 0);
    assert_eq!(hcr & ICH_HCR_NPIE, 0);
    assert_ne!(hcr & ICH_HCR_TDIR, 0);
}

#[test]
fn pending_non_active_interrupt_preempts_an_active_pending_lr() {
    let (controller, backend) = controller(1, 1);
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let repeating = SpiId::new(32).unwrap();
    let fresh = SpiId::new(33).unwrap();

    for spi in [repeating, fresh] {
        enable_spi(&controller, spi);
        controller
            .configure_spi_input(spi, TriggerMode::Edge)
            .unwrap();
    }
    controller.pulse_spi(repeating).unwrap();
    binding.load().unwrap();
    backend.activate_all(0);
    binding.save().unwrap();

    controller.pulse_spi(repeating).unwrap();
    controller.pulse_spi(fresh).unwrap();
    binding.load().unwrap();

    assert_eq!(backend.loaded_intids(0), vec![IntId::Spi(fresh)]);
}

#[test]
fn trapped_dir_harvests_a_hardware_activation_before_deactivation() {
    let (controller, backend) = controller(1, 1);
    let vcpu = GicVcpuId::new(0);
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let timer = PpiId::new(30).unwrap();

    controller
        .write_redistributor(
            vcpu,
            GICR_SGI_BASE + GICD_ISENABLER,
            AccessWidth::Dword,
            1 << timer.raw(),
        )
        .unwrap();
    controller.set_ppi_level(vcpu, timer, true).unwrap();
    binding.load().unwrap();

    // Hardware can activate the LR while the VM-local snapshot still says
    // Pending. A level timer is normally lowered before the guest writes DIR.
    backend.activate_all(0);
    controller.set_ppi_level(vcpu, timer, false).unwrap();
    binding.deactivate(IntId::Ppi(timer)).unwrap();

    assert!(backend.loaded_intids(0).is_empty());
    assert_eq!(
        controller
            .interrupt_state(Some(vcpu), IntId::Ppi(timer))
            .unwrap(),
        InterruptState::Inactive
    );
    assert_eq!(
        backend.retired_interrupts(),
        vec![(vcpu, IntId::Ppi(timer))]
    );
}

#[test]
fn eoi_count_deactivates_an_active_interrupt_outside_the_lrs() {
    let (controller, backend) = controller(1, 1);
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let low_priority = SpiId::new(32).unwrap();
    let high_priority = SpiId::new(33).unwrap();

    for (spi, priority) in [(low_priority, 0xa0), (high_priority, 0x20)] {
        controller
            .write_distributor(
                GICD_IPRIORITYR + u64::from(spi.raw()),
                AccessWidth::Byte,
                priority,
            )
            .unwrap();
        enable_spi(&controller, spi);
        controller
            .configure_spi_input(spi, TriggerMode::Edge)
            .unwrap();
    }

    controller.pulse_spi(low_priority).unwrap();
    binding.load().unwrap();
    backend.activate_all(0);
    binding.save().unwrap();
    controller.pulse_spi(high_priority).unwrap();
    binding.load().unwrap();

    backend.set_eoi_count(0, 1);
    binding.synchronize().unwrap();

    assert_eq!(
        controller
            .interrupt_state(None, IntId::Spi(low_priority))
            .unwrap(),
        InterruptState::Inactive
    );
    assert_eq!(
        backend.retired_interrupts(),
        vec![(GicVcpuId::new(0), IntId::Spi(low_priority))]
    );
}

#[test]
fn lr_lifecycle_preserves_pending_active_and_inactive_states() {
    let (controller, backend) = controller(1, 1);
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let spi = SpiId::new(32).unwrap();
    enable_spi(&controller, spi);
    controller
        .configure_spi_input(spi, TriggerMode::Edge)
        .unwrap();

    controller.pulse_spi(spi).unwrap();
    binding.load().unwrap();
    assert_eq!(
        controller.interrupt_state(None, IntId::Spi(spi)).unwrap(),
        InterruptState::Pending
    );

    backend.activate_all(0);
    binding.save().unwrap();
    assert_eq!(
        controller.interrupt_state(None, IntId::Spi(spi)).unwrap(),
        InterruptState::Active
    );

    backend.complete_all(0);
    binding.synchronize().unwrap();
    assert_eq!(
        controller.interrupt_state(None, IntId::Spi(spi)).unwrap(),
        InterruptState::Inactive
    );
}

#[test]
fn retiring_lr_notifies_the_backend_once() {
    let (controller, backend) = controller(1, 1);
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let spi = SpiId::new(32).unwrap();
    enable_spi(&controller, spi);
    controller
        .configure_spi_input(spi, TriggerMode::Edge)
        .unwrap();

    controller.pulse_spi(spi).unwrap();
    binding.load().unwrap();
    backend.complete_all(0);
    binding.synchronize().unwrap();
    binding.synchronize().unwrap();

    assert_eq!(
        backend.retired_interrupts(),
        vec![(GicVcpuId::new(0), IntId::Spi(spi))]
    );
}

#[test]
fn active_lr_merges_redelivery_into_the_hardware_state() {
    let (controller, backend) = controller(1, 1);
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let spi = SpiId::new(32).unwrap();
    enable_spi(&controller, spi);
    controller
        .configure_spi_input(spi, TriggerMode::Edge)
        .unwrap();

    controller.pulse_spi(spi).unwrap();
    binding.load().unwrap();
    backend.activate_all(0);
    binding.save().unwrap();

    controller.pulse_spi(spi).unwrap();
    binding.synchronize().unwrap();

    assert_eq!(
        backend.loaded_states(0),
        vec![InterruptState::ActivePending]
    );

    backend.complete_all(0);
    binding.synchronize().unwrap();
    assert!(backend.loaded_intids(0).is_empty());
    assert_eq!(
        controller.interrupt_state(None, IntId::Spi(spi)).unwrap(),
        InterruptState::Inactive
    );
}

#[test]
fn inflight_edge_preserves_redelivery_when_hardware_activation_is_not_yet_saved() {
    let (controller, backend) = controller(1, 1);
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let spi = SpiId::new(32).unwrap();
    enable_spi(&controller, spi);
    controller
        .configure_spi_input(spi, TriggerMode::Edge)
        .unwrap();

    controller.pulse_spi(spi).unwrap();
    binding.load().unwrap();
    backend.activate_all(0);

    controller.pulse_spi(spi).unwrap();
    binding.synchronize().unwrap();

    assert_eq!(
        backend.loaded_states(0),
        vec![InterruptState::ActivePending]
    );
}

#[test]
fn active_level_retriggers_only_after_the_current_delivery_completes() {
    let (controller, backend) = controller(1, 1);
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let spi = SpiId::new(32).unwrap();
    enable_spi(&controller, spi);
    controller
        .configure_spi_input(spi, TriggerMode::Level)
        .unwrap();

    controller.set_spi_level(spi, true).unwrap();
    binding.load().unwrap();
    backend.activate_all(0);
    binding.synchronize().unwrap();

    assert_eq!(backend.loaded_states(0), vec![InterruptState::Active]);

    backend.complete_all(0);
    binding.synchronize().unwrap();
    assert_eq!(backend.loaded_states(0), vec![InterruptState::Pending]);
}

#[test]
fn asserted_level_retriggers_after_completion_until_deasserted() {
    let (controller, backend) = controller(1, 1);
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));
    let spi = SpiId::new(32).unwrap();
    enable_spi(&controller, spi);
    controller
        .configure_spi_input(spi, TriggerMode::Level)
        .unwrap();

    controller.set_spi_level(spi, true).unwrap();
    binding.load().unwrap();
    backend.complete_all(0);
    binding.synchronize().unwrap();
    assert_eq!(backend.loaded_intids(0), vec![IntId::Spi(spi)]);

    controller.set_spi_level(spi, false).unwrap();
    backend.complete_all(0);
    binding.synchronize().unwrap();
    assert!(backend.loaded_intids(0).is_empty());
}

#[test]
fn dropping_vcpu_binding_releases_redistributor_for_retry() {
    let (controller, _) = controller(1, 1);
    let binding = attach(&controller, 0, GicAffinity::new(0, 0, 0, 0));

    drop(binding);

    let rebound = controller.attach_vcpu(
        GicVcpuId::new(0),
        GicAffinity::new(0, 0, 0, 0),
        Arc::new(NoopWake),
    );
    assert!(rebound.is_ok());
}

fn controller(
    vcpu_count: usize,
    list_register_count: usize,
) -> (GicV3Controller, Arc<TestBackend>) {
    let config = GicV3Config::new(
        GicV3SpiOwnership::AllGuestOwned,
        GicV3MmioRegion::new(0x0800_0000, 0x1_0000).unwrap(),
        GicV3MmioRegion::new(0x080a_0000, 0x2_0000 * vcpu_count as u64).unwrap(),
        0x2_0000,
        vcpu_count,
    )
    .unwrap()
    .with_spi_count(32)
    .unwrap()
    .with_list_register_count(list_register_count)
    .unwrap();
    let backend = Arc::new(TestBackend::default());
    let controller = GicV3Controller::new(config, backend.clone()).unwrap();
    (controller, backend)
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

fn enable_spi(controller: &GicV3Controller, spi: SpiId) {
    let bank = u64::from(spi.raw() / 32);
    let bit = spi.raw() % 32;
    controller
        .write_distributor(GICD_ISENABLER + bank * 4, AccessWidth::Dword, 1 << bit)
        .unwrap();
    controller
        .write_distributor(GICD_CTLR, AccessWidth::Dword, 1 << 1)
        .unwrap();
}

struct NoopWake;

impl GicV3VcpuWake for NoopWake {
    fn wake(&self) -> VgicResult {
        Ok(())
    }
}

#[derive(Default)]
struct TestBackend {
    interfaces: Mutex<BTreeMap<GicVcpuId, CpuInterfaceState>>,
    retired: Mutex<Vec<(GicVcpuId, IntId)>>,
}

impl TestBackend {
    fn loaded_hcr(&self, raw_vcpu: usize) -> u64 {
        self.interfaces
            .lock()
            .unwrap()
            .get(&GicVcpuId::new(raw_vcpu))
            .map_or(0, CpuInterfaceState::hcr)
    }

    fn loaded_intids(&self, raw_vcpu: usize) -> Vec<IntId> {
        self.interfaces
            .lock()
            .unwrap()
            .get(&GicVcpuId::new(raw_vcpu))
            .into_iter()
            .flat_map(CpuInterfaceState::list_registers)
            .flatten()
            .map(|entry| entry.intid())
            .collect()
    }

    fn loaded_priorities(&self, raw_vcpu: usize) -> Vec<u8> {
        self.interfaces
            .lock()
            .unwrap()
            .get(&GicVcpuId::new(raw_vcpu))
            .into_iter()
            .flat_map(CpuInterfaceState::list_registers)
            .flatten()
            .map(|entry| entry.priority().raw())
            .collect()
    }

    fn loaded_states(&self, raw_vcpu: usize) -> Vec<InterruptState> {
        self.interfaces
            .lock()
            .unwrap()
            .get(&GicVcpuId::new(raw_vcpu))
            .into_iter()
            .flat_map(CpuInterfaceState::list_registers)
            .flatten()
            .map(|entry| entry.state())
            .collect()
    }

    fn complete_all(&self, raw_vcpu: usize) {
        if let Some(state) = self
            .interfaces
            .lock()
            .unwrap()
            .get_mut(&GicVcpuId::new(raw_vcpu))
        {
            state.list_registers_mut().fill(None);
        }
    }

    fn activate_all(&self, raw_vcpu: usize) {
        if let Some(state) = self
            .interfaces
            .lock()
            .unwrap()
            .get_mut(&GicVcpuId::new(raw_vcpu))
        {
            for entry in state.list_registers_mut().iter_mut().flatten() {
                entry.set_state(InterruptState::Active);
            }
        }
    }

    fn set_eoi_count(&self, raw_vcpu: usize, count: u8) {
        const ICH_HCR_EOI_COUNT_MASK: u64 = 0x1f << 27;

        if let Some(state) = self
            .interfaces
            .lock()
            .unwrap()
            .get_mut(&GicVcpuId::new(raw_vcpu))
        {
            state
                .set_hcr((state.hcr() & !ICH_HCR_EOI_COUNT_MASK) | (u64::from(count & 0x1f) << 27));
        }
    }

    fn retired_interrupts(&self) -> Vec<(GicVcpuId, IntId)> {
        self.retired.lock().unwrap().clone()
    }
}

impl GicV3Backend for TestBackend {
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
        if let Some(current) = self.interfaces.lock().unwrap().get(&vcpu) {
            *state = current.clone();
        }
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
