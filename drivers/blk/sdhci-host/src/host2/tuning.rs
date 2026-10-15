use super::*;

impl Sdhci {
    pub(super) fn advance_host2_tuning(
        &mut self,
        state: &mut SdhciTuningState,
        cause: sdmmc_host::ProgressCause,
    ) -> Result<sdmmc_host::RequestProgress<()>, sdmmc_host::Error> {
        let now = self
            .timer
            .map(HostTimer::now_ms)
            .ok_or(sdmmc_host::Error::Controller)?;
        match *state {
            SdhciTuningState::Start {
                cmd_index,
                block_size,
            } => {
                let ctrl = (self.read_u16(REG_HOST_CONTROL2) | HOST_CTRL2_EXECUTE_TUNING)
                    & !HOST_CTRL2_SAMPLING_CLOCK_SELECT;
                self.write_u16(REG_HOST_CONTROL2, ctrl);
                self.issue_tuning_block(cmd_index, block_size);
                *state = SdhciTuningState::Wait {
                    cmd_index,
                    block_size,
                    remaining: SDHCI_TUNING_ATTEMPTS - 1,
                    deadline_ms: now.saturating_add(SDHCI_TUNING_TIMEOUT_MS),
                };
            }
            SdhciTuningState::Wait {
                cmd_index,
                block_size,
                remaining,
                deadline_ms,
            } => {
                // A timer wake enforces the deadline; only acknowledged IRQs
                // consume completion. Never poll the hardware status register.
                if cause == sdmmc_host::ProgressCause::AcknowledgedIrq {
                    let error = self.irq.state.take_error(ERROR_INT_CLEAR_ALL);
                    if error != 0 {
                        return Err(sdmmc_host::Error::Controller);
                    }
                    if self.irq.state.take_normal(NORMAL_INT_BUFFER_READ_READY) != 0 {
                        self.clear_cached_irq_status();
                        let ctrl = self.read_u16(REG_HOST_CONTROL2);
                        if ctrl & HOST_CTRL2_EXECUTE_TUNING == 0 {
                            return if ctrl & HOST_CTRL2_SAMPLING_CLOCK_SELECT != 0 {
                                Ok(sdmmc_host::RequestProgress::Complete(Ok(())))
                            } else {
                                Err(sdmmc_host::Error::Controller)
                            };
                        }
                        if remaining == 0 {
                            return Err(sdmmc_host::Error::Timeout);
                        }
                        // SD CMD19 requires a one millisecond inter-command
                        // delay. eMMC CMD21 may start the next attempt at once.
                        *state = SdhciTuningState::Delay {
                            cmd_index,
                            block_size,
                            remaining,
                            deadline_ms: now.saturating_add(u64::from(cmd_index == 19)),
                        };
                        return Ok(sdmmc_host::RequestProgress::RegisterPending {
                            retry_after: SDHCI_REGISTER_RETRY_DELAY,
                        });
                    }
                }
                if now >= deadline_ms {
                    return Err(sdmmc_host::Error::Timeout);
                }
            }
            SdhciTuningState::Delay {
                cmd_index,
                block_size,
                remaining,
                deadline_ms,
            } => {
                if now >= deadline_ms {
                    self.issue_tuning_block(cmd_index, block_size);
                    *state = SdhciTuningState::Wait {
                        cmd_index,
                        block_size,
                        remaining: remaining - 1,
                        deadline_ms: now.saturating_add(SDHCI_TUNING_TIMEOUT_MS),
                    };
                }
            }
        }
        Ok(sdmmc_host::RequestProgress::RegisterPending {
            retry_after: Duration::from_millis(1),
        })
    }

    fn issue_tuning_block(&mut self, cmd_index: u8, block_size: u16) {
        self.write_interrupt_status(NORMAL_INT_CLEAR_ALL, ERROR_INT_CLEAR_ALL);
        self.irq.state.begin_request();
        self.write_u16(REG_BLOCK_SIZE, block_size);
        self.write_u16(REG_BLOCK_COUNT, 1);
        self.write_u8(REG_TIMEOUT_CONTROL, 0x0e);
        // The controller consumes the tuning pattern itself; DMA stays off.
        self.write_u16(REG_TRANSFER_MODE, XFER_MODE_READ);
        self.write_u32(REG_ARGUMENT, 0);
        self.write_u16(
            REG_COMMAND,
            (u16::from(cmd_index) << 8)
                | CMD_RESP_LEN48
                | CMD_CRC_CHECK
                | CMD_INDEX_CHECK
                | CMD_DATA_PRESENT,
        );
    }
}
