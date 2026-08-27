//! Tests for command issue, IRQ caching, and response progress.

use core::ptr::NonNull;

use sdmmc_protocol::{
    CommandProgress, DataDirection,
    cmd::{cmd7, cmd17},
    sdio::host::SdMmcIrqHandle,
};

use super::*;

#[repr(align(4))]
struct FakeRegs([u8; 0x100]);

#[test]
fn generic_multi_block_transfer_does_not_assume_auto_cmd12() {
    let mode = transfer_mode(DataDirection::Read, 4, false);

    assert_ne!(mode & XFER_MODE_MULTI_BLOCK, 0);
    assert_eq!(
        mode & XFER_MODE_AUTO_CMD12,
        0,
        "Linux enables Auto CMD12 only for an explicit controller quirk"
    );
}

#[test]
fn new_command_discards_cached_irq_status_from_previous_request() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.irq.state.begin_request();
    let old_generation = host.irq.state.generation();
    host.irq.state.cache_if_current(
        old_generation,
        NORMAL_INT_CMD_COMPLETE | NORMAL_INT_XFER_COMPLETE,
        ERROR_INT_DATA_TIMEOUT,
    );
    host.submit_dma_command(
        &cmd17(0),
        crate::host::PendingData {
            direction: DataDirection::Read,
            block_size: 512,
            block_count: 1,
        },
    )
    .unwrap();

    assert_eq!(host.irq.state.pending_normal(), 0);
    assert_eq!(host.irq.state.pending_error(), 0);
}

#[test]
fn issued_command_keeps_irq_generation_active_for_completion_cache() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_interrupt_status_capture();
    host.enable_completion_irq();
    host.submit_dma_command(
        &cmd17(0),
        crate::host::PendingData {
            direction: DataDirection::Read,
            block_size: 512,
            block_count: 1,
        },
    )
    .unwrap();
    assert_ne!(host.irq.state.generation(), 0);

    host.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CMD_COMPLETE);
    let mut irq = host.irq_endpoint();
    assert_eq!(irq.handle_irq(), crate::Event::CommandComplete);
    assert_ne!(
        host.irq.state.pending_normal() & NORMAL_INT_CMD_COMPLETE,
        0,
        "IRQ handler must cache completion status for the active generation"
    );
}

#[test]
fn task_side_does_not_harvest_unacknowledged_raw_command_status() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.command_state = CommandState::Issued {
        cmd: cmd17(0),
        data_line: false,
        polls: 0,
    };
    host.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CMD_COMPLETE);

    assert!(matches!(
        host.advance_command(),
        Ok(CommandProgress::Pending)
    ));
}

#[test]
fn r1b_command_completes_when_dat0_is_already_released_at_command_irq() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_interrupt_status_capture();
    host.enable_completion_irq();

    host.submit_command(&cmd7(1)).unwrap();
    host.write_u32(REG_PRESENT_STATE, PRESENT_DAT0_LINE_SIGNAL_LEVEL);
    host.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CMD_COMPLETE);
    let mut irq = host.irq_endpoint();
    assert_eq!(irq.handle_irq(), crate::Event::CommandComplete);

    assert!(matches!(
        host.advance_command_response(),
        Ok(CommandResponseProgress::Complete(Response::R1b(_)))
    ));
}

#[test]
fn r1b_busy_release_is_register_progress_after_command_irq() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_interrupt_status_capture();
    host.enable_completion_irq();

    host.submit_command(&cmd7(1)).unwrap();
    host.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CMD_COMPLETE);
    let mut irq = host.irq_endpoint();
    assert_eq!(irq.handle_irq(), crate::Event::CommandComplete);
    assert!(matches!(
        host.advance_command_response(),
        Ok(CommandResponseProgress::Pending)
    ));
    assert_eq!(
        host.progress_wait_kind(),
        sdmmc_protocol::sdio::HostProgressWait::Register {
            retry_after: crate::SDHCI_REGISTER_RETRY_DELAY,
        }
    );

    host.write_u32(REG_PRESENT_STATE, PRESENT_DAT0_LINE_SIGNAL_LEVEL);
    assert!(matches!(
        host.advance_command_response(),
        Ok(CommandResponseProgress::Complete(Response::R1b(_)))
    ));
}

#[test]
fn irq_cache_drops_events_from_previous_generation() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let host = unsafe { Sdhci::new(base) };
    host.irq.state.begin_request();
    let old_generation = host.irq.state.generation();
    host.irq.state.end_request();
    host.irq.state.begin_request();
    assert_ne!(host.irq.state.generation(), old_generation);

    host.irq
        .state
        .cache_if_current(old_generation, NORMAL_INT_CMD_COMPLETE, 0);

    assert_eq!(host.irq.state.pending_normal(), 0);
}
