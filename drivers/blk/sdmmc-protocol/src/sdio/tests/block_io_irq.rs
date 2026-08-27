use super::*;

#[test]
fn set_bus_width_bit8_is_unsupported_via_acmd6() {
    assert_eq!(sd_acmd6_arg(BusWidth::Bit8), Err(Error::UnsupportedCommand));
}

#[test]
fn submit_read_blocks_into_leaves_multi_block_stop_to_host_request() {
    let mut host = MockHost::new(std::vec![ok_r1()]);
    let expected: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();
    host.next_read_payload = Some(expected.clone());

    let mut driver = SdMmcCard::new(host);
    driver.high_capacity = true;
    let mut buf = [0u8; 1024];

    let mut request = driver.submit_read_blocks_into(7, &mut buf).unwrap();
    assert!(matches!(
        driver
            .advance_data_request(&mut request, sdmmc_host::ProgressCause::AcknowledgedIrq)
            .unwrap(),
        DataCommandProgress::Complete(_)
    ));

    assert_eq!(&buf[..], &expected[..]);
    assert_eq!(
        driver.host().data_requests,
        std::vec![(sdmmc_host::DataDirection::Read, 512, 2)]
    );
    assert_eq!(
        driver
            .host()
            .commands
            .iter()
            .map(|c| c.index)
            .collect::<Vec<_>>(),
        std::vec![18]
    );
    assert_eq!(driver.host().commands[0].argument, 7);
}

#[test]
fn submit_write_blocks_from_leaves_multi_block_stop_to_host_request() {
    let host = MockHost::new(std::vec![ok_r1()]);
    let mut driver = SdMmcCard::new(host);
    driver.high_capacity = true;
    let buf = [0x5au8; 1024];

    let mut request = driver.submit_write_blocks_from(11, &buf).unwrap();
    assert!(matches!(
        driver
            .advance_data_request(&mut request, sdmmc_host::ProgressCause::AcknowledgedIrq)
            .unwrap(),
        DataCommandProgress::Complete(_)
    ));

    assert_eq!(
        driver.host().data_requests,
        std::vec![(sdmmc_host::DataDirection::Write, 512, 2)]
    );
    assert_eq!(
        driver
            .host()
            .commands
            .iter()
            .map(|c| c.index)
            .collect::<Vec<_>>(),
        std::vec![25]
    );
    assert_eq!(driver.host().commands[0].argument, 11);
    assert_eq!(driver.host().writes, std::vec![buf.to_vec()]);
}

#[test]
fn submit_block_io_rejects_misaligned_buffers() {
    let host = MockHost::new(std::vec![]);
    let mut driver = SdMmcCard::new(host);
    let mut read_buf = [0u8; 513];
    let write_buf = [0u8; 513];

    assert_eq!(
        driver.submit_read_blocks_into(0, &mut read_buf).map(|_| ()),
        Err(Error::Misaligned)
    );
    assert_eq!(
        driver.submit_write_blocks_from(0, &write_buf).map(|_| ()),
        Err(Error::Misaligned)
    );
    assert!(driver.host().commands.is_empty());
}

struct MockIrqHandle {
    event: IrqTestEvent,
}

impl SdMmcIrqHandle for MockIrqHandle {
    type Event = IrqTestEvent;

    fn handle_irq(&mut self) -> Self::Event {
        self.event
    }
}

#[derive(Clone, Copy, Default)]
struct IrqTestEvent(HostEventKind);

impl HostEvent for IrqTestEvent {
    fn kind(&self) -> HostEventKind {
        self.0
    }
}

#[test]
fn host_irq_events_map_to_single_sdmmc_block_queue() {
    assert_eq!(
        block_queue_ready_from_host_event(&IrqTestEvent(HostEventKind::None)),
        None
    );
    for kind in [
        HostEventKind::CommandComplete,
        HostEventKind::TransferComplete,
        HostEventKind::ReceiveReady,
        HostEventKind::TransmitReady,
        HostEventKind::Error,
        HostEventKind::Other,
    ] {
        assert_eq!(
            block_queue_ready_from_host_event(&IrqTestEvent(kind)),
            Some(SDMMC_BLOCK_QUEUE_ID)
        );
    }
}

#[test]
fn irq_handle_is_move_only_and_handles_with_mutable_endpoint() {
    let mut handle = MockIrqHandle {
        event: IrqTestEvent(HostEventKind::TransferComplete),
    };

    assert_eq!(handle.handle_irq().kind(), HostEventKind::TransferComplete);
}
