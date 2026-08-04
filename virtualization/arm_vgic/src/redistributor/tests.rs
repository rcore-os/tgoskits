use super::*;

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
