use rdif_block::{
    DeviceInfo, IdList, QueueContractError, QueueExecution, QueueInfo, QueueKind, QueueLimits,
    validate_queue_info,
};

fn interrupt_kind() -> QueueKind {
    let mut sources = IdList::none();
    sources.insert(2);
    QueueKind::Interrupt { sources }
}

fn queue(kind: QueueKind, execution: QueueExecution) -> QueueInfo {
    QueueInfo {
        id: 0,
        device: DeviceInfo::new(8, 512),
        limits: QueueLimits::simple(512, u64::MAX),
        kind,
        execution,
    }
}

#[test]
fn queue_execution_is_an_explicit_ownership_contract() {
    assert_eq!(
        validate_queue_info(queue(QueueKind::Inline, QueueExecution::Inline)),
        Ok(())
    );
    assert_eq!(
        validate_queue_info(queue(interrupt_kind(), QueueExecution::Tagged)),
        Ok(())
    );
    assert_eq!(
        validate_queue_info(queue(interrupt_kind(), QueueExecution::Serialized)),
        Ok(())
    );

    assert_eq!(
        validate_queue_info(queue(QueueKind::Inline, QueueExecution::Tagged)),
        Err(QueueContractError::QueueExecutionMismatch { queue_id: 0 })
    );
    assert_eq!(
        validate_queue_info(queue(interrupt_kind(), QueueExecution::Inline)),
        Err(QueueContractError::QueueExecutionMismatch { queue_id: 0 })
    );
}
