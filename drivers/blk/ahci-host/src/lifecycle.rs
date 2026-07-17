use rdif_block::{
    ControllerEpoch, ControllerReady, DmaQuiesced, InitError, InitInput, InitPoll, InitSchedule,
    RecoveryCause,
};

use crate::{
    AhciConfig,
    irq::HostShared,
    registers::{
        CMD_CR, CMD_FR, CMD_FRE, CMD_ICC_ACTIVE, CMD_POD, CMD_ST, CMD_SUD, GHC_AE, GHC_HR, GHC_IE,
        HOST_GHC, HOST_IS, MAX_PORTS, PX_CLB, PX_CLBU, PX_CMD, PX_FB, PX_FBU, PX_IE, PX_IS,
        PX_SCTL, PX_SERR, PX_SSTS, PX_TFD, SCTL_DET_INIT, SCTL_DET_MASK, SCTL_DET_NONE,
        TFD_NOT_READY, read_port, write_port,
    },
};

pub(crate) struct AhciLifecycle {
    state: LifecycleState,
    last_epoch: ControllerEpoch,
}

impl AhciLifecycle {
    pub(crate) const fn running() -> Self {
        Self {
            state: LifecycleState::Running,
            last_epoch: ControllerEpoch::INITIAL,
        }
    }

    pub(crate) fn begin_dma_quiesce(
        &mut self,
        shared: &HostShared,
        epoch: ControllerEpoch,
        _cause: RecoveryCause,
    ) -> Result<(), InitError> {
        if !matches!(
            self.state,
            LifecycleState::Running | LifecycleState::GuestOwned
        ) || epoch <= self.last_epoch
        {
            return Err(InitError::InvalidState);
        }
        self.last_epoch = epoch;
        shared.set_irq_delivery_enabled(false);
        shared.mask_all_ports();
        for port in ready_port_indices(shared) {
            shared.port(port).set_online(false);
            let command = read_port(shared.registers(), port, PX_CMD);
            write_port(shared.registers(), port, PX_CMD, command & !CMD_ST);
        }
        let ghc = shared.registers().read32(HOST_GHC);
        shared.registers().write32(HOST_GHC, ghc & !GHC_IE);
        self.state = LifecycleState::StoppingCommand {
            epoch,
            deadline_ns: None,
        };
        Ok(())
    }

    pub(crate) fn poll_dma_quiesce(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        controller_cookie: usize,
        input: InitInput,
    ) -> InitPoll<DmaQuiesced> {
        match self.state {
            LifecycleState::StoppingCommand { epoch, deadline_ns } => {
                self.poll_command_stop(shared, config, input, epoch, deadline_ns)
            }
            LifecycleState::StoppingFis { epoch, deadline_ns } => {
                self.poll_fis_stop(shared, config, input, epoch, deadline_ns)
            }
            LifecycleState::ResettingForQuiesce { epoch, deadline_ns } => self
                .poll_reset_for_quiesce(
                    shared,
                    config,
                    controller_cookie,
                    input,
                    epoch,
                    deadline_ns,
                ),
            _ => InitPoll::Failed(InitError::InvalidState),
        }
    }

    pub(crate) fn enter_guest_owned(
        &mut self,
        controller_cookie: usize,
        quiesced: DmaQuiesced,
    ) -> Result<(), InitError> {
        let LifecycleState::Quiesced { epoch } = self.state else {
            return Err(InitError::InvalidState);
        };
        validate_proof(epoch, controller_cookie, &quiesced)?;
        self.state = LifecycleState::GuestOwned;
        Ok(())
    }

    pub(crate) fn begin_reinitialize(
        &mut self,
        shared: &HostShared,
        controller_cookie: usize,
        quiesced: DmaQuiesced,
    ) -> Result<(), InitError> {
        let LifecycleState::Quiesced { epoch } = self.state else {
            return Err(InitError::InvalidState);
        };
        validate_proof(epoch, controller_cookie, &quiesced)?;
        let ghc = shared.registers().read32(HOST_GHC);
        shared
            .registers()
            .write32(HOST_GHC, (ghc | GHC_AE | GHC_HR) & !GHC_IE);
        self.state = LifecycleState::ResettingForReinitialize {
            epoch,
            deadline_ns: None,
        };
        Ok(())
    }

    pub(crate) fn poll_reinitialize(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        controller_cookie: usize,
        input: InitInput,
    ) -> InitPoll<ControllerReady> {
        match self.state {
            LifecycleState::ResettingForReinitialize { epoch, deadline_ns } => {
                self.poll_reset_for_reinitialize(shared, config, input, epoch, deadline_ns)
            }
            LifecycleState::AssertingComreset {
                epoch,
                release_at_ns,
            } => self.poll_comreset_assertion(shared, config, input, epoch, release_at_ns),
            LifecycleState::WaitingLink { epoch, deadline_ns } => {
                self.poll_reinitialize_link(shared, config, input, epoch, deadline_ns)
            }
            LifecycleState::StartingFis { epoch, deadline_ns } => {
                self.poll_starting_fis(shared, config, input, epoch, deadline_ns)
            }
            LifecycleState::StartingPorts { epoch, deadline_ns } => self.poll_starting_ports(
                shared,
                config,
                controller_cookie,
                input,
                epoch,
                deadline_ns,
            ),
            _ => InitPoll::Failed(InitError::InvalidState),
        }
    }

    fn poll_command_stop(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        input: InitInput,
        epoch: ControllerEpoch,
        deadline_ns: Option<u64>,
    ) -> InitPoll<DmaQuiesced> {
        let deadline_ns =
            deadline_ns.unwrap_or_else(|| input.now_ns.saturating_add(config.port_stop_timeout_ns));
        if ready_port_indices(shared)
            .any(|port| read_port(shared.registers(), port, PX_CMD) & CMD_CR != 0)
        {
            if input.now_ns >= deadline_ns {
                return self.quiesce_failure();
            }
            self.state = LifecycleState::StoppingCommand {
                epoch,
                deadline_ns: Some(deadline_ns),
            };
            return InitPoll::Pending(status_check_schedule(input.now_ns, deadline_ns, config));
        }

        for port in ready_port_indices(shared) {
            let command = read_port(shared.registers(), port, PX_CMD);
            write_port(shared.registers(), port, PX_CMD, command & !CMD_FRE);
        }
        self.state = LifecycleState::StoppingFis {
            epoch,
            deadline_ns: Some(deadline_ns),
        };
        InitPoll::Pending(status_check_schedule(input.now_ns, deadline_ns, config))
    }

    fn poll_fis_stop(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        input: InitInput,
        epoch: ControllerEpoch,
        deadline_ns: Option<u64>,
    ) -> InitPoll<DmaQuiesced> {
        let deadline_ns =
            deadline_ns.unwrap_or_else(|| input.now_ns.saturating_add(config.port_stop_timeout_ns));
        if ready_port_indices(shared)
            .any(|port| read_port(shared.registers(), port, PX_CMD) & CMD_FR != 0)
        {
            if input.now_ns >= deadline_ns {
                return self.quiesce_failure();
            }
            self.state = LifecycleState::StoppingFis {
                epoch,
                deadline_ns: Some(deadline_ns),
            };
            return InitPoll::Pending(status_check_schedule(input.now_ns, deadline_ns, config));
        }

        let ghc = shared.registers().read32(HOST_GHC);
        shared
            .registers()
            .write32(HOST_GHC, (ghc | GHC_AE | GHC_HR) & !GHC_IE);
        let reset_deadline = input.now_ns.saturating_add(config.reset_timeout_ns);
        self.state = LifecycleState::ResettingForQuiesce {
            epoch,
            deadline_ns: reset_deadline,
        };
        InitPoll::Pending(status_check_schedule(input.now_ns, reset_deadline, config))
    }

    fn poll_reset_for_quiesce(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        controller_cookie: usize,
        input: InitInput,
        epoch: ControllerEpoch,
        deadline_ns: u64,
    ) -> InitPoll<DmaQuiesced> {
        if shared.registers().read32(HOST_GHC) & GHC_HR != 0 {
            if input.now_ns >= deadline_ns {
                return self.quiesce_failure();
            }
            self.state = LifecycleState::ResettingForQuiesce { epoch, deadline_ns };
            return InitPoll::Pending(status_check_schedule(input.now_ns, deadline_ns, config));
        }
        self.state = LifecycleState::Quiesced { epoch };
        InitPoll::Ready(unsafe {
            // SAFETY: runtime admission and OS IRQ actions are closed before
            // this lifecycle starts. Every ready port acknowledged CR=FR=0,
            // then the complete HBA reset acknowledged HR=0, so no retained
            // command list, received-FIS area, or request buffer is DMA-live.
            DmaQuiesced::new(epoch, controller_cookie)
        })
    }

    fn poll_reset_for_reinitialize(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        input: InitInput,
        epoch: ControllerEpoch,
        deadline_ns: Option<u64>,
    ) -> InitPoll<ControllerReady> {
        let deadline_ns =
            deadline_ns.unwrap_or_else(|| input.now_ns.saturating_add(config.reset_timeout_ns));
        if shared.registers().read32(HOST_GHC) & GHC_HR != 0 {
            if input.now_ns >= deadline_ns {
                return self.reinitialize_failure(shared);
            }
            self.state = LifecycleState::ResettingForReinitialize {
                epoch,
                deadline_ns: Some(deadline_ns),
            };
            return InitPoll::Pending(status_check_schedule(input.now_ns, deadline_ns, config));
        }

        let ghc = shared.registers().read32(HOST_GHC);
        shared
            .registers()
            .write32(HOST_GHC, (ghc | GHC_AE) & !GHC_IE);
        for port in ready_port_indices(shared) {
            shared.port(port).discard_stale_snapshots();
            shared.port(port).publish_epoch(epoch.get());
            write_port(shared.registers(), port, PX_IE, 0);
            let control = read_port(shared.registers(), port, PX_SCTL);
            write_port(
                shared.registers(),
                port,
                PX_SCTL,
                (control & !SCTL_DET_MASK) | SCTL_DET_INIT,
            );
        }
        let release_at_ns = input.now_ns.saturating_add(config.comreset_assert_ns);
        self.state = LifecycleState::AssertingComreset {
            epoch,
            release_at_ns,
        };
        InitPoll::Pending(InitSchedule::wait_until(release_at_ns))
    }

    fn poll_comreset_assertion(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        input: InitInput,
        epoch: ControllerEpoch,
        release_at_ns: u64,
    ) -> InitPoll<ControllerReady> {
        if input.now_ns < release_at_ns {
            self.state = LifecycleState::AssertingComreset {
                epoch,
                release_at_ns,
            };
            return InitPoll::Pending(InitSchedule::wait_until(release_at_ns));
        }
        for port in ready_port_indices(shared) {
            let control = read_port(shared.registers(), port, PX_SCTL);
            write_port(
                shared.registers(),
                port,
                PX_SCTL,
                (control & !SCTL_DET_MASK) | SCTL_DET_NONE,
            );
        }
        let deadline_ns = input.now_ns.saturating_add(config.link_timeout_ns);
        self.state = LifecycleState::WaitingLink { epoch, deadline_ns };
        InitPoll::Pending(status_check_schedule(input.now_ns, deadline_ns, config))
    }

    fn poll_reinitialize_link(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        input: InitInput,
        epoch: ControllerEpoch,
        deadline_ns: u64,
    ) -> InitPoll<ControllerReady> {
        if ready_port_indices(shared)
            .any(|port| read_port(shared.registers(), port, PX_SSTS) & 0xf != 3)
        {
            if input.now_ns >= deadline_ns {
                return self.reinitialize_failure(shared);
            }
            self.state = LifecycleState::WaitingLink { epoch, deadline_ns };
            return InitPoll::Pending(status_check_schedule(input.now_ns, deadline_ns, config));
        }

        for port in ready_port_indices(shared) {
            let Some((command_list, received_fis)) = shared.port(port).dma_bases() else {
                return self.reinitialize_failure(shared);
            };
            write_dma_base(shared, port, PX_CLB, PX_CLBU, command_list);
            write_dma_base(shared, port, PX_FB, PX_FBU, received_fis);
            let command = read_port(shared.registers(), port, PX_CMD);
            write_port(
                shared.registers(),
                port,
                PX_CMD,
                (command | CMD_FRE | CMD_SUD | CMD_POD | CMD_ICC_ACTIVE) & !CMD_ST,
            );
        }
        let start_deadline = input.now_ns.saturating_add(config.port_stop_timeout_ns);
        self.state = LifecycleState::StartingFis {
            epoch,
            deadline_ns: start_deadline,
        };
        InitPoll::Pending(status_check_schedule(input.now_ns, start_deadline, config))
    }

    fn poll_starting_fis(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        input: InitInput,
        epoch: ControllerEpoch,
        deadline_ns: u64,
    ) -> InitPoll<ControllerReady> {
        if ready_port_indices(shared)
            .any(|port| read_port(shared.registers(), port, PX_CMD) & CMD_FR == 0)
        {
            if input.now_ns >= deadline_ns {
                return self.reinitialize_failure(shared);
            }
            self.state = LifecycleState::StartingFis { epoch, deadline_ns };
            return InitPoll::Pending(status_check_schedule(input.now_ns, deadline_ns, config));
        }

        for port in ready_port_indices(shared) {
            let command = read_port(shared.registers(), port, PX_CMD);
            write_port(shared.registers(), port, PX_CMD, command | CMD_ST);
        }
        let command_deadline = input.now_ns.saturating_add(config.link_timeout_ns);
        self.state = LifecycleState::StartingPorts {
            epoch,
            deadline_ns: command_deadline,
        };
        InitPoll::Pending(status_check_schedule(
            input.now_ns,
            command_deadline,
            config,
        ))
    }

    fn poll_starting_ports(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        controller_cookie: usize,
        input: InitInput,
        epoch: ControllerEpoch,
        deadline_ns: u64,
    ) -> InitPoll<ControllerReady> {
        let ready = ready_port_indices(shared).all(|port| {
            read_port(shared.registers(), port, PX_SSTS) & 0xf == 3
                && read_port(shared.registers(), port, PX_CMD) & (CMD_CR | CMD_FR)
                    == (CMD_CR | CMD_FR)
                && read_port(shared.registers(), port, PX_TFD) & TFD_NOT_READY == 0
        });
        if !ready {
            if input.now_ns >= deadline_ns {
                return self.reinitialize_failure(shared);
            }
            self.state = LifecycleState::StartingPorts { epoch, deadline_ns };
            return InitPoll::Pending(status_check_schedule(input.now_ns, deadline_ns, config));
        }

        clear_reinitialize_irq_state(shared);
        for port in ready_port_indices(shared) {
            shared.port(port).set_online(true);
        }
        self.state = LifecycleState::Running;
        InitPoll::Ready(unsafe {
            // SAFETY: the retained bases and per-port epoch were restored only
            // after HBA reset. Every published port has link, command engine,
            // FIS receive engine, and task-file readiness. Device IRQs remain
            // masked until the runtime has restored and published its action.
            ControllerReady::new(epoch, controller_cookie)
        })
    }

    fn quiesce_failure<T>(&mut self) -> InitPoll<T> {
        self.state = LifecycleState::Failed;
        InitPoll::Failed(InitError::Hardware(
            "AHCI controller could not prove DMA quiescence",
        ))
    }

    fn reinitialize_failure<T>(&mut self, shared: &HostShared) -> InitPoll<T> {
        shared.mask_all_ports();
        for port in ready_port_indices(shared) {
            shared.port(port).set_online(false);
        }
        self.state = LifecycleState::Failed;
        InitPoll::Failed(InitError::Hardware(
            "AHCI controller reinitialization failed",
        ))
    }
}

#[derive(Clone, Copy)]
enum LifecycleState {
    Running,
    GuestOwned,
    StoppingCommand {
        epoch: ControllerEpoch,
        deadline_ns: Option<u64>,
    },
    StoppingFis {
        epoch: ControllerEpoch,
        deadline_ns: Option<u64>,
    },
    ResettingForQuiesce {
        epoch: ControllerEpoch,
        deadline_ns: u64,
    },
    Quiesced {
        epoch: ControllerEpoch,
    },
    ResettingForReinitialize {
        epoch: ControllerEpoch,
        deadline_ns: Option<u64>,
    },
    AssertingComreset {
        epoch: ControllerEpoch,
        release_at_ns: u64,
    },
    WaitingLink {
        epoch: ControllerEpoch,
        deadline_ns: u64,
    },
    StartingFis {
        epoch: ControllerEpoch,
        deadline_ns: u64,
    },
    StartingPorts {
        epoch: ControllerEpoch,
        deadline_ns: u64,
    },
    Failed,
}

fn ready_port_indices(shared: &HostShared) -> impl Iterator<Item = usize> + '_ {
    let bits = shared.ready_ports();
    (0..MAX_PORTS).filter(move |port| bits & (1 << port) != 0)
}

fn validate_proof(
    epoch: ControllerEpoch,
    controller_cookie: usize,
    proof: &DmaQuiesced,
) -> Result<(), InitError> {
    if proof.epoch() != epoch || proof.controller_cookie() != controller_cookie {
        return Err(InitError::InvalidState);
    }
    Ok(())
}

fn write_dma_base(shared: &HostShared, port: usize, low: usize, high: usize, value: u64) {
    write_port(shared.registers(), port, low, value as u32);
    write_port(shared.registers(), port, high, (value >> 32) as u32);
}

fn clear_reinitialize_irq_state(shared: &HostShared) {
    // The controller-global source and every ready PxIE are still masked, and
    // the runtime has not republished queue admission. This lifecycle phase is
    // therefore the exclusive non-IRQ owner allowed to discard link/reset
    // latches before the normal IRQ endpoint resumes destructive W1C access.
    for port in ready_port_indices(shared) {
        let sata_error = read_port(shared.registers(), port, PX_SERR);
        if sata_error != 0 {
            write_port(shared.registers(), port, PX_SERR, sata_error);
        }
        let status = read_port(shared.registers(), port, PX_IS);
        if status != 0 {
            write_port(shared.registers(), port, PX_IS, status);
        }
        shared.port(port).discard_stale_snapshots();
    }

    let host_status = shared.registers().read32(HOST_IS) & shared.implemented_ports();
    if host_status != 0 {
        shared.registers().write32(HOST_IS, host_status);
    }
}

fn status_check_schedule(now_ns: u64, deadline_ns: u64, config: AhciConfig) -> InitSchedule {
    InitSchedule::wait_until(
        now_ns
            .saturating_add(config.status_check_ns)
            .min(deadline_ns),
    )
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;

    use super::*;
    use crate::registers::{
        GHC_HR, HOST_IS, IRQ_PHY_READY_CHANGE, MMIO_REQUIRED_SIZE, PX_IS, PX_SERR, RegisterIo,
        port_offset, tests_support::FakeRegisters,
    };

    #[test]
    fn quiescence_proof_requires_both_engines_and_hba_reset_to_stop() {
        let (registers, shared) = running_controller();
        let cookie = Arc::as_ptr(&shared).expose_provenance();
        let epoch = ControllerEpoch::new(2);
        let mut lifecycle = AhciLifecycle::running();

        lifecycle
            .begin_dma_quiesce(&shared, epoch, RecoveryCause::Handoff)
            .unwrap();
        assert!(matches!(
            lifecycle.poll_dma_quiesce(&shared, test_config(), cookie, InitInput::at(0)),
            InitPoll::Pending(_)
        ));

        registers.set(port_offset(0, PX_CMD), CMD_FR | CMD_FRE);
        assert!(matches!(
            lifecycle.poll_dma_quiesce(&shared, test_config(), cookie, InitInput::at(1)),
            InitPoll::Pending(_)
        ));
        assert!(matches!(
            lifecycle.poll_dma_quiesce(&shared, test_config(), cookie, InitInput::at(2)),
            InitPoll::Pending(_)
        ));

        registers.set(port_offset(0, PX_CMD), 0);
        assert!(matches!(
            lifecycle.poll_dma_quiesce(&shared, test_config(), cookie, InitInput::at(3)),
            InitPoll::Pending(_)
        ));
        assert_ne!(registers.read32(HOST_GHC) & GHC_HR, 0);
        assert!(matches!(
            lifecycle.poll_dma_quiesce(&shared, test_config(), cookie, InitInput::at(4)),
            InitPoll::Pending(_)
        ));

        registers.set(HOST_GHC, 0);
        let InitPoll::Ready(proof) =
            lifecycle.poll_dma_quiesce(&shared, test_config(), cookie, InitInput::at(5))
        else {
            panic!("all DMA owners have acknowledged stop");
        };
        assert_eq!(proof.epoch(), epoch);
        assert_eq!(proof.controller_cookie(), cookie);
    }

    #[test]
    fn quiesce_rejects_an_epoch_that_is_not_newer_than_published_queues() {
        let (registers, shared) = running_controller();
        registers.clear_access_log();
        let mut lifecycle = AhciLifecycle::running();

        let result =
            lifecycle.begin_dma_quiesce(&shared, ControllerEpoch::INITIAL, RecoveryCause::Handoff);

        assert_eq!(result, Err(InitError::InvalidState));
        assert!(shared.port(0).is_online());
        assert!(registers.writes().is_empty());
    }

    #[test]
    fn reinitialize_keeps_device_irq_masked_until_runtime_restores_actions() {
        let (registers, shared) = running_controller();
        shared.port(0).publish_dma_bases(0, 0);
        let cookie = Arc::as_ptr(&shared).expose_provenance();
        let epoch = ControllerEpoch::new(3);
        let mut lifecycle = AhciLifecycle::running();
        let proof = quiesce(&mut lifecycle, &registers, &shared, cookie, epoch);

        lifecycle
            .begin_reinitialize(&shared, cookie, proof)
            .unwrap();
        registers.set(HOST_GHC, 0);
        assert!(matches!(
            lifecycle.poll_reinitialize(&shared, test_config(), cookie, InitInput::at(10),),
            InitPoll::Pending(_)
        ));
        assert_eq!(registers.read32(HOST_GHC) & GHC_IE, 0);
        assert_eq!(read_port(registers.as_ref(), 0, PX_IE), 0);

        registers.set(port_offset(0, PX_SSTS), 3);
        registers.set(port_offset(0, PX_CMD), CMD_FR);
        registers.set(port_offset(0, PX_TFD), 0);
        assert!(matches!(
            lifecycle.poll_reinitialize(&shared, test_config(), cookie, InitInput::at(11),),
            InitPoll::Pending(_)
        ));
        assert_eq!(registers.read32(HOST_GHC) & GHC_IE, 0);
        assert_eq!(read_port(registers.as_ref(), 0, PX_IE), 0);

        registers.set(port_offset(0, PX_CMD), CMD_CR | CMD_FR);
        assert!(matches!(
            lifecycle.poll_reinitialize(&shared, test_config(), cookie, InitInput::at(12),),
            InitPoll::Pending(_)
        ));
        assert_eq!(registers.read32(HOST_GHC) & GHC_IE, 0);
        assert_eq!(read_port(registers.as_ref(), 0, PX_IE), 0);

        assert!(matches!(
            lifecycle.poll_reinitialize(&shared, test_config(), cookie, InitInput::at(13),),
            InitPoll::Pending(_)
        ));
        registers.set(port_offset(0, PX_CMD), CMD_CR | CMD_FR);
        registers.set(port_offset(0, PX_IS), IRQ_PHY_READY_CHANGE);
        registers.set(port_offset(0, PX_SERR), 0x20);
        registers.set(HOST_IS, 1);
        assert!(matches!(
            lifecycle.poll_reinitialize(&shared, test_config(), cookie, InitInput::at(14),),
            InitPoll::Ready(_)
        ));
        assert_eq!(registers.read32(HOST_GHC) & GHC_IE, 0);
        assert_eq!(read_port(registers.as_ref(), 0, PX_IE), 0);
        assert_eq!(read_port(registers.as_ref(), 0, PX_IS), 0);
        assert_eq!(read_port(registers.as_ref(), 0, PX_SERR), 0);
        assert_eq!(registers.read32(HOST_IS), 0);
        assert!(shared.port(0).is_online());
    }

    fn running_controller() -> (Arc<FakeRegisters>, Arc<HostShared>) {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let shared = HostShared::new(registers.shared());
        shared.publish_implemented_ports(1);
        shared.publish_ready_port(0);
        shared.set_irq_delivery_enabled(true);
        registers.set(port_offset(0, PX_CMD), CMD_ST | CMD_FRE | CMD_CR | CMD_FR);
        registers.set(
            port_offset(0, PX_IE),
            crate::registers::DEFAULT_PORT_IRQ_MASK,
        );
        registers.set(port_offset(0, PX_IS), 0);
        registers.set(HOST_IS, 0);
        (registers, shared)
    }

    fn quiesce(
        lifecycle: &mut AhciLifecycle,
        registers: &FakeRegisters,
        shared: &HostShared,
        cookie: usize,
        epoch: ControllerEpoch,
    ) -> DmaQuiesced {
        lifecycle
            .begin_dma_quiesce(shared, epoch, RecoveryCause::Handoff)
            .unwrap();
        registers.set(port_offset(0, PX_CMD), 0);
        assert!(matches!(
            lifecycle.poll_dma_quiesce(shared, test_config(), cookie, InitInput::at(0)),
            InitPoll::Pending(_)
        ));
        assert!(matches!(
            lifecycle.poll_dma_quiesce(shared, test_config(), cookie, InitInput::at(1)),
            InitPoll::Pending(_)
        ));
        registers.set(HOST_GHC, 0);
        let InitPoll::Ready(proof) =
            lifecycle.poll_dma_quiesce(shared, test_config(), cookie, InitInput::at(2))
        else {
            panic!("test controller must become quiescent");
        };
        proof
    }

    const fn test_config() -> AhciConfig {
        AhciConfig {
            irq_source_id: 0,
            ownership_timeout_ns: 100,
            reset_timeout_ns: 100,
            port_stop_timeout_ns: 100,
            comreset_assert_ns: 1,
            link_timeout_ns: 100,
            command_timeout_ns: 100,
            status_check_ns: 7,
        }
    }
}
