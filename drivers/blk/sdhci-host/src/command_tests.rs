//! Tests for command issue, IRQ caching, and response progress.

use core::ptr::NonNull;

use sdmmc_protocol::{
    DataDirection,
    cmd::{cmd7, cmd17},
    sdio::host::SdioIrqHandle,
};

use super::*;

#[repr(align(4))]
struct FakeRegs([u8; 0x100]);

#[test]
fn multi_block_transfer_mode_leaves_stop_command_to_request_state_machine() {
    let mode = transfer_mode(DataDirection::Read, 4, false);

    assert_ne!(mode & XFER_MODE_MULTI_BLOCK, 0);
    assert_eq!(mode & XFER_MODE_AUTO_CMD12, 0);
}

#[test]
fn fifo_status_consumes_irq_cached_buffer_ready() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    host.irq.state.begin_request();
    let generation = host.irq.state.generation();
    host.irq.state.cache_if_current(
        generation,
        NORMAL_INT_BUFFER_WRITE_READY | NORMAL_INT_XFER_COMPLETE,
        0,
    );

    let (status, _) = host.take_fifo_irq_status(NORMAL_INT_BUFFER_WRITE_READY | NORMAL_INT_ERROR);

    assert_ne!(status & NORMAL_INT_BUFFER_WRITE_READY, 0);
    assert_eq!(
        host.irq.state.pending_normal() & NORMAL_INT_BUFFER_WRITE_READY,
        0,
        "FIFO ready must be consumed after the data step handles it"
    );
    assert_ne!(
        host.irq.state.pending_normal() & NORMAL_INT_XFER_COMPLETE,
        0,
        "transfer completion belongs to the data-complete poll step"
    );
}

#[test]
fn fifo_status_consumes_irq_cached_error_bits() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    host.irq.state.begin_request();
    let generation = host.irq.state.generation();
    host.irq
        .state
        .cache_if_current(generation, NORMAL_INT_ERROR, ERROR_INT_DATA_TIMEOUT);

    let (status, error) =
        host.take_fifo_irq_status(NORMAL_INT_BUFFER_READ_READY | NORMAL_INT_ERROR);

    assert_ne!(
        status & NORMAL_INT_ERROR,
        0,
        "FIFO poll must observe error status cached by the IRQ handler"
    );
    assert_ne!(
        error & ERROR_INT_DATA_TIMEOUT,
        0,
        "FIFO poll must preserve error bits after the IRQ handler clears hardware status"
    );
    assert_eq!(host.irq.state.pending_normal() & NORMAL_INT_ERROR, 0);
    assert_eq!(host.irq.state.pending_error(), 0);
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
    host.pending_data = Some(crate::host::PendingData {
        direction: DataDirection::Read,
        block_size: 512,
        block_count: 1,
    });

    host.submit_command(&cmd17(0)).unwrap();

    assert_eq!(host.irq.state.pending_normal(), 0);
    assert_eq!(host.irq.state.pending_error(), 0);
}

#[test]
fn issued_command_keeps_irq_generation_active_for_completion_cache() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    host.pending_data = Some(crate::host::PendingData {
        direction: DataDirection::Read,
        block_size: 512,
        block_count: 1,
    });

    host.submit_command(&cmd17(0)).unwrap();
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
fn r1b_command_completes_when_dat0_is_already_released_at_command_irq() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();

    host.submit_command(&cmd7(1)).unwrap();
    host.write_u32(REG_PRESENT_STATE, PRESENT_DAT0_LINE_SIGNAL_LEVEL);
    host.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CMD_COMPLETE);
    let mut irq = host.irq_endpoint();
    assert_eq!(irq.handle_irq(), crate::Event::CommandComplete);

    assert!(matches!(
        host.poll_command_response(),
        Ok(CommandResponsePoll::Complete(Response::R1b(_)))
    ));
}

#[test]
fn r1b_busy_release_is_register_progress_after_command_irq() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();

    host.submit_command(&cmd7(1)).unwrap();
    host.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CMD_COMPLETE);
    let mut irq = host.irq_endpoint();
    assert_eq!(irq.handle_irq(), crate::Event::CommandComplete);
    assert!(matches!(
        host.poll_command_response(),
        Ok(CommandResponsePoll::Pending)
    ));
    assert_eq!(
        host.progress_wait_kind(),
        sdmmc_protocol::sdio::HostProgressWait::Register
    );

    host.write_u32(REG_PRESENT_STATE, PRESENT_DAT0_LINE_SIGNAL_LEVEL);
    assert!(matches!(
        host.poll_command_response(),
        Ok(CommandResponsePoll::Complete(Response::R1b(_)))
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
