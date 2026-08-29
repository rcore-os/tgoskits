use alloc::sync::Arc;
use core::time::Duration;

use rdif_eth::{
    NetError, NetIrqSnapshot, NetOwnerStartup, NetOwnerStartupProgress, NetPollIrqControl,
    NetRearmResult,
};
use ringbuf::traits::{Consumer, Producer};
use sdmmc_protocol::sdio::SdMmcIrqHost;

use crate::rdif::{
    device::{OwnerReceiver, OwnerSender, WifiProgressSignal},
    error::AicRdifError,
    owner::{AicOwner, OwnerProgress, OwnerWait},
};

pub(super) struct AicOwnerStartup<H: SdMmcIrqHost + Send + 'static> {
    owner: Option<AicOwner<H>>,
    owner_sender: OwnerSender<H>,
    startup_delay: Duration,
    startup_timeout: Duration,
    delay_deadline: Option<u64>,
    timeout_deadline: Option<u64>,
    started: bool,
}

impl<H: SdMmcIrqHost + Send + 'static> AicOwnerStartup<H> {
    pub(super) fn new(
        owner: AicOwner<H>,
        owner_sender: OwnerSender<H>,
        startup_delay: Duration,
        startup_timeout: Duration,
    ) -> Self {
        Self {
            owner: Some(owner),
            owner_sender,
            startup_delay,
            startup_timeout,
            delay_deadline: None,
            timeout_deadline: None,
            started: false,
        }
    }

    fn finish(&mut self, progress: OwnerProgress) -> Result<NetOwnerStartupProgress, NetError> {
        let timeout_deadline = self.timeout_deadline.ok_or(NetError::InvalidParts)?;
        match progress {
            OwnerProgress::Ready => {
                let owner_sender = &mut self.owner_sender;
                transfer_owner(&mut self.owner, |owner| owner_sender.try_push(owner).err())
                    .map_err(|error| match error {
                        TransferOwnerError::Missing => NetError::Stopped,
                        TransferOwnerError::Rejected => NetError::InvalidParts,
                    })?;
                Ok(NetOwnerStartupProgress::Ready)
            }
            OwnerProgress::Wait(wait) => Ok(startup_wait(wait, timeout_deadline)),
        }
    }
}

const fn startup_wait(wait: OwnerWait, timeout_deadline: u64) -> NetOwnerStartupProgress {
    match wait {
        OwnerWait::Interrupt => NetOwnerStartupProgress::WaitForInterruptUntil {
            deadline_nanos: timeout_deadline,
        },
        OwnerWait::InterruptUntil(deadline_nanos) => {
            NetOwnerStartupProgress::WaitForInterruptUntil {
                deadline_nanos: if deadline_nanos < timeout_deadline {
                    deadline_nanos
                } else {
                    timeout_deadline
                },
            }
        }
        OwnerWait::RetryAt(deadline_nanos) => NetOwnerStartupProgress::RetryAt {
            deadline_nanos: if deadline_nanos < timeout_deadline {
                deadline_nanos
            } else {
                timeout_deadline
            },
        },
    }
}

impl<H: SdMmcIrqHost + Send + 'static> NetOwnerStartup for AicOwnerStartup<H> {
    fn start(&mut self, now_nanos: u64) -> Result<NetOwnerStartupProgress, NetError> {
        let timeout = u64::try_from(self.startup_timeout.as_nanos()).unwrap_or(u64::MAX);
        self.timeout_deadline = Some(now_nanos.saturating_add(timeout));
        log::info!(
            "[wifi] AIC owner startup timeout armed for {:?}",
            self.startup_timeout
        );
        if !self.startup_delay.is_zero() {
            let delay = u64::try_from(self.startup_delay.as_nanos()).unwrap_or(u64::MAX);
            let delay_deadline = now_nanos.saturating_add(delay);
            self.delay_deadline = Some(delay_deadline);
            log::info!(
                "[wifi] AIC owner startup waiting {:?} for SDIO1 reset settle",
                self.startup_delay
            );
            return Ok(NetOwnerStartupProgress::RetryAt {
                deadline_nanos: delay_deadline
                    .min(self.timeout_deadline.ok_or(NetError::InvalidParts)?),
            });
        }
        self.started = true;
        log::info!("[wifi] AIC owner starting SDIO card enumeration");
        let progress = self
            .owner
            .as_mut()
            .ok_or(NetError::Stopped)?
            .start(now_nanos)
            .map_err(NetError::from)?;
        self.finish(progress)
    }

    fn advance(&mut self, now_nanos: u64) -> Result<NetOwnerStartupProgress, NetError> {
        let timeout_deadline = self.timeout_deadline.ok_or(NetError::InvalidParts)?;
        if now_nanos >= timeout_deadline {
            let (card_protocol_ready, irq_sequence, irq_pending, completion_pending) = self
                .owner
                .as_mut()
                .ok_or(NetError::Stopped)?
                .startup_diagnostic();
            log::error!(
                "[wifi] AIC owner startup timed out (enumeration_started={}, \
                 card_protocol_ready={}, irq_sequence={}, irq_pending={}, \
                 completion_pending={completion_pending:?})",
                self.started,
                card_protocol_ready,
                irq_sequence,
                irq_pending
            );
            shutdown_owner(&mut self.owner, |owner| {
                owner.shutdown().map_err(NetError::from)
            })?;
            return Err(NetError::from(AicRdifError::StartupTimeout {
                enumeration_started: self.started,
                card_protocol_ready,
                irq_sequence,
                irq_pending,
                completion_pending,
            }));
        }
        let owner = self.owner.as_mut().ok_or(NetError::Stopped)?;
        let progress = if self.started {
            owner.advance(now_nanos)
        } else {
            let deadline = self.delay_deadline.ok_or(NetError::InvalidParts)?;
            if now_nanos < deadline {
                return Ok(NetOwnerStartupProgress::RetryAt {
                    deadline_nanos: deadline,
                });
            }
            self.started = true;
            log::info!("[wifi] AIC owner reset settle complete; starting SDIO card enumeration");
            owner.start(now_nanos)
        }
        .map_err(NetError::from)?;
        self.finish(progress)
    }

    fn cancel(&mut self) -> Result<(), NetError> {
        shutdown_owner(&mut self.owner, |owner| {
            owner.shutdown().map_err(NetError::from)
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransferOwnerError {
    Missing,
    Rejected,
}

fn transfer_owner<T>(
    slot: &mut Option<T>,
    transfer: impl FnOnce(T) -> Option<T>,
) -> Result<(), TransferOwnerError> {
    let owner = slot.take().ok_or(TransferOwnerError::Missing)?;
    match transfer(owner) {
        None => Ok(()),
        Some(owner) => {
            *slot = Some(owner);
            Err(TransferOwnerError::Rejected)
        }
    }
}

fn shutdown_owner<T, E>(
    slot: &mut Option<T>,
    shutdown: impl FnOnce(&mut T) -> Result<(), E>,
) -> Result<(), E> {
    let Some(mut owner) = slot.take() else {
        return Ok(());
    };
    if let Err(error) = shutdown(&mut owner) {
        // Shutdown failure cannot prove that hardware stopped touching the
        // owner's backing, so preserve the entire ownership domain.
        core::mem::forget(owner);
        return Err(error);
    }
    Ok(())
}

pub(super) struct AicPollIrqControl<H: SdMmcIrqHost + Send + 'static> {
    owner: Option<AicOwner<H>>,
    owner_receiver: OwnerReceiver<H>,
    wifi_progress_signal: Arc<WifiProgressSignal>,
}

impl<H: SdMmcIrqHost + Send + 'static> AicPollIrqControl<H> {
    pub(super) fn new(
        owner_receiver: OwnerReceiver<H>,
        wifi_progress_signal: Arc<WifiProgressSignal>,
    ) -> Self {
        Self {
            owner: None,
            owner_receiver,
            wifi_progress_signal,
        }
    }

    fn owner(&mut self) -> Result<&mut AicOwner<H>, NetError> {
        if self.owner.is_none() {
            self.owner = self.owner_receiver.try_pop();
        }
        self.owner.as_mut().ok_or(NetError::Stopped)
    }
}

impl<H: SdMmcIrqHost + Send + 'static> NetPollIrqControl for AicPollIrqControl<H> {
    fn quiesce(&mut self) -> Result<(), NetError> {
        self.owner()?.quiesce().map_err(NetError::from)
    }

    fn shutdown(&mut self) -> Result<(), NetError> {
        self.owner()?.shutdown().map_err(NetError::from)
    }

    fn rearm_and_check(&mut self, now_nanos: u64) -> Result<NetRearmResult, NetError> {
        let (progress, pending) = self
            .owner()?
            .rearm_and_advance(now_nanos)
            .map_err(NetError::from)?;
        Ok(owner_rearm_result(
            progress,
            pending || self.wifi_progress_signal.has_pending(),
        ))
    }
}

const fn owner_rearm_result(progress: OwnerProgress, pending: bool) -> NetRearmResult {
    if pending {
        NetRearmResult::WorkPending(NetIrqSnapshot::all_queue_work())
    } else if let OwnerProgress::Wait(
        OwnerWait::RetryAt(deadline_nanos) | OwnerWait::InterruptUntil(deadline_nanos),
    ) = progress
    {
        NetRearmResult::RetryAt { deadline_nanos }
    } else {
        NetRearmResult::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_owner_transfer_restores_the_unique_owner() {
        let mut owner = Some(7_u8);

        assert_eq!(
            transfer_owner(&mut owner, Some),
            Err(TransferOwnerError::Rejected)
        );
        assert_eq!(owner, Some(7));
    }

    #[test]
    fn shutdown_consumes_owner_and_is_idempotent() {
        let mut owner = Some(());
        let mut shutdown_count = 0;

        shutdown_owner(&mut owner, |_| {
            shutdown_count += 1;
            Ok::<_, ()>(())
        })
        .unwrap();
        shutdown_owner(&mut owner, |_| {
            shutdown_count += 1;
            Ok::<_, ()>(())
        })
        .unwrap();

        assert_eq!(shutdown_count, 1);
    }

    #[test]
    fn interrupt_deadline_rearms_card_irq_and_schedules_owner_retry() {
        assert_eq!(
            owner_rearm_result(OwnerProgress::Wait(OwnerWait::InterruptUntil(41)), false),
            NetRearmResult::RetryAt { deadline_nanos: 41 }
        );
    }

    #[test]
    fn startup_retry_never_extends_the_end_to_end_timeout() {
        assert_eq!(
            startup_wait(OwnerWait::RetryAt(101), 41),
            NetOwnerStartupProgress::RetryAt { deadline_nanos: 41 }
        );
    }
}
