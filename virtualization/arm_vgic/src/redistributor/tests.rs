use axvm_types::AccessWidth;

use super::*;
use crate::{
    GicV3Config, GicV3Controller, GicV3MmioRegion, GicV3SpiOwnership, SgiId, SgiTarget,
    SoftwareGicV3Backend, register::GICR_WAKER,
};

struct NoopWake;

impl GicV3VcpuWake for NoopWake {
    fn wake(&self) -> VgicResult {
        Ok(())
    }
}

#[test]
fn software_level_delivery_preserves_eoi_maintenance_in_list_register() {
    let mut redistributor = RedistributorState::new(
        GicVcpuId::new(0),
        GicAffinity::new(0, 0, 0, 0),
        4,
        0,
        Arc::new(NoopWake),
    )
    .unwrap();
    let timer = IntId::new(27).unwrap();
    let software_edge = IntId::new(1).unwrap();

    redistributor.queue(timer, TriggerMode::Level);
    redistributor.queue(software_edge, TriggerMode::Edge);
    redistributor
        .refill_list_registers(|_| unreachable!("private interrupts have local priorities"))
        .unwrap();

    let timer_entry = redistributor
        .cpu_interface()
        .list_registers()
        .iter()
        .flatten()
        .find(|entry| entry.intid() == timer)
        .unwrap();
    let edge_entry = redistributor
        .cpu_interface()
        .list_registers()
        .iter()
        .flatten()
        .find(|entry| entry.intid() == software_edge)
        .unwrap();

    assert!(timer_entry.maintenance_on_eoi());
    assert!(!edge_entry.maintenance_on_eoi());
}

#[test]
fn physical_delivery_uses_only_preallocated_queue_slots() {
    let mut redistributor = RedistributorState::new(
        GicVcpuId::new(0),
        GicAffinity::new(0, 0, 0, 0),
        4,
        4,
        Arc::new(NoopWake),
    )
    .unwrap();
    let capacity = redistributor.queued_deliveries.capacity();

    for raw in 0..32 {
        let trigger = if raw < 16 {
            TriggerMode::Edge
        } else {
            TriggerMode::Level
        };
        redistributor.queue(IntId::new(raw).unwrap(), trigger);
    }
    for raw in 32..36 {
        redistributor
            .queue_physical(IntId::new(raw).unwrap(), PhysicalIrqId::new(u64::from(raw)))
            .unwrap();
    }

    assert_eq!(redistributor.queued_deliveries.capacity(), capacity);
    assert!(matches!(
        redistributor.queue_physical(IntId::new(36).unwrap(), PhysicalIrqId::new(36),),
        Err(VgicError::DeliveryQueueFull { .. })
    ));
    assert_eq!(redistributor.queued_deliveries.capacity(), capacity);
}

#[test]
fn waker_processor_sleep_is_retained_and_reports_children_asleep() {
    let config = GicV3Config::new(
        GicV3SpiOwnership::AllGuestOwned,
        GicV3MmioRegion::new(0x0800_0000, 0x1_0000).unwrap(),
        GicV3MmioRegion::new(0x080a_0000, 0x2_0000).unwrap(),
        0x2_0000,
        1,
    )
    .unwrap();
    let controller = GicV3Controller::new(config, Arc::new(SoftwareGicV3Backend)).unwrap();
    // Keep the binding alive: dropping it detaches the vCPU and removes its
    // Redistributor from the controller.
    let _binding = controller
        .attach_vcpu(
            GicVcpuId::new(0),
            GicAffinity::new(0, 0, 0, 0),
            Arc::new(NoopWake),
        )
        .unwrap();

    // A guest powering down a secondary CPU writes GICR_WAKER.ProcessorSleep
    // (bit 1) and then polls ChildrenAsleep (bit 0).  The redistributor is
    // quiescent here, so the write must be retained and ChildrenAsleep must
    // become visible.
    controller
        .write_redistributor(GicVcpuId::new(0), GICR_WAKER, AccessWidth::Dword, 0b10)
        .unwrap();
    let waker = controller
        .read_redistributor(GicVcpuId::new(0), GICR_WAKER, AccessWidth::Dword)
        .unwrap();
    assert_eq!(
        waker & 0b11,
        0b11,
        "ProcessorSleep write must be retained and ChildrenAsleep must be visible after quiescence"
    );
}

#[test]
fn waker_children_asleep_clears_while_delivery_is_queued() {
    let config = GicV3Config::new(
        GicV3SpiOwnership::AllGuestOwned,
        GicV3MmioRegion::new(0x0800_0000, 0x1_0000).unwrap(),
        GicV3MmioRegion::new(0x080a_0000, 0x2_0000).unwrap(),
        0x2_0000,
        1,
    )
    .unwrap();
    let controller = GicV3Controller::new(config, Arc::new(SoftwareGicV3Backend)).unwrap();
    // Keep the binding alive: dropping it detaches the vCPU and removes its
    // Redistributor from the controller.
    let _binding = controller
        .attach_vcpu(
            GicVcpuId::new(0),
            GicAffinity::new(0, 0, 0, 0),
            Arc::new(NoopWake),
        )
        .unwrap();

    controller
        .write_redistributor(GicVcpuId::new(0), GICR_WAKER, AccessWidth::Dword, 0b10)
        .unwrap();
    assert_eq!(
        controller
            .read_redistributor(GicVcpuId::new(0), GICR_WAKER, AccessWidth::Dword)
            .unwrap()
            & 0b11,
        0b11,
        "quiescent Redistributor reports ChildrenAsleep after ProcessorSleep"
    );

    // A pending delivery makes the Redistributor non-quiescent: ChildrenAsleep
    // must clear while ProcessorSleep stays set.  SGIs are deliverable by
    // default, so this queues one without further configuration.
    controller
        .send_sgi(
            GicVcpuId::new(0),
            SgiId::new(1).unwrap(),
            SgiTarget::SelfOnly,
        )
        .unwrap();
    assert_eq!(
        controller
            .read_redistributor(GicVcpuId::new(0), GICR_WAKER, AccessWidth::Dword)
            .unwrap()
            & 0b11,
        0b10,
        "ChildrenAsleep must clear while a delivery is queued"
    );
}
