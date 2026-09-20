use super::*;
use crate::{
    response::{SdioOcrResponse, SdioRwResponse},
    sdio::io::SdioInitRequest,
};

const SDIO_READY: u32 = (1 << 31) | (1 << 28) | 0x0030_0000;

#[test]
fn io_only_init_paces_after_identification_clock_before_cmd5() {
    let mut card = SdioCard::new(MockHost::new(std::vec![r4(SDIO_READY)]));
    let mut request = card.submit_init().unwrap();

    for _ in 0..2 {
        assert!(matches!(
            card.advance_init_request(&mut request, sdmmc_host::ProgressCause::AcknowledgedIrq,),
            Ok(OperationProgress::Pending)
        ));
    }
    assert_eq!(
        request.register_retry_after(),
        Some(core::time::Duration::from_millis(10))
    );
    assert_eq!(card.host().last_clock, None);

    assert!(matches!(
        card.advance_init_request(&mut request, sdmmc_host::ProgressCause::RegisterRetry,),
        Ok(OperationProgress::Pending)
    ));
    for _ in 0..2 {
        assert!(matches!(
            card.advance_init_request(&mut request, sdmmc_host::ProgressCause::AcknowledgedIrq,),
            Ok(OperationProgress::Pending)
        ));
    }

    assert_eq!(card.host().last_clock, Some(ClockSpeed::Identification));
    assert_eq!(
        request.register_retry_after(),
        Some(core::time::Duration::from_millis(10))
    );
    assert!(
        card.host().commands.is_empty(),
        "CMD5 must not be submitted until the post-clock power stabilization delay expires"
    );

    assert!(matches!(
        card.advance_init_request(&mut request, sdmmc_host::ProgressCause::RegisterRetry,),
        Ok(OperationProgress::Pending)
    ));
    assert_eq!(card.host().commands.len(), 1);
    assert_eq!(card.host().commands[0].index, 5);
}

#[test]
fn io_only_init_enumerates_cccr_fbr_and_cis_without_host_protocol_duplication() {
    let (mut card, info) = initialized_io_only_card(std::vec![]);

    assert_eq!(info.rca, 0x2345);
    assert_eq!(info.io_functions, 1);
    assert_eq!(info.common_cis.manufacturer_id, Some(0x02c8));
    assert_eq!(info.common_cis.product_id, Some(0x8878));
    let function = card.function(FunctionNumber::new(1).unwrap()).unwrap();
    assert_eq!(function.interface_code, 2);
    assert_eq!(function.block_size.unwrap().get(), 512);
    assert_eq!(function.cis.pointer, 0x120);
    assert_eq!(function.cis.manufacturer_id, Some(0x02c8));
    assert_eq!(card.host().bus_width, Some(BusWidth::Bit4));
    assert_eq!(card.host().last_clock, Some(ClockSpeed::HighSpeed));

    card.host_mut().replies.extend(
        std::vec![r5(0), r5(1 << 1), r5(0), r5(1 << 1)]
            .into_iter()
            .map(Ok),
    );
    let function_number = FunctionNumber::new(1).unwrap();

    assert_eq!(
        card.submit_set_block_size(
            FunctionNumber::COMMON,
            core::num::NonZeroU16::new(512).unwrap(),
        )
        .err(),
        Some(Error::InvalidArgument)
    );

    let mut invalid_buffer = [0u8; 1];
    assert_eq!(
        card.submit_read(
            FunctionNumber::COMMON,
            IoAddress::new(0).unwrap(),
            AddressMode::Fixed,
            TransferMode::Byte,
            &mut invalid_buffer,
        )
        .err(),
        Some(Error::InvalidArgument)
    );
    assert_eq!(
        card.submit_read(
            FunctionNumber::new(2).unwrap(),
            IoAddress::new(0).unwrap(),
            AddressMode::Fixed,
            TransferMode::Byte,
            &mut invalid_buffer,
        )
        .err(),
        Some(Error::InvalidArgument)
    );

    let mut enable = card.submit_enable_function(function_number).unwrap();
    loop {
        match card
            .advance_enable_function(&mut enable, sdmmc_host::ProgressCause::AcknowledgedIrq)
            .unwrap()
        {
            OperationProgress::Pending => {}
            OperationProgress::Complete(()) => break,
        }
    }

    card.host_mut()
        .replies
        .extend(std::vec![r5(0), r5(0), r5(0), r5(2)].into_iter().map(Ok));
    let block_size = core::num::NonZeroU16::new(512).unwrap();
    let mut set_size = card
        .submit_set_block_size(function_number, block_size)
        .unwrap();
    loop {
        match card
            .advance_set_block_size(&mut set_size, sdmmc_host::ProgressCause::AcknowledgedIrq)
            .unwrap()
        {
            OperationProgress::Pending => {}
            OperationProgress::Complete(()) => break,
        }
    }
    assert_eq!(
        card.function(function_number).unwrap().block_size,
        Some(block_size)
    );

    card.host_mut()
        .replies
        .extend(std::vec![r5(0), r5(0b11)].into_iter().map(Ok));
    let mut interrupt = card
        .submit_enable_function_interrupt(function_number)
        .unwrap();
    loop {
        match card
            .advance_enable_function_interrupt(
                &mut interrupt,
                sdmmc_host::ProgressCause::AcknowledgedIrq,
            )
            .unwrap()
        {
            OperationProgress::Pending => {}
            OperationProgress::Complete(()) => break,
        }
    }
}

#[test]
fn stale_direct_request_cannot_advance_a_later_command() {
    let mut card = SdioCard::new(MockHost::new(std::vec![r5(0x11), r5(0x22)]));
    let address = IoAddress::new(0x10).unwrap();
    let mut stale = card
        .submit_read_byte(FunctionNumber::COMMON, address)
        .unwrap();
    card.abort_direct_request(&mut stale).unwrap();
    let mut active = card
        .submit_read_byte(FunctionNumber::COMMON, address)
        .unwrap();

    assert!(matches!(
        card.advance_direct_request(&mut stale, sdmmc_host::ProgressCause::AcknowledgedIrq,),
        Err(Error::InvalidArgument)
    ));
    assert!(matches!(
        card.advance_direct_request(&mut active, sdmmc_host::ProgressCause::AcknowledgedIrq,),
        Ok(OperationProgress::Complete(0x22))
    ));
}

#[test]
fn combo_card_is_rejected_before_rca_assignment() {
    let combo = (1 << 28) | (1 << 27) | 0x0030_0000;
    let mut card = SdioCard::new(MockHost::new(std::vec![r4(combo), r4(combo | (1 << 31)),]));
    let mut request = card.submit_init().unwrap();

    loop {
        match advance_init(&mut card, &mut request) {
            Ok(OperationProgress::Pending) => {}
            Err(error) => {
                assert_eq!(error, Error::UnsupportedComboCard);
                assert!(
                    card.host()
                        .commands
                        .iter()
                        .all(|command| command.index != 3)
                );
                break;
            }
            Ok(OperationProgress::Complete(_)) => panic!("combo card unexpectedly initialized"),
        }
    }
}

#[test]
fn direct_and_extended_io_preserve_typed_wire_semantics() {
    let function = FunctionNumber::new(1).unwrap();
    let (mut card, _) = initialized_io_only_card(std::vec![
        r5(0),
        r5(1 << 1),
        r5(0),
        r5(1 << 1),
        r5(0x5a),
        r5(0),
    ]);
    let mut enable = card.submit_enable_function(function).unwrap();
    loop {
        match card
            .advance_enable_function(&mut enable, sdmmc_host::ProgressCause::AcknowledgedIrq)
            .unwrap()
        {
            OperationProgress::Pending => {}
            OperationProgress::Complete(()) => break,
        }
    }
    card.host_mut().next_read_payload = Some(std::vec![0xa5; 512]);
    let address = IoAddress::new(0x1_abcd).unwrap();

    let mut direct = card.submit_read_byte(function, address).unwrap();
    assert!(matches!(
        card.advance_direct_request(&mut direct, sdmmc_host::ProgressCause::AcknowledgedIrq),
        Ok(OperationProgress::Complete(0x5a))
    ));

    let mut buffer = [0u8; 512];
    let mut transfer = card
        .submit_read(
            function,
            address,
            AddressMode::Fixed,
            TransferMode::Byte,
            &mut buffer,
        )
        .unwrap();
    assert!(matches!(
        card.advance_transfer_request(&mut transfer, sdmmc_host::ProgressCause::AcknowledgedIrq),
        Ok(OperationProgress::Complete(()))
    ));
    drop(transfer);
    assert_eq!(buffer, [0xa5; 512]);
    assert_eq!(
        card.host().data_requests.last().copied(),
        Some((sdmmc_host::DataDirection::Read, 512, 1,))
    );
    let command = card.host().commands.last().unwrap();
    assert_eq!(command.index, 53);
    assert_eq!(command.argument & 0x1ff, 0);
    assert_eq!((command.argument >> 26) & 1, 0); // fixed address
    assert_eq!((command.argument >> 27) & 1, 0); // byte mode
}

fn initialized_io_only_card(
    mut extra_replies: std::vec::Vec<Response>,
) -> (SdioCard<MockHost>, SdioCardInfo) {
    let mut replies = std::vec![
        r4(SDIO_READY),
        rca_response(0x2345),
        ok_r1(),
        r5(0x32), // CCCR/SDIO revision
        r5(0x03), // SD physical revision
        r5(0x00), // normal-speed four-bit capability
        r5(0x00), // current bus interface
        r5(0x01), // high-speed supported
        r5(0x00),
        r5(0x01),
        r5(0x00), // common CIS pointer = 0x100
        r5(0x20),
        r5(4),
        r5(0xc8),
        r5(0x02),
        r5(0x78),
        r5(0x88), // common MANFID
        r5(0x02), // function interface code
        r5(0x20),
        r5(0x01),
        r5(0x00), // function CIS pointer = 0x120
        r5(0x00),
        r5(0x02), // block size = 512
        r5(0x20),
        r5(4),
        r5(0xc8),
        r5(0x02),
        r5(0x78),
        r5(0x88), // function MANFID
        r5(0x02), // four-bit bus write readback
        r5(0x03), // high-speed write readback
    ];
    replies.append(&mut extra_replies);
    let mut card = SdioCard::new(MockHost::new(replies));
    let mut request = card.submit_init().unwrap();
    let info = loop {
        match advance_init(&mut card, &mut request).unwrap() {
            OperationProgress::Pending => {}
            OperationProgress::Complete(info) => break info,
        }
    };
    (card, info)
}

fn advance_init(
    card: &mut SdioCard<MockHost>,
    request: &mut SdioInitRequest<MockHost>,
) -> Result<OperationProgress<SdioCardInfo>, Error> {
    let cause = if request.register_retry_after().is_some() {
        sdmmc_host::ProgressCause::RegisterRetry
    } else {
        sdmmc_host::ProgressCause::AcknowledgedIrq
    };
    card.advance_init_request(request, cause)
}

fn r4(raw: u32) -> Response {
    Response::R4(SdioOcrResponse::from_raw(raw))
}

fn r5(data: u8) -> Response {
    Response::R5(SdioRwResponse::from_raw(u32::from(data)))
}
