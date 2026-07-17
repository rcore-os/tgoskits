use alloc::vec::Vec;
use core::{mem, num::NonZeroUsize};

use dma_api::{CpuDmaBuffer, DeviceDma, DmaDirection, InFlightDma};
use rdif_block::{IdList, InitError, InitInput, InitPoll, InitSchedule};

use crate::{
    AhciConfig,
    ata::AtaDevice,
    command::PortCommandMemory,
    irq::HostShared,
    queue::{ReadyPort, freeze_port},
    registers::{
        BOHC_BB, BOHC_BOS, BOHC_OOS, CAP_S64A, CAP2_BOH, CMD_CR, CMD_FR, CMD_FRE, CMD_ICC_ACTIVE,
        CMD_POD, CMD_ST, CMD_SUD, GHC_AE, GHC_HR, GHC_IE, HOST_BOHC, HOST_CAP, HOST_CAP2, HOST_GHC,
        HOST_IS, HOST_PI, PX_CI, PX_CMD, PX_IE, PX_IS, PX_SCTL, PX_SERR, PX_SSTS, PX_TFD,
        SCTL_DET_INIT, SCTL_DET_MASK, SCTL_DET_NONE, TFD_NOT_READY, read_port, write_port,
    },
};

const IDENTIFY_BYTES: usize = 512;
const COMMAND_SLOT: usize = 0;

/// Externally observable phase of AHCI discovery-to-ready initialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerInitState {
    Discovered,
    AcquiringOwnership,
    Resetting,
    ConfiguringPorts,
    Identifying,
    Ready,
    Failed,
}

pub(crate) struct AhciInitialization {
    state: InitState,
}

impl AhciInitialization {
    pub(crate) const fn discovered() -> Self {
        Self {
            state: InitState::Discovered,
        }
    }

    #[cfg(test)]
    pub(crate) fn mark_ready_for_test(&mut self) {
        self.state = InitState::Ready;
    }

    pub(crate) fn state(&self) -> ControllerInitState {
        match self.state {
            InitState::Discovered => ControllerInitState::Discovered,
            InitState::AcquiringOwnership { .. } => ControllerInitState::AcquiringOwnership,
            InitState::Resetting { .. } => ControllerInitState::Resetting,
            InitState::Scanning(_)
            | InitState::StoppingCommand(_)
            | InitState::StoppingFis(_)
            | InitState::AssertingComreset(_)
            | InitState::WaitingLink(_)
            | InitState::StartingFis { .. }
            | InitState::StartingEngine { .. }
            | InitState::AbortCommand(_)
            | InitState::AbortFis(_) => ControllerInitState::ConfiguringPorts,
            InitState::WaitingIdentify(_) => ControllerInitState::Identifying,
            InitState::Ready => ControllerInitState::Ready,
            InitState::Failed | InitState::Transition => ControllerInitState::Failed,
        }
    }

    /// Retains every DMA object whose engine-stop proof was not completed.
    ///
    /// This is the fail-closed destructor path for a caller that abandons an
    /// in-progress controller. It returns whether live DMA backing had to be
    /// quarantined so the controller owner can retain its MMIO/shared state.
    pub(crate) fn quarantine_owned_dma(&mut self) -> bool {
        let state = mem::replace(&mut self.state, InitState::Failed);
        match state {
            InitState::StartingFis { command_memory, .. }
            | InitState::StartingEngine { command_memory, .. } => {
                mem::forget(command_memory);
                true
            }
            InitState::WaitingIdentify(IdentifyCommand {
                command_memory,
                dma,
                ..
            })
            | InitState::AbortCommand(AbortingPort {
                command_memory,
                dma: Some(dma),
                ..
            })
            | InitState::AbortFis(AbortingPort {
                command_memory,
                dma: Some(dma),
                ..
            }) => {
                let _quarantined = dma.quarantine();
                mem::forget(command_memory);
                true
            }
            InitState::AbortCommand(AbortingPort {
                command_memory,
                dma: None,
                ..
            })
            | InitState::AbortFis(AbortingPort {
                command_memory,
                dma: None,
                ..
            }) => {
                mem::forget(command_memory);
                true
            }
            _ => false,
        }
    }

    pub(crate) fn poll(
        &mut self,
        shared: &HostShared,
        dma: &DeviceDma,
        config: AhciConfig,
        ready_ports: &mut Vec<Option<ReadyPort>>,
        input: InitInput,
    ) -> InitPoll<()> {
        let state = mem::replace(&mut self.state, InitState::Transition);
        let progress = match state {
            InitState::Discovered => self.begin_ownership(shared, config, input),
            InitState::AcquiringOwnership { deadline_ns } => {
                self.poll_ownership(shared, config, input, deadline_ns)
            }
            InitState::Resetting { deadline_ns } => {
                self.poll_reset(shared, config, input, deadline_ns)
            }
            InitState::Scanning(scan) => {
                self.scan_next_port(shared, config, ready_ports, input, scan)
            }
            InitState::StoppingCommand(cursor) => {
                self.poll_command_stop(shared, config, input, cursor)
            }
            InitState::StoppingFis(cursor) => self.poll_fis_stop(shared, config, input, cursor),
            InitState::AssertingComreset(cursor) => {
                self.poll_comreset_assertion(shared, config, input, cursor)
            }
            InitState::WaitingLink(cursor) => self.poll_link(shared, dma, config, input, cursor),
            InitState::StartingFis {
                cursor,
                command_memory,
            } => self.poll_fis_start(shared, config, input, cursor, command_memory),
            InitState::StartingEngine {
                cursor,
                command_memory,
            } => self.poll_engine_start(shared, dma, config, input, cursor, command_memory),
            InitState::WaitingIdentify(command) => {
                self.poll_identify(shared, config, ready_ports, input, command)
            }
            InitState::AbortCommand(abort) => self.poll_abort_command(shared, config, input, abort),
            InitState::AbortFis(abort) => self.poll_abort_fis(shared, config, input, abort),
            InitState::Ready => {
                self.state = InitState::Ready;
                InitPoll::Ready(())
            }
            InitState::Failed | InitState::Transition => self.fail(InitError::InvalidState),
        };
        debug_assert!(!matches!(self.state, InitState::Transition));
        progress
    }

    fn begin_reset(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        input: InitInput,
    ) -> InitPoll<()> {
        if !shared.initial_handler_live() || !shared.irq_delivery_enabled() {
            return self.fail(InitError::MissingInterrupt);
        }
        let ghc = shared.registers().read32(HOST_GHC);
        // HBA reset clears the controller-global interrupt enable. Keep it
        // masked while reset is in flight; `poll_reset` may restore it only
        // because the runtime permit above proves the init action is live.
        shared
            .registers()
            .write32(HOST_GHC, (ghc | GHC_AE | GHC_HR) & !GHC_IE);
        let deadline_ns = input.now_ns.saturating_add(config.reset_timeout_ns);
        self.state = InitState::Resetting { deadline_ns };
        InitPoll::Pending(status_check_schedule(input.now_ns, deadline_ns, config))
    }

    fn begin_ownership(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        input: InitInput,
    ) -> InitPoll<()> {
        if !shared.initial_handler_live() || !shared.irq_delivery_enabled() {
            return self.fail(InitError::MissingInterrupt);
        }
        if shared.registers().read32(HOST_CAP2) & CAP2_BOH == 0 {
            return self.begin_reset(shared, config, input);
        }

        let ownership = shared.registers().read32(HOST_BOHC);
        shared.registers().write32(HOST_BOHC, ownership | BOHC_OOS);
        let deadline_ns = input.now_ns.saturating_add(config.ownership_timeout_ns);
        self.state = InitState::AcquiringOwnership { deadline_ns };
        InitPoll::Pending(status_check_schedule(input.now_ns, deadline_ns, config))
    }

    fn poll_ownership(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        input: InitInput,
        deadline_ns: u64,
    ) -> InitPoll<()> {
        let ownership = shared.registers().read32(HOST_BOHC);
        if ownership & (BOHC_BOS | BOHC_BB) == 0 {
            return self.begin_reset(shared, config, input);
        }
        if input.now_ns >= deadline_ns {
            return self.fail(InitError::Hardware(
                "AHCI firmware ownership handoff timed out",
            ));
        }
        self.state = InitState::AcquiringOwnership { deadline_ns };
        InitPoll::Pending(status_check_schedule(input.now_ns, deadline_ns, config))
    }

    fn poll_reset(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        input: InitInput,
        deadline_ns: u64,
    ) -> InitPoll<()> {
        let ghc = shared.registers().read32(HOST_GHC);
        if ghc & GHC_HR != 0 {
            if input.now_ns >= deadline_ns {
                return self.fail(InitError::TimedOut);
            }
            self.state = InitState::Resetting { deadline_ns };
            return InitPoll::Pending(status_check_schedule(input.now_ns, deadline_ns, config));
        }

        if !shared.irq_delivery_enabled() {
            return self.fail(InitError::MissingInterrupt);
        }
        let cap = shared.registers().read32(HOST_CAP);
        let ports = shared.registers().read32(HOST_PI);
        if ports == 0 {
            return self.fail(InitError::Hardware("AHCI controller implements no ports"));
        }
        shared.publish_implemented_ports(ports);
        // The hard-IRQ endpoint filters HOST_IS through the published port
        // inventory. Mask each discovered source before opening the global
        // gate so no interrupt can observe a temporarily empty inventory or
        // inherited per-port enable state.
        shared.mask_all_ports();
        shared.registers().write32(HOST_GHC, ghc | GHC_AE | GHC_IE);
        self.state = InitState::Scanning(PortScan {
            cap,
            ports,
            next: 0,
        });
        InitPoll::Pending(InitSchedule::immediate())
    }

    fn scan_next_port(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        ready_ports: &[Option<ReadyPort>],
        input: InitInput,
        scan: PortScan,
    ) -> InitPoll<()> {
        let Some(port) = (scan.next..u32::BITS as usize).find(|port| scan.ports & (1 << port) != 0)
        else {
            if ready_ports.iter().all(Option::is_none) {
                return self.fail(InitError::Hardware("AHCI controller found no ATA disk"));
            }
            self.state = InitState::Ready;
            return InitPoll::Ready(());
        };

        freeze_port(shared, port);
        let command = read_port(shared.registers(), port, PX_CMD);
        write_port(shared.registers(), port, PX_CMD, command & !CMD_ST);
        let deadline_ns = input.now_ns.saturating_add(config.port_stop_timeout_ns);
        self.state = InitState::StoppingCommand(PortCursor {
            scan: PortScan {
                next: port + 1,
                ..scan
            },
            port,
            deadline_ns,
        });
        InitPoll::Pending(status_check_schedule(input.now_ns, deadline_ns, config))
    }

    fn poll_command_stop(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        input: InitInput,
        cursor: PortCursor,
    ) -> InitPoll<()> {
        let command = read_port(shared.registers(), cursor.port, PX_CMD);
        if command & CMD_CR != 0 {
            if input.now_ns >= cursor.deadline_ns {
                return self.fail(InitError::Hardware("AHCI command engine did not stop"));
            }
            self.state = InitState::StoppingCommand(cursor);
            return InitPoll::Pending(status_check_schedule(
                input.now_ns,
                cursor.deadline_ns,
                config,
            ));
        }

        write_port(shared.registers(), cursor.port, PX_CMD, command & !CMD_FRE);
        self.state = InitState::StoppingFis(cursor);
        InitPoll::Pending(status_check_schedule(
            input.now_ns,
            cursor.deadline_ns,
            config,
        ))
    }

    fn poll_fis_stop(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        input: InitInput,
        cursor: PortCursor,
    ) -> InitPoll<()> {
        let command = read_port(shared.registers(), cursor.port, PX_CMD);
        if command & CMD_FR != 0 {
            if input.now_ns >= cursor.deadline_ns {
                return self.fail(InitError::Hardware("AHCI FIS receive engine did not stop"));
            }
            self.state = InitState::StoppingFis(cursor);
            return InitPoll::Pending(status_check_schedule(
                input.now_ns,
                cursor.deadline_ns,
                config,
            ));
        }

        write_port(
            shared.registers(),
            cursor.port,
            PX_CMD,
            (command | CMD_SUD | CMD_POD | CMD_ICC_ACTIVE) & !(CMD_ST | CMD_FRE),
        );
        let control = read_port(shared.registers(), cursor.port, PX_SCTL);
        write_port(
            shared.registers(),
            cursor.port,
            PX_SCTL,
            (control & !SCTL_DET_MASK) | SCTL_DET_INIT,
        );
        let release_at_ns = input.now_ns.saturating_add(config.comreset_assert_ns);
        self.state = InitState::AssertingComreset(cursor.with_deadline(release_at_ns));
        InitPoll::Pending(InitSchedule::wait_until(release_at_ns))
    }

    fn poll_comreset_assertion(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        input: InitInput,
        cursor: PortCursor,
    ) -> InitPoll<()> {
        if input.now_ns < cursor.deadline_ns {
            self.state = InitState::AssertingComreset(cursor);
            return InitPoll::Pending(InitSchedule::wait_until(cursor.deadline_ns));
        }

        let control = read_port(shared.registers(), cursor.port, PX_SCTL);
        write_port(
            shared.registers(),
            cursor.port,
            PX_SCTL,
            (control & !SCTL_DET_MASK) | SCTL_DET_NONE,
        );
        let link_deadline = input.now_ns.saturating_add(config.link_timeout_ns);
        self.state = InitState::WaitingLink(cursor.with_deadline(link_deadline));
        InitPoll::Pending(status_check_schedule(input.now_ns, link_deadline, config))
    }

    fn poll_link(
        &mut self,
        shared: &HostShared,
        dma: &DeviceDma,
        config: AhciConfig,
        input: InitInput,
        cursor: PortCursor,
    ) -> InitPoll<()> {
        if read_port(shared.registers(), cursor.port, PX_SSTS) & 0xf != 3 {
            if input.now_ns >= cursor.deadline_ns {
                self.state = InitState::Scanning(cursor.scan);
                return InitPoll::Pending(InitSchedule::immediate());
            }
            self.state = InitState::WaitingLink(cursor);
            return InitPoll::Pending(status_check_schedule(
                input.now_ns,
                cursor.deadline_ns,
                config,
            ));
        }

        shared.port(cursor.port).discard_stale_snapshots();
        let port_dma = constrained_dma(dma, cursor.scan.cap);
        let command_memory = match PortCommandMemory::allocate(&port_dma) {
            Ok(memory) => memory,
            Err(_) => return self.fail(InitError::Hardware("AHCI command DMA allocation failed")),
        };
        command_memory.program_bases(shared.registers(), cursor.port);
        shared.port(cursor.port).publish_dma_bases(
            command_memory.command_list_dma(),
            command_memory.received_fis_dma(),
        );
        let command = read_port(shared.registers(), cursor.port, PX_CMD);
        write_port(
            shared.registers(),
            cursor.port,
            PX_CMD,
            (command | CMD_FRE | CMD_SUD | CMD_POD | CMD_ICC_ACTIVE) & !CMD_ST,
        );
        let engine_deadline = input.now_ns.saturating_add(config.port_stop_timeout_ns);
        self.state = InitState::StartingFis {
            cursor: cursor.with_deadline(engine_deadline),
            command_memory,
        };
        InitPoll::Pending(status_check_schedule(input.now_ns, engine_deadline, config))
    }

    fn poll_fis_start(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        input: InitInput,
        cursor: PortCursor,
        command_memory: PortCommandMemory,
    ) -> InitPoll<()> {
        let command = read_port(shared.registers(), cursor.port, PX_CMD);
        if command & CMD_FR == 0 {
            if input.now_ns >= cursor.deadline_ns {
                return self.begin_abort(
                    shared,
                    config,
                    input.now_ns,
                    cursor,
                    command_memory,
                    None,
                );
            }
            self.state = InitState::StartingFis {
                cursor,
                command_memory,
            };
            return InitPoll::Pending(status_check_schedule(
                input.now_ns,
                cursor.deadline_ns,
                config,
            ));
        }

        write_port(shared.registers(), cursor.port, PX_CMD, command | CMD_ST);
        let command_deadline = input.now_ns.saturating_add(config.port_stop_timeout_ns);
        self.state = InitState::StartingEngine {
            cursor: cursor.with_deadline(command_deadline),
            command_memory,
        };
        InitPoll::Pending(status_check_schedule(
            input.now_ns,
            command_deadline,
            config,
        ))
    }

    fn poll_engine_start(
        &mut self,
        shared: &HostShared,
        dma_device: &DeviceDma,
        config: AhciConfig,
        input: InitInput,
        cursor: PortCursor,
        mut command_memory: PortCommandMemory,
    ) -> InitPoll<()> {
        let command = read_port(shared.registers(), cursor.port, PX_CMD);
        let task_file = read_port(shared.registers(), cursor.port, PX_TFD);
        if command & (CMD_CR | CMD_FR) != (CMD_CR | CMD_FR) || task_file & TFD_NOT_READY != 0 {
            if input.now_ns >= cursor.deadline_ns {
                return self.begin_abort(
                    shared,
                    config,
                    input.now_ns,
                    cursor,
                    command_memory,
                    None,
                );
            }
            self.state = InitState::StartingEngine {
                cursor,
                command_memory,
            };
            return InitPoll::Pending(status_check_schedule(
                input.now_ns,
                cursor.deadline_ns,
                config,
            ));
        }

        let Some(_register_window) = shared.try_claim_register_window() else {
            self.state = InitState::StartingEngine {
                cursor,
                command_memory,
            };
            return InitPoll::Pending(InitSchedule::immediate());
        };
        clear_initial_irq_state(shared, cursor.port);
        let port_dma = constrained_dma(dma_device, cursor.scan.cap);
        let identify_len = NonZeroUsize::new(IDENTIFY_BYTES)
            .expect("the ATA IDENTIFY transfer has a nonzero fixed size");
        let identify =
            match CpuDmaBuffer::new_zero(&port_dma, identify_len, 2, DmaDirection::FromDevice) {
                Ok(buffer) => buffer.prepare_for_device(),
                Err(_) => {
                    return self.begin_abort(
                        shared,
                        config,
                        input.now_ns,
                        cursor,
                        command_memory,
                        None,
                    );
                }
            };
        command_memory.build_identify(identify.dma_addr());
        let identify = unsafe {
            // SAFETY: state owns this backing before PxCI is armed and only an
            // IRQ completion or explicit engine stop can return ownership.
            identify.into_in_flight()
        };
        let generation = shared.port(cursor.port).next_request_generation();
        if !shared.port(cursor.port).publish_active_request(generation) {
            return self.begin_abort(
                shared,
                config,
                input.now_ns,
                cursor,
                command_memory,
                Some(identify),
            );
        }
        // Publish the command before unmasking the port. A link event latched
        // while the port was masked can then never look like completion of an
        // as-yet-unissued IDENTIFY request.
        write_port(shared.registers(), cursor.port, PX_CI, 1 << COMMAND_SLOT);
        write_port(
            shared.registers(),
            cursor.port,
            PX_IE,
            crate::registers::DEFAULT_PORT_IRQ_MASK,
        );
        let identify_deadline = input.now_ns.saturating_add(config.command_timeout_ns);
        self.state = InitState::WaitingIdentify(IdentifyCommand {
            cursor: cursor.with_deadline(identify_deadline),
            generation,
            command_memory,
            dma: identify,
            completion_seen: false,
        });
        InitPoll::Pending(command_wait_schedule(
            config.irq_source_id,
            identify_deadline,
        ))
    }

    fn poll_identify(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        ready_ports: &mut Vec<Option<ReadyPort>>,
        input: InitInput,
        mut command: IdentifyCommand,
    ) -> InitPoll<()> {
        if shared.port(command.cursor.port).take_overflow() {
            shared
                .port(command.cursor.port)
                .clear_active_request(command.generation);
            return self.begin_abort(
                shared,
                config,
                input.now_ns,
                command.cursor,
                command.command_memory,
                Some(command.dma),
            );
        }
        let mut failed = false;
        for _ in 0..crate::irq::IRQ_SNAPSHOT_CAPACITY {
            let Some(snapshot) = shared.port(command.cursor.port).pop_snapshot() else {
                break;
            };
            if snapshot.epoch != shared.port(command.cursor.port).epoch() {
                continue;
            }
            if snapshot.has_error() {
                failed = true;
                break;
            }
            if snapshot.completes(COMMAND_SLOT, command.generation) {
                command.completion_seen = true;
            }
        }

        if failed {
            shared
                .port(command.cursor.port)
                .clear_active_request(command.generation);
            return self.begin_abort(
                shared,
                config,
                input.now_ns,
                command.cursor,
                command.command_memory,
                Some(command.dma),
            );
        }
        if shared.port(command.cursor.port).has_snapshots() {
            // A full bounded batch may have observed command completion while
            // a later acknowledged error is already queued. Preserve the
            // candidate but classify the remaining IRQ facts before exposing
            // disk capacity or command-memory ownership.
            self.state = InitState::WaitingIdentify(command);
            return InitPoll::Pending(InitSchedule::immediate());
        }
        if command.completion_seen {
            if !shared
                .port(command.cursor.port)
                .clear_active_request(command.generation)
            {
                return self.begin_abort(
                    shared,
                    config,
                    input.now_ns,
                    command.cursor,
                    command.command_memory,
                    Some(command.dma),
                );
            }
            write_port(shared.registers(), command.cursor.port, PX_IE, 0);
            let completed = unsafe {
                // SAFETY: the acknowledged IRQ snapshot observed PxCI clear
                // for slot zero, proving the IDENTIFY PRDT is no longer live.
                command.dma.complete_after_quiesce()
            };
            let buffer = completed.into_cpu_buffer();
            let Some(ata) = AtaDevice::parse_identify(buffer.as_slice_cpu()) else {
                return self.begin_abort(
                    shared,
                    config,
                    input.now_ns,
                    command.cursor,
                    command.command_memory,
                    None,
                );
            };
            ready_ports.push(Some(ReadyPort {
                port: command.cursor.port,
                ata,
                command_memory: command.command_memory,
            }));
            shared.publish_ready_port(command.cursor.port);
            self.state = InitState::Scanning(command.cursor.scan);
            return InitPoll::Pending(InitSchedule::immediate());
        }

        if input.now_ns >= command.cursor.deadline_ns {
            shared
                .port(command.cursor.port)
                .clear_active_request(command.generation);
            return self.begin_abort(
                shared,
                config,
                input.now_ns,
                command.cursor,
                command.command_memory,
                Some(command.dma),
            );
        }
        let deadline_ns = command.cursor.deadline_ns;
        self.state = InitState::WaitingIdentify(command);
        InitPoll::Pending(command_wait_schedule(config.irq_source_id, deadline_ns))
    }

    fn begin_abort(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        now_ns: u64,
        cursor: PortCursor,
        command_memory: PortCommandMemory,
        dma: Option<InFlightDma>,
    ) -> InitPoll<()> {
        freeze_port(shared, cursor.port);
        let command = read_port(shared.registers(), cursor.port, PX_CMD);
        write_port(shared.registers(), cursor.port, PX_CMD, command & !CMD_ST);
        let deadline_ns = now_ns.saturating_add(config.port_stop_timeout_ns);
        self.state = InitState::AbortCommand(AbortingPort {
            cursor: cursor.with_deadline(deadline_ns),
            command_memory,
            dma,
        });
        InitPoll::Pending(status_check_schedule(now_ns, deadline_ns, config))
    }

    fn poll_abort_command(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        input: InitInput,
        abort: AbortingPort,
    ) -> InitPoll<()> {
        let command = read_port(shared.registers(), abort.cursor.port, PX_CMD);
        if command & CMD_CR != 0 {
            if input.now_ns >= abort.cursor.deadline_ns {
                return self.quarantine_failed_port(abort.command_memory, abort.dma);
            }
            let deadline_ns = abort.cursor.deadline_ns;
            self.state = InitState::AbortCommand(abort);
            return InitPoll::Pending(status_check_schedule(input.now_ns, deadline_ns, config));
        }
        write_port(
            shared.registers(),
            abort.cursor.port,
            PX_CMD,
            command & !CMD_FRE,
        );
        let deadline_ns = abort.cursor.deadline_ns;
        self.state = InitState::AbortFis(abort);
        InitPoll::Pending(status_check_schedule(input.now_ns, deadline_ns, config))
    }

    fn poll_abort_fis(
        &mut self,
        shared: &HostShared,
        config: AhciConfig,
        input: InitInput,
        abort: AbortingPort,
    ) -> InitPoll<()> {
        if read_port(shared.registers(), abort.cursor.port, PX_CMD) & CMD_FR != 0 {
            if input.now_ns >= abort.cursor.deadline_ns {
                return self.quarantine_failed_port(abort.command_memory, abort.dma);
            }
            let deadline_ns = abort.cursor.deadline_ns;
            self.state = InitState::AbortFis(abort);
            return InitPoll::Pending(status_check_schedule(input.now_ns, deadline_ns, config));
        }
        if let Some(dma) = abort.dma {
            let _completed = unsafe {
                // SAFETY: both AHCI command-list and FIS receive engines have
                // acknowledged stop, so this port can no longer access PRDT.
                dma.complete_after_quiesce()
            };
        }
        drop(abort.command_memory);
        self.state = InitState::Scanning(abort.cursor.scan);
        InitPoll::Pending(InitSchedule::immediate())
    }

    fn quarantine_failed_port(
        &mut self,
        command_memory: PortCommandMemory,
        dma: Option<InFlightDma>,
    ) -> InitPoll<()> {
        if let Some(dma) = dma {
            let _ = dma.quarantine();
        }
        mem::forget(command_memory);
        self.fail(InitError::Hardware(
            "AHCI port could not prove DMA quiescence",
        ))
    }

    fn fail<T>(&mut self, error: InitError) -> InitPoll<T> {
        self.state = InitState::Failed;
        InitPoll::Failed(error)
    }
}

#[derive(Clone, Copy)]
struct PortScan {
    cap: u32,
    ports: u32,
    next: usize,
}

#[derive(Clone, Copy)]
struct PortCursor {
    scan: PortScan,
    port: usize,
    deadline_ns: u64,
}

impl PortCursor {
    const fn with_deadline(self, deadline_ns: u64) -> Self {
        Self {
            deadline_ns,
            ..self
        }
    }
}

struct IdentifyCommand {
    cursor: PortCursor,
    generation: u64,
    command_memory: PortCommandMemory,
    dma: InFlightDma,
    completion_seen: bool,
}

struct AbortingPort {
    cursor: PortCursor,
    command_memory: PortCommandMemory,
    dma: Option<InFlightDma>,
}

enum InitState {
    Discovered,
    AcquiringOwnership {
        deadline_ns: u64,
    },
    Resetting {
        deadline_ns: u64,
    },
    Scanning(PortScan),
    StoppingCommand(PortCursor),
    StoppingFis(PortCursor),
    AssertingComreset(PortCursor),
    WaitingLink(PortCursor),
    StartingFis {
        cursor: PortCursor,
        command_memory: PortCommandMemory,
    },
    StartingEngine {
        cursor: PortCursor,
        command_memory: PortCommandMemory,
    },
    WaitingIdentify(IdentifyCommand),
    AbortCommand(AbortingPort),
    AbortFis(AbortingPort),
    Ready,
    Failed,
    Transition,
}

fn constrained_dma(dma: &DeviceDma, cap: u32) -> DeviceDma {
    let mut constraints = dma.constraints();
    if cap & CAP_S64A == 0 {
        constraints.addr_mask = constraints.addr_mask.min(u64::from(u32::MAX));
    }
    dma.with_constraints(constraints)
}

fn clear_initial_irq_state(shared: &HostShared, port: usize) {
    // The initialization action is live, but this port's PxIE remains zero and
    // no normal queue exists. This exclusive phase may clear firmware/link
    // latches before publishing the first request; normal I/O never performs
    // these destructive reads outside the hard-IRQ endpoint.
    let sata_error = read_port(shared.registers(), port, PX_SERR);
    if sata_error != 0 {
        write_port(shared.registers(), port, PX_SERR, sata_error);
    }
    let status = read_port(shared.registers(), port, PX_IS);
    if status != 0 {
        write_port(shared.registers(), port, PX_IS, status);
    }
    shared.registers().write32(HOST_IS, 1 << port);
    shared.port(port).discard_stale_snapshots();
}

fn status_check_schedule(now_ns: u64, deadline_ns: u64, config: AhciConfig) -> InitSchedule {
    InitSchedule::wait_until(
        now_ns
            .saturating_add(config.status_check_ns)
            .min(deadline_ns),
    )
}

fn command_wait_schedule(source_id: usize, deadline_ns: u64) -> InitSchedule {
    let mut sources = IdList::none();
    sources.insert(source_id);
    InitSchedule::wait_for_irq_until(sources, deadline_ns)
        .expect("a live AHCI command IRQ source must fit the RDIF source mask")
}

#[cfg(test)]
mod tests {
    use dma_api::DeviceDma;

    use super::*;
    use crate::{
        registers::{
            CMD_CR, CMD_FR, DEFAULT_PORT_IRQ_MASK, GHC_AE, HOST_CAP, HOST_GHC, HOST_IS, HOST_PI,
            IRQ_D2H_REG_FIS, MMIO_REQUIRED_SIZE, PX_CI, PX_CMD, PX_IE, PX_IS, PX_SERR, PX_SSTS,
            port_offset, tests_support::FakeRegisters,
        },
        test_support::TEST_DMA,
    };

    #[test]
    fn first_hardware_command_requires_bound_and_enabled_irq_owner() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let shared = HostShared::new(registers.shared());
        let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
        let mut initialization = AhciInitialization::discovered();

        assert!(matches!(
            initialization.poll(
                &shared,
                &dma,
                test_config(),
                &mut Vec::new(),
                InitInput::at(0),
            ),
            InitPoll::Failed(InitError::MissingInterrupt)
        ));
        assert!(registers.writes().is_empty());

        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let shared = HostShared::new(registers.shared());
        let _handler = shared.take_initial_handler().unwrap();
        let mut initialization = AhciInitialization::discovered();
        assert!(matches!(
            initialization.poll(
                &shared,
                &dma,
                test_config(),
                &mut Vec::new(),
                InitInput::at(0),
            ),
            InitPoll::Failed(InitError::MissingInterrupt)
        ));
        assert!(registers.writes().is_empty());
    }

    #[test]
    fn dropped_initial_irq_endpoint_prevents_the_first_hardware_command() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let shared = HostShared::new(registers.shared());
        let handler = shared.take_initial_handler().unwrap();
        drop(handler);
        shared.set_irq_delivery_enabled(true);
        let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
        let mut initialization = AhciInitialization::discovered();

        assert!(matches!(
            initialization.poll(
                &shared,
                &dma,
                test_config(),
                &mut Vec::new(),
                InitInput::at(0),
            ),
            InitPoll::Failed(InitError::MissingInterrupt)
        ));
        assert!(registers.writes().is_empty());
    }

    #[test]
    fn reset_timeout_is_absolute_and_independent_of_poll_frequency() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let shared = HostShared::new(registers.shared());
        let _handler = shared.take_initial_handler().unwrap();
        shared.set_irq_delivery_enabled(true);
        let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
        let mut initialization = AhciInitialization::discovered();
        let mut ready = Vec::new();

        let InitPoll::Pending(first) =
            initialization.poll(&shared, &dma, test_config(), &mut ready, InitInput::at(10))
        else {
            panic!("reset must be asynchronous");
        };
        assert_eq!(first.wake_at_ns(), Some(17));

        for (now_ns, expected_wake) in [(11, 18), (50, 57), (109, 110)] {
            let InitPoll::Pending(schedule) = initialization.poll(
                &shared,
                &dma,
                test_config(),
                &mut ready,
                InitInput::at(now_ns),
            ) else {
                panic!("reset must retain its original absolute deadline");
            };
            assert_eq!(schedule.wake_at_ns(), Some(expected_wake));
        }
        assert!(matches!(
            initialization.poll(&shared, &dma, test_config(), &mut ready, InitInput::at(110),),
            InitPoll::Failed(InitError::TimedOut)
        ));
    }

    #[test]
    fn reset_masks_published_ports_before_global_irq_enable() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let shared = HostShared::new(registers.shared());
        let _handler = shared.take_initial_handler().unwrap();
        shared.set_irq_delivery_enabled(true);
        let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
        let mut initialization = AhciInitialization::discovered();
        let mut ready = Vec::new();

        assert!(matches!(
            initialization.poll(&shared, &dma, test_config(), &mut ready, InitInput::at(0),),
            InitPoll::Pending(_)
        ));
        registers.set(HOST_GHC, GHC_AE);
        registers.set(HOST_CAP, CAP_S64A);
        registers.set(HOST_PI, 1);
        registers.clear_access_log();

        assert!(matches!(
            initialization.poll(&shared, &dma, test_config(), &mut ready, InitInput::at(1),),
            InitPoll::Pending(_)
        ));

        let reads = registers.reads();
        let writes = registers.writes();
        let port_inventory = reads
            .iter()
            .find(|read| read.offset == HOST_PI)
            .expect("reset completion must read the implemented-port bitmap");
        let port_mask = writes
            .iter()
            .find(|write| write.offset == port_offset(0, PX_IE) && write.value == 0)
            .expect("every implemented port must be masked before global IRQ enable");
        let global_enable = writes
            .iter()
            .find(|write| write.offset == HOST_GHC && write.value & GHC_IE != 0)
            .expect("the live initialization action must eventually receive IRQs");
        assert!(port_inventory.sequence < port_mask.sequence);
        assert!(port_mask.sequence < global_enable.sequence);
    }

    #[test]
    fn firmware_handoff_precedes_reset_and_uses_an_absolute_deadline() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        registers.set(HOST_CAP2, CAP2_BOH);
        registers.set(HOST_BOHC, BOHC_BOS | BOHC_BB);
        let shared = HostShared::new(registers.shared());
        let _handler = shared.take_initial_handler().unwrap();
        shared.set_irq_delivery_enabled(true);
        let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
        let mut initialization = AhciInitialization::discovered();
        let mut ready = Vec::new();

        let InitPoll::Pending(schedule) =
            initialization.poll(&shared, &dma, test_config(), &mut ready, InitInput::at(10))
        else {
            panic!("firmware-owned HBA must enter handoff");
        };
        assert_eq!(schedule.wake_at_ns(), Some(17));
        assert!(matches!(
            initialization.state,
            InitState::AcquiringOwnership { deadline_ns: 110 }
        ));
        assert!(
            registers
                .writes()
                .iter()
                .any(|write| { write.offset == HOST_BOHC && write.value & BOHC_OOS != 0 })
        );
        assert!(
            registers
                .writes()
                .iter()
                .all(|write| write.offset != HOST_GHC || write.value & GHC_HR == 0)
        );

        registers.set(HOST_BOHC, BOHC_OOS);
        assert!(matches!(
            initialization.poll(&shared, &dma, test_config(), &mut ready, InitInput::at(50),),
            InitPoll::Pending(_)
        ));
        assert!(matches!(
            initialization.state,
            InitState::Resetting { deadline_ns: 150 }
        ));
        assert!(
            registers
                .writes()
                .iter()
                .any(|write| { write.offset == HOST_GHC && write.value & GHC_HR != 0 })
        );
    }

    #[test]
    fn comreset_release_uses_the_original_absolute_deadline() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        registers.set(port_offset(0, PX_SCTL), SCTL_DET_INIT);
        let shared = HostShared::new(registers.shared());
        let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
        let cursor = PortCursor {
            scan: PortScan {
                cap: 0,
                ports: 1,
                next: 1,
            },
            port: 0,
            deadline_ns: 10,
        };
        let mut initialization = AhciInitialization {
            state: InitState::AssertingComreset(cursor),
        };
        let mut ready = Vec::new();

        for now_ns in [1, 4, 9] {
            registers.clear_access_log();
            let InitPoll::Pending(schedule) = initialization.poll(
                &shared,
                &dma,
                test_config(),
                &mut ready,
                InitInput::at(now_ns),
            ) else {
                panic!("COMRESET must remain asserted before its deadline");
            };
            assert_eq!(schedule.wake_at_ns(), Some(10));
            assert!(registers.writes().is_empty());
        }

        assert!(matches!(
            initialization.poll(&shared, &dma, test_config(), &mut ready, InitInput::at(10),),
            InitPoll::Pending(_)
        ));
        assert_eq!(
            read_port(registers.as_ref(), 0, PX_SCTL) & SCTL_DET_MASK,
            SCTL_DET_NONE
        );
        assert!(matches!(initialization.state, InitState::WaitingLink(_)));
    }

    #[test]
    fn identify_arm_waits_for_the_destructive_irq_register_window() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        registers.set(port_offset(0, PX_CMD), CMD_CR | CMD_FR | CMD_ST | CMD_FRE);
        let shared = HostShared::new(registers.shared());
        let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
        let command_memory = PortCommandMemory::allocate(&dma).unwrap();
        let cursor = PortCursor {
            scan: PortScan {
                cap: CAP_S64A,
                ports: 1,
                next: 1,
            },
            port: 0,
            deadline_ns: 100,
        };
        let mut initialization = AhciInitialization {
            state: InitState::StartingEngine {
                cursor,
                command_memory,
            },
        };
        let register_window = shared
            .try_claim_register_window()
            .expect("the test IRQ endpoint must own the destructive register window");
        registers.clear_access_log();

        let InitPoll::Pending(schedule) = initialization.poll(
            &shared,
            &dma,
            test_config(),
            &mut Vec::new(),
            InitInput::at(10),
        ) else {
            panic!("initialization must defer while IRQ owns the register window");
        };

        assert!(schedule.run_again());
        assert!(matches!(
            initialization.state,
            InitState::StartingEngine { .. }
        ));
        assert!(
            registers
                .writes()
                .iter()
                .all(|write| write.offset != port_offset(0, PX_CI))
        );

        drop(register_window);
        registers.set(port_offset(0, PX_TFD), crate::registers::TFD_BSY);
        assert!(matches!(
            initialization.poll(
                &shared,
                &dma,
                test_config(),
                &mut Vec::new(),
                InitInput::at(100),
            ),
            InitPoll::Pending(_)
        ));
        registers.set(port_offset(0, PX_CMD), 0);
        assert!(matches!(
            initialization.poll(
                &shared,
                &dma,
                test_config(),
                &mut Vec::new(),
                InitInput::at(101),
            ),
            InitPoll::Pending(_)
        ));
        assert!(matches!(
            initialization.poll(
                &shared,
                &dma,
                test_config(),
                &mut Vec::new(),
                InitInput::at(102),
            ),
            InitPoll::Pending(_)
        ));
    }

    #[test]
    fn identify_watchdog_never_reads_completion_state() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let shared = HostShared::new(registers.shared());
        let _handler = shared.take_initial_handler().unwrap();
        shared.set_irq_delivery_enabled(true);
        let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
        let mut initialization = AhciInitialization::discovered();
        let mut ready = Vec::new();

        assert!(matches!(
            initialization.poll(&shared, &dma, test_config(), &mut ready, InitInput::at(0),),
            InitPoll::Pending(_)
        ));
        registers.set(HOST_GHC, GHC_AE);
        registers.set(HOST_CAP, CAP_S64A);
        registers.set(HOST_PI, 1);
        assert!(matches!(
            initialization.poll(&shared, &dma, test_config(), &mut ready, InitInput::at(1),),
            InitPoll::Pending(_)
        ));
        assert!(matches!(
            initialization.poll(&shared, &dma, test_config(), &mut ready, InitInput::at(2),),
            InitPoll::Pending(_)
        ));
        assert!(matches!(
            initialization.poll(&shared, &dma, test_config(), &mut ready, InitInput::at(3),),
            InitPoll::Pending(_)
        ));
        assert!(matches!(
            initialization.poll(&shared, &dma, test_config(), &mut ready, InitInput::at(4),),
            InitPoll::Pending(_)
        ));
        registers.set(port_offset(0, PX_SSTS), 3);
        assert!(matches!(
            initialization.poll(&shared, &dma, test_config(), &mut ready, InitInput::at(5),),
            InitPoll::Pending(_)
        ));
        registers.set(port_offset(0, PX_CMD), CMD_CR | CMD_FR | CMD_ST | CMD_FRE);
        assert!(matches!(
            initialization.poll(&shared, &dma, test_config(), &mut ready, InitInput::at(6),),
            InitPoll::Pending(_)
        ));
        registers.set(HOST_IS, 1);
        registers.set(port_offset(0, PX_IS), IRQ_D2H_REG_FIS);
        registers.set(port_offset(0, PX_SERR), 0x20);
        assert!(matches!(
            initialization.poll(&shared, &dma, test_config(), &mut ready, InitInput::at(7),),
            InitPoll::Pending(_)
        ));
        registers.clear_access_log();
        assert!(matches!(
            initialization.poll(&shared, &dma, test_config(), &mut ready, InitInput::at(8),),
            InitPoll::Pending(_)
        ));

        let writes = registers.writes();
        let command_issue = writes
            .iter()
            .find(|write| write.offset == port_offset(0, PX_CI) && write.value == 1)
            .unwrap();
        let interrupt_enable = writes
            .iter()
            .find(|write| {
                write.offset == port_offset(0, PX_IE) && write.value == DEFAULT_PORT_IRQ_MASK
            })
            .unwrap();
        let stale_status_ack = writes
            .iter()
            .find(|write| write.offset == port_offset(0, PX_IS) && write.value == IRQ_D2H_REG_FIS)
            .unwrap();
        let stale_host_ack = writes
            .iter()
            .find(|write| write.offset == HOST_IS && write.value == 1)
            .unwrap();
        assert!(stale_status_ack.sequence < command_issue.sequence);
        assert!(stale_host_ack.sequence < command_issue.sequence);
        assert!(command_issue.sequence < interrupt_enable.sequence);

        registers.clear_access_log();
        assert!(matches!(
            initialization.poll(&shared, &dma, test_config(), &mut ready, InitInput::at(108),),
            InitPoll::Pending(_)
        ));
        assert!(
            registers
                .reads()
                .iter()
                .all(|read| read.offset != port_offset(0, PX_CI))
        );
        assert!(matches!(initialization.state, InitState::AbortCommand(_)));
        assert!(initialization.quarantine_owned_dma());
    }

    #[test]
    fn identify_classifies_queued_error_before_publishing_ready_port() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let shared = HostShared::new(registers.shared());
        shared.publish_implemented_ports(1);
        shared.set_irq_delivery_enabled(true);
        let mut handler = shared.take_initial_handler().unwrap();
        let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
        let command_memory = PortCommandMemory::allocate(&dma).unwrap();
        let mut identify = CpuDmaBuffer::new_zero(
            &dma,
            NonZeroUsize::new(IDENTIFY_BYTES).unwrap(),
            2,
            DmaDirection::FromDevice,
        )
        .unwrap();
        let bytes = unsafe {
            // SAFETY: the test still owns the CPU-side DMA buffer and has not
            // prepared or published it to fake hardware.
            identify.as_mut_slice_cpu()
        };
        set_identify_word(bytes, 49, 1 << 9);
        set_identify_word(bytes, 60, 0x1000);
        let identify = unsafe {
            // SAFETY: the fake controller has no DMA engine. The state machine
            // retains the buffer until an IRQ snapshot or abort quiesces it.
            identify.prepare_for_device().into_in_flight()
        };
        let generation = shared.port(0).next_request_generation();
        assert!(shared.port(0).publish_active_request(generation));
        let cursor = PortCursor {
            scan: PortScan {
                cap: CAP_S64A,
                ports: 1,
                next: 1,
            },
            port: 0,
            deadline_ns: 100,
        };
        let mut initialization = AhciInitialization {
            state: InitState::WaitingIdentify(IdentifyCommand {
                cursor,
                generation,
                command_memory,
                dma: identify,
                completion_seen: false,
            }),
        };
        let mut ready = Vec::new();

        registers.set(HOST_IS, 1);
        registers.set(port_offset(0, PX_IS), IRQ_D2H_REG_FIS);
        registers.set(port_offset(0, PX_CI), 0);
        assert!(handler.handle_irq().is_handled());
        registers.set(HOST_IS, 1);
        registers.set(port_offset(0, PX_IS), crate::registers::IRQ_TASK_FILE_ERROR);
        registers.set(port_offset(0, PX_TFD), crate::registers::TFD_ERR);
        registers.set(port_offset(0, PX_SERR), 0x20);
        assert!(handler.handle_irq().is_handled());

        assert!(matches!(
            initialization.poll(&shared, &dma, test_config(), &mut ready, InitInput::at(10),),
            InitPoll::Pending(_)
        ));
        assert!(ready.is_empty());
        assert!(matches!(initialization.state, InitState::AbortCommand(_)));
        assert!(initialization.quarantine_owned_dma());
    }

    fn set_identify_word(bytes: &mut [u8], index: usize, value: u16) {
        bytes[index * 2..index * 2 + 2].copy_from_slice(&value.to_le_bytes());
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
