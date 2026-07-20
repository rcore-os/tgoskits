use core::ptr::NonNull;

use rdif_irq::{IrqCapture, IrqEndpoint};
use sdmmc_protocol::{DataDirection, cmd::cmd17};

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
fn command_completion_never_hides_coalesced_data_error() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    host.irq.state.begin_request();
    host.command_state = CommandState::Issued { cmd: cmd17(0) };
    let generation = host.irq.state.generation();
    host.irq.state.cache_if_current(
        generation,
        NORMAL_INT_CMD_COMPLETE | NORMAL_INT_ERROR,
        ERROR_INT_DATA_CRC,
    );

    assert!(matches!(host.poll_command(), Err(Error::Crc(_))));
    assert_eq!(
        host.read_u8(REG_SOFTWARE_RESET),
        0,
        "IRQ error service must defer reset to the lifecycle FSM"
    );
}

#[test]
fn merged_cmd12_completion_and_busy_release_finish_in_one_poll() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    host.irq.state.begin_request();
    host.command_state = CommandState::Issued {
        cmd: sdmmc_protocol::cmd::CMD12,
    };
    let generation = host.irq.state.generation();
    host.irq.state.cache_if_current(
        generation,
        NORMAL_INT_CMD_COMPLETE | NORMAL_INT_XFER_COMPLETE,
        0,
    );

    assert_eq!(host.poll_command(), Ok(CommandPoll::Complete));
}

#[test]
fn runtime_r1b_never_uses_present_state_as_completion() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    host.irq.state.begin_request();
    host.command_state = CommandState::WaitingBusy {
        cmd: sdmmc_protocol::cmd::CMD12,
        response: Response::Empty,
    };
    host.write_u32(REG_PRESENT_STATE, PRESENT_DAT0_LINE_SIGNAL_LEVEL);

    assert_eq!(host.poll_command(), Ok(CommandPoll::Pending));
}

#[test]
fn masked_runtime_request_never_falls_back_to_task_context_status_reads() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    host.irq.state.begin_request();
    host.command_state = CommandState::Issued { cmd: cmd17(0) };

    // A recovery path may temporarily mask signal delivery. Ownership of
    // the W1C status registers must nevertheless remain with the IRQ
    // endpoint until the controller lifecycle explicitly transfers it to
    // initialization/recovery ownership.
    host.disable_completion_irq();
    host.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CMD_COMPLETE);

    assert_eq!(host.poll_command(), Ok(CommandPoll::Pending));
}

#[test]
fn irq_owned_submit_rejects_inhibit_instead_of_waiting_for_timer_repoll() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    host.write_u32(REG_PRESENT_STATE, PRESENT_CMD_INHIBIT);

    assert_eq!(host.submit_command(&cmd17(0)), Err(Error::Busy));
    assert!(matches!(host.command_state, CommandState::Idle));
    assert_eq!(host.irq.state.generation(), 0);
    assert_eq!(host.read_u16(REG_COMMAND), 0);
}

#[test]
fn initialization_owned_submit_is_explicitly_fail_closed() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_initialization_status().unwrap();
    assert_eq!(
        host.submit_command(&cmd17(0)),
        Err(Error::UnsupportedCommand)
    );
    assert!(matches!(host.command_state, CommandState::Idle));
}

#[test]
fn command_completion_timeout_is_owned_by_the_external_watchdog() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    host.submit_command(&cmd17(0)).unwrap();
    for _ in 0..128 {
        assert_eq!(host.poll_command(), Ok(CommandPoll::Pending));
    }
}

#[test]
fn runtime_command_abort_requires_controller_lifecycle_quiescence() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    host.irq.state.begin_request();
    host.command_state = CommandState::Issued { cmd: cmd17(0) };

    assert_eq!(host.abort_command(), Err(Error::Busy));
    assert!(matches!(host.command_state, CommandState::Issued { .. }));
    assert_eq!(host.read_u8(REG_SOFTWARE_RESET), 0);
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

    let snapshot = host.take_fifo_irq_status(NORMAL_INT_BUFFER_WRITE_READY | NORMAL_INT_ERROR);

    assert_ne!(snapshot.normal & NORMAL_INT_BUFFER_WRITE_READY, 0);
    assert_eq!(
        host.irq.state.pending_normal() & NORMAL_INT_BUFFER_WRITE_READY,
        0,
        "FIFO ready must be consumed after the data step handles it"
    );
    assert_ne!(
        host.pending_irq.normal & NORMAL_INT_XFER_COMPLETE,
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

    let snapshot = host.take_fifo_irq_status(NORMAL_INT_BUFFER_READ_READY | NORMAL_INT_ERROR);

    assert_ne!(
        snapshot.normal & NORMAL_INT_ERROR,
        0,
        "FIFO poll must observe error status cached by the IRQ handler"
    );
    assert_ne!(
        snapshot.error & ERROR_INT_DATA_TIMEOUT,
        0,
        "FIFO poll must preserve error bits after the IRQ handler clears hardware status"
    );
    assert_eq!(host.irq.state.pending_normal() & NORMAL_INT_ERROR, 0);
    assert_eq!(host.irq.state.pending_error(), 0);
}

#[test]
fn new_command_rejects_unconsumed_irq_status_from_previous_generation() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
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
        adma_descriptor: None,
    });

    assert_eq!(host.submit_command(&cmd17(0)), Err(Error::Busy));

    assert_eq!(host.irq.state.generation(), old_generation);
    assert_ne!(host.irq.state.pending_normal(), 0);
    assert_ne!(host.irq.state.pending_error(), 0);
    assert!(matches!(host.command_state, CommandState::Idle));
}

#[test]
fn issued_command_keeps_irq_generation_active_for_completion_cache() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    let (mut irq, _control) = host.take_irq_source().unwrap().into_parts();
    host.enable_completion_irq();
    host.pending_data = Some(crate::host::PendingData {
        direction: DataDirection::Read,
        block_size: 512,
        block_count: 1,
        adma_descriptor: None,
    });

    host.submit_command(&cmd17(0)).unwrap();
    assert_ne!(host.irq.state.generation(), 0);

    host.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CMD_COMPLETE);
    assert!(matches!(
        irq.capture(),
        IrqCapture::Captured {
            event,
            masked: None,
        } if event == crate::Event::from_status(NORMAL_INT_CMD_COMPLETE, 0)
    ));
    assert_ne!(
        host.irq.state.pending_normal() & NORMAL_INT_CMD_COMPLETE,
        0,
        "IRQ handler must cache completion status for the active generation"
    );
}

#[test]
fn irq_cache_drops_events_from_previous_generation() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let host = unsafe { Sdhci::new(base) };
    assert!(host.irq.state.begin_request());
    let old_generation = host.irq.state.generation();
    assert!(host.irq.state.request_handoff_ready());
    assert!(host.irq.state.begin_request());
    assert_ne!(host.irq.state.generation(), old_generation);

    host.irq
        .state
        .cache_if_current(old_generation, NORMAL_INT_CMD_COMPLETE, 0);

    assert_eq!(host.irq.state.pending_normal(), 0);
}
