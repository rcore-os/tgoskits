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

    redistributor.queue(timer, TriggerMode::Level).unwrap();
    redistributor
        .queue(software_edge, TriggerMode::Edge)
        .unwrap();
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
    // One LPI staging slot keeps the arithmetic below easy to follow.
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
        redistributor
            .queue(IntId::new(raw).unwrap(), trigger)
            .unwrap();
    }
    // Fill the remaining preallocated slots with hardware-backed SPIs: the
    // queue must accept every push that fits without growing.
    let mut raw = 32;
    while redistributor.queued_deliveries.len() < capacity {
        redistributor
            .queue_physical(IntId::new(raw).unwrap(), PhysicalIrqId::new(u64::from(raw)))
            .unwrap();
        raw += 1;
    }

    assert_eq!(redistributor.queued_deliveries.capacity(), capacity);
    assert!(matches!(
        redistributor.queue_physical(IntId::new(raw).unwrap(), PhysicalIrqId::new(u64::from(raw))),
        Err(VgicError::DeliveryQueueFull { .. })
    ));
    assert!(matches!(
        redistributor.queue(IntId::new(raw).unwrap(), TriggerMode::Edge),
        Err(RefillFailure::QueueFull { .. })
    ));
    assert_eq!(redistributor.queued_deliveries.capacity(), capacity);
}

#[test]
fn task_side_prepare_materializes_lpi_records_and_grows_queue() {
    let mut redistributor = RedistributorState::new(
        GicVcpuId::new(0),
        GicAffinity::new(0, 0, 0, 0),
        4,
        0,
        Arc::new(NoopWake),
    )
    .unwrap();
    let lpis = [LpiId::new(8192).unwrap(), LpiId::new(9000).unwrap()];
    assert!(redistributor.lpi(lpis[0]).is_none());

    let base_queue = redistributor.delivery_queue_capacity();
    let mut capacity = RedistributorState::reserve_lpi_capacity(
        redistributor.lpi_record_count(),
        base_queue,
        lpis.len(),
    );
    let displaced = redistributor
        .try_install_lpis(&mut capacity, &lpis)
        .expect("the reserved capacity fits the requested records");
    drop(displaced);

    assert_eq!(redistributor.lpi_record_count(), lpis.len());
    assert!(redistributor.lpi_prepared(lpis[0]));
    assert!(redistributor.lpi_prepared(lpis[1]));
    assert!(redistributor.delivery_queue_capacity() >= base_queue + lpis.len());
    // A repeated prepare is a no-op and never re-materializes records.
    let mut capacity = RedistributorState::reserve_lpi_capacity(
        redistributor.lpi_record_count(),
        base_queue,
        lpis.len(),
    );
    assert!(
        redistributor
            .try_install_lpis(&mut capacity, &lpis)
            .is_some()
    );
    assert_eq!(redistributor.lpi_record_count(), lpis.len());
}
