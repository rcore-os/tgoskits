use rdif_block::{
    ControllerEpoch, ControllerReady, DmaQuiesced, IdList, InitError, InitInput, InitPoll,
    InitSchedule, RecoveryCause,
};

const CONTROLLER_STATUS_CHECK_INTERVAL_NS: u64 = 1_000_000;
const ADMIN_COMMAND_TIMEOUT_NS: u64 = 30_000_000_000;

/// Stable completion copied out of the IRQ-owned admin CQ consumer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AdminCompletion {
    pub command_id: u16,
    pub success: bool,
    pub result: u64,
}

/// One bounded admin command required to rebuild the retained I/O queues.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdminCommand {
    IdentifyController,
    SetQueueCount { count: usize },
    CreateCompletionQueue { queue_index: usize },
    CreateSubmissionQueue { queue_index: usize },
    IdentifyNamespaceList,
    IdentifyNamespace { namespace_id: u32 },
}

/// Narrow hardware boundary used by the portable lifecycle state machine.
pub(crate) trait LifecycleHardware {
    fn controller_cookie(&self) -> usize;
    fn controller_timeout_ns(&self) -> u64;
    fn begin_controller_disable(&self);
    fn controller_ready(&self) -> bool;
    fn controller_fatal(&self) -> bool;
    /// Reinitializes retained DMA queue memory and writes CC.EN.
    ///
    /// # Safety
    ///
    /// The matching controller must have acknowledged CC.RDY=0, and every OS
    /// IRQ action and task-side queue accessor must remain drained.
    unsafe fn prepare_reinitialize(&self) -> Result<(), InitError>;
    fn queue_count(&self) -> usize;
    fn admin_irq_source(&self) -> Option<usize>;
    fn submit_admin_command(&self, command: AdminCommand) -> Result<u16, InitError>;
    fn take_admin_completion(&self) -> Option<AdminCompletion>;
    fn complete_admin_command(
        &self,
        command: AdminCommand,
        completion: AdminCompletion,
    ) -> Result<Option<AdminCommand>, InitError>;
}

/// Non-blocking reset and retained-queue reconstruction state.
pub(crate) struct NvmeLifecycle {
    state: LifecycleState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecycleState {
    Running,
    GuestOwned,
    Disabling {
        epoch: ControllerEpoch,
        deadline_ns: Option<u64>,
    },
    Quiesced {
        epoch: ControllerEpoch,
    },
    Enabling {
        epoch: ControllerEpoch,
        deadline_ns: Option<u64>,
    },
    IssueAdmin {
        epoch: ControllerEpoch,
        command: AdminCommand,
    },
    WaitAdmin {
        epoch: ControllerEpoch,
        command: AdminCommand,
        command_id: u16,
        deadline_ns: u64,
    },
    Aborting {
        error: InitError,
        deadline_ns: u64,
    },
    Failed,
}

impl NvmeLifecycle {
    pub(crate) const fn new() -> Self {
        Self {
            state: LifecycleState::Running,
        }
    }

    pub(crate) fn begin_dma_quiesce(
        &mut self,
        hardware: &impl LifecycleHardware,
        epoch: ControllerEpoch,
        _cause: RecoveryCause,
    ) -> Result<(), InitError> {
        if !matches!(
            self.state,
            LifecycleState::Running | LifecycleState::GuestOwned
        ) || hardware.controller_cookie() == 0
        {
            return Err(InitError::InvalidState);
        }

        hardware.begin_controller_disable();
        self.state = LifecycleState::Disabling {
            epoch,
            deadline_ns: None,
        };
        Ok(())
    }

    pub(crate) fn poll_dma_quiesce(
        &mut self,
        hardware: &impl LifecycleHardware,
        input: InitInput,
    ) -> InitPoll<DmaQuiesced> {
        let LifecycleState::Disabling {
            epoch,
            mut deadline_ns,
        } = self.state
        else {
            return InitPoll::Failed(InitError::InvalidState);
        };

        if !hardware.controller_ready() {
            self.state = LifecycleState::Quiesced { epoch };
            // SAFETY: the runtime closes queue admission and drains the exact
            // IRQ actions before entering this state. The hardware boundary
            // only reports RDY=0 after CC.EN was cleared, which is the NVMe
            // controller's acknowledgement that command processing and DMA
            // from the disabled controller have stopped for this epoch.
            return InitPoll::Ready(unsafe {
                DmaQuiesced::new(epoch, hardware.controller_cookie())
            });
        }
        let deadline = *deadline_ns.get_or_insert_with(|| {
            input
                .now_ns
                .saturating_add(hardware.controller_timeout_ns())
        });
        if input.now_ns >= deadline {
            return self.fail(InitError::TimedOut);
        }
        self.state = LifecycleState::Disabling { epoch, deadline_ns };
        InitPoll::Pending(status_check_schedule(input.now_ns, deadline))
    }

    pub(crate) fn enter_guest_owned(
        &mut self,
        hardware: &impl LifecycleHardware,
        quiesced: DmaQuiesced,
    ) -> Result<(), InitError> {
        let LifecycleState::Quiesced { epoch } = self.state else {
            return Err(InitError::InvalidState);
        };
        if quiesced.epoch() != epoch || quiesced.controller_cookie() != hardware.controller_cookie()
        {
            return Err(InitError::InvalidState);
        }
        self.state = LifecycleState::GuestOwned;
        Ok(())
    }

    pub(crate) fn begin_reinitialize(
        &mut self,
        hardware: &impl LifecycleHardware,
        quiesced: DmaQuiesced,
    ) -> Result<(), InitError> {
        let LifecycleState::Quiesced { epoch } = self.state else {
            return Err(InitError::InvalidState);
        };
        if quiesced.epoch() != epoch || quiesced.controller_cookie() != hardware.controller_cookie()
        {
            return Err(InitError::InvalidState);
        }
        if hardware.controller_ready() {
            return Err(InitError::InvalidState);
        }

        // SAFETY: the state machine only reaches Quiesced after producing and
        // validating the exact linear DmaQuiesced proof consumed above.
        unsafe { hardware.prepare_reinitialize()? };
        self.state = LifecycleState::Enabling {
            epoch,
            deadline_ns: None,
        };
        Ok(())
    }

    pub(crate) fn poll_reinitialize(
        &mut self,
        hardware: &impl LifecycleHardware,
        input: InitInput,
    ) -> InitPoll<ControllerReady> {
        match self.state {
            LifecycleState::Enabling { epoch, deadline_ns } => {
                self.poll_enabling(hardware, input, epoch, deadline_ns)
            }
            LifecycleState::IssueAdmin { epoch, command } => {
                self.issue_admin(hardware, input, epoch, command)
            }
            LifecycleState::WaitAdmin {
                epoch,
                command,
                command_id,
                deadline_ns,
            } => self.poll_admin(hardware, input, epoch, command, command_id, deadline_ns),
            LifecycleState::Aborting { error, deadline_ns } => {
                self.poll_aborting(hardware, input.now_ns, error, deadline_ns)
            }
            _ => InitPoll::Failed(InitError::InvalidState),
        }
    }

    fn poll_enabling(
        &mut self,
        hardware: &impl LifecycleHardware,
        input: InitInput,
        epoch: ControllerEpoch,
        mut deadline_ns: Option<u64>,
    ) -> InitPoll<ControllerReady> {
        if hardware.controller_fatal() {
            return self.begin_abort(
                hardware,
                input.now_ns,
                InitError::Hardware("NVMe controller reported fatal status while enabling"),
            );
        }
        if hardware.controller_ready() {
            let count = hardware.queue_count();
            if count == 0 {
                return self.begin_abort(
                    hardware,
                    input.now_ns,
                    InitError::Hardware("NVMe controller has no retained I/O queue"),
                );
            }
            self.state = LifecycleState::IssueAdmin {
                epoch,
                command: AdminCommand::IdentifyController,
            };
            return InitPoll::Pending(InitSchedule::immediate());
        }

        let deadline = *deadline_ns.get_or_insert_with(|| {
            input
                .now_ns
                .saturating_add(hardware.controller_timeout_ns())
        });
        if input.now_ns >= deadline {
            return self.begin_abort(hardware, input.now_ns, InitError::TimedOut);
        }
        self.state = LifecycleState::Enabling { epoch, deadline_ns };
        InitPoll::Pending(status_check_schedule(input.now_ns, deadline))
    }

    fn issue_admin(
        &mut self,
        hardware: &impl LifecycleHardware,
        input: InitInput,
        epoch: ControllerEpoch,
        command: AdminCommand,
    ) -> InitPoll<ControllerReady> {
        let Some(source_id) = hardware.admin_irq_source() else {
            return self.begin_abort(hardware, input.now_ns, InitError::MissingInterrupt);
        };
        let command_id = match hardware.submit_admin_command(command) {
            Ok(command_id) => command_id,
            Err(error) => return self.begin_abort(hardware, input.now_ns, error),
        };
        let deadline_ns = input.now_ns.saturating_add(ADMIN_COMMAND_TIMEOUT_NS);
        self.state = LifecycleState::WaitAdmin {
            epoch,
            command,
            command_id,
            deadline_ns,
        };
        InitPoll::Pending(admin_wait_schedule(source_id, deadline_ns))
    }

    fn poll_admin(
        &mut self,
        hardware: &impl LifecycleHardware,
        input: InitInput,
        epoch: ControllerEpoch,
        command: AdminCommand,
        command_id: u16,
        deadline_ns: u64,
    ) -> InitPoll<ControllerReady> {
        if hardware.controller_fatal() {
            return self.begin_abort(
                hardware,
                input.now_ns,
                InitError::Hardware("NVMe controller reported fatal status during admin command"),
            );
        }

        let Some(source_id) = hardware.admin_irq_source() else {
            return self.begin_abort(hardware, input.now_ns, InitError::MissingInterrupt);
        };
        let completion = input
            .irq_sources
            .contains(source_id)
            .then(|| hardware.take_admin_completion())
            .flatten();
        let Some(completion) = completion else {
            if input.now_ns >= deadline_ns {
                return self.begin_abort(hardware, input.now_ns, InitError::TimedOut);
            }
            return InitPoll::Pending(admin_wait_schedule(source_id, deadline_ns));
        };
        if completion.command_id != command_id || !completion.success {
            return self.begin_abort(
                hardware,
                input.now_ns,
                InitError::Hardware("NVMe admin command failed"),
            );
        }

        match hardware.complete_admin_command(command, completion) {
            Ok(Some(next)) => {
                self.state = LifecycleState::IssueAdmin {
                    epoch,
                    command: next,
                };
                InitPoll::Pending(InitSchedule::immediate())
            }
            Ok(None) => {
                self.state = LifecycleState::Running;
                // SAFETY: the retained admin and I/O queues were reset only
                // after consuming the matching DmaQuiesced proof. CC.RDY is
                // set, and every Set Features/Create CQ/Create SQ command was
                // observed as successful by the IRQ-owned admin CQ endpoint.
                InitPoll::Ready(unsafe {
                    ControllerReady::new(epoch, hardware.controller_cookie())
                })
            }
            Err(error) => self.begin_abort(hardware, input.now_ns, error),
        }
    }

    fn begin_abort<T>(
        &mut self,
        hardware: &impl LifecycleHardware,
        now_ns: u64,
        error: InitError,
    ) -> InitPoll<T> {
        hardware.begin_controller_disable();
        let deadline_ns = now_ns.saturating_add(hardware.controller_timeout_ns());
        self.state = LifecycleState::Aborting { error, deadline_ns };
        self.poll_aborting(hardware, now_ns, error, deadline_ns)
    }

    fn poll_aborting<T>(
        &mut self,
        hardware: &impl LifecycleHardware,
        now_ns: u64,
        error: InitError,
        deadline_ns: u64,
    ) -> InitPoll<T> {
        if !hardware.controller_ready() || now_ns >= deadline_ns {
            return self.fail(error);
        }
        InitPoll::Pending(status_check_schedule(now_ns, deadline_ns))
    }

    fn fail<T>(&mut self, error: InitError) -> InitPoll<T> {
        self.state = LifecycleState::Failed;
        InitPoll::Failed(error)
    }
}

fn status_check_schedule(now_ns: u64, deadline_ns: u64) -> InitSchedule {
    InitSchedule::wait_until(
        now_ns
            .saturating_add(CONTROLLER_STATUS_CHECK_INTERVAL_NS)
            .min(deadline_ns),
    )
}

fn admin_wait_schedule(source_id: usize, deadline_ns: u64) -> InitSchedule {
    let mut sources = IdList::none();
    sources.insert(source_id);
    InitSchedule::wait_for_irq_until(sources, deadline_ns)
        .expect("a live NVMe admin IRQ source must fit the RDIF source mask")
}

pub(crate) fn queue_count_supported(result: u64, requested: usize) -> bool {
    let result = result as u32;
    let allocated_submission = usize::from((result & 0xffff) as u16).saturating_add(1);
    let allocated_completion = usize::from((result >> 16) as u16).saturating_add(1);
    allocated_submission >= requested && allocated_completion >= requested
}

#[cfg(test)]
mod tests {
    use alloc::{collections::VecDeque, vec::Vec};
    use core::cell::{Cell, RefCell};

    use rdif_block::{ControllerEpoch, IdList, InitError, InitInput, InitPoll, RecoveryCause};

    use super::{
        ADMIN_COMMAND_TIMEOUT_NS, AdminCommand, AdminCompletion, LifecycleHardware, NvmeLifecycle,
    };

    struct FakeHardware {
        ready: Cell<bool>,
        fatal: Cell<bool>,
        disable_count: Cell<usize>,
        prepare_count: Cell<usize>,
        next_command_id: Cell<u16>,
        commands: RefCell<Vec<AdminCommand>>,
        completions: RefCell<VecDeque<AdminCompletion>>,
        queue_count: usize,
    }

    impl FakeHardware {
        fn new(queue_count: usize) -> Self {
            Self {
                ready: Cell::new(true),
                fatal: Cell::new(false),
                disable_count: Cell::new(0),
                prepare_count: Cell::new(0),
                next_command_id: Cell::new(7),
                commands: RefCell::new(Vec::new()),
                completions: RefCell::new(VecDeque::new()),
                queue_count,
            }
        }

        fn complete_last_command(&self, result: u64) {
            let command_id = self.next_command_id.get() - 1;
            self.completions.borrow_mut().push_back(AdminCompletion {
                command_id,
                success: true,
                result,
            });
        }
    }

    impl LifecycleHardware for FakeHardware {
        fn controller_cookie(&self) -> usize {
            0x1234
        }

        fn controller_timeout_ns(&self) -> u64 {
            500_000_000
        }

        fn begin_controller_disable(&self) {
            self.disable_count.set(self.disable_count.get() + 1);
        }

        fn controller_ready(&self) -> bool {
            self.ready.get()
        }

        fn controller_fatal(&self) -> bool {
            self.fatal.get()
        }

        unsafe fn prepare_reinitialize(&self) -> Result<(), InitError> {
            self.prepare_count.set(self.prepare_count.get() + 1);
            Ok(())
        }

        fn queue_count(&self) -> usize {
            self.queue_count
        }

        fn admin_irq_source(&self) -> Option<usize> {
            Some(0)
        }

        fn submit_admin_command(&self, command: AdminCommand) -> Result<u16, InitError> {
            let command_id = self.next_command_id.get();
            self.next_command_id.set(command_id + 1);
            self.commands.borrow_mut().push(command);
            Ok(command_id)
        }

        fn take_admin_completion(&self) -> Option<AdminCompletion> {
            self.completions.borrow_mut().pop_front()
        }

        fn complete_admin_command(
            &self,
            command: AdminCommand,
            completion: AdminCompletion,
        ) -> Result<Option<AdminCommand>, InitError> {
            let next = match command {
                AdminCommand::IdentifyController => Some(AdminCommand::SetQueueCount {
                    count: self.queue_count,
                }),
                AdminCommand::SetQueueCount { count }
                    if super::queue_count_supported(completion.result, count) =>
                {
                    Some(AdminCommand::CreateCompletionQueue { queue_index: 0 })
                }
                AdminCommand::SetQueueCount { .. } => {
                    return Err(InitError::Hardware("queue count changed"));
                }
                AdminCommand::CreateCompletionQueue { queue_index } => {
                    Some(AdminCommand::CreateSubmissionQueue { queue_index })
                }
                AdminCommand::CreateSubmissionQueue { queue_index } => {
                    let next = queue_index.saturating_add(1);
                    if next < self.queue_count {
                        Some(AdminCommand::CreateCompletionQueue { queue_index: next })
                    } else {
                        Some(AdminCommand::IdentifyNamespaceList)
                    }
                }
                AdminCommand::IdentifyNamespaceList => {
                    Some(AdminCommand::IdentifyNamespace { namespace_id: 9 })
                }
                AdminCommand::IdentifyNamespace { namespace_id: 9 } => None,
                AdminCommand::IdentifyNamespace { .. } => {
                    return Err(InitError::Hardware("namespace changed"));
                }
            };
            Ok(next)
        }
    }

    #[test]
    fn quiesce_waits_without_spinning_and_proves_dma_only_after_ready_clears() {
        let hardware = FakeHardware::new(1);
        let mut lifecycle = NvmeLifecycle::new();
        let epoch = ControllerEpoch::new(9);

        lifecycle
            .begin_dma_quiesce(&hardware, epoch, RecoveryCause::QueueFault { queue_id: 0 })
            .expect("quiesce must start without blocking");
        assert_eq!(hardware.disable_count.get(), 1);

        let pending = lifecycle.poll_dma_quiesce(&hardware, InitInput::at(10));
        let InitPoll::Pending(schedule) = pending else {
            panic!("ready controller must schedule a later status check")
        };
        assert!(schedule.irq_sources().is_empty());
        assert_eq!(schedule.wake_at_ns(), Some(1_000_010));

        hardware.ready.set(false);
        let InitPoll::Ready(proof) =
            lifecycle.poll_dma_quiesce(&hardware, InitInput::new(20, IdList::none()))
        else {
            panic!("RDY=0 must produce the linear DMA proof")
        };
        assert_eq!(proof.epoch(), epoch);
        assert_eq!(proof.controller_cookie(), hardware.controller_cookie());
    }

    #[test]
    fn quiesce_timeout_uses_absolute_time_not_poll_count() {
        let hardware = FakeHardware::new(1);
        let mut lifecycle = NvmeLifecycle::new();
        lifecycle
            .begin_dma_quiesce(&hardware, ControllerEpoch::new(1), RecoveryCause::Handoff)
            .unwrap();

        assert!(matches!(
            lifecycle.poll_dma_quiesce(&hardware, InitInput::at(100)),
            InitPoll::Pending(_)
        ));
        assert!(matches!(
            lifecycle.poll_dma_quiesce(&hardware, InitInput::at(500_000_099)),
            InitPoll::Pending(_)
        ));
        assert!(matches!(
            lifecycle.poll_dma_quiesce(&hardware, InitInput::at(500_000_100)),
            InitPoll::Failed(InitError::TimedOut)
        ));
    }

    #[test]
    fn guest_return_starts_a_fresh_quiescence_epoch() {
        let hardware = FakeHardware::new(1);
        let mut lifecycle = NvmeLifecycle::new();
        let handoff_epoch = ControllerEpoch::new(4);
        hardware.ready.set(false);
        lifecycle
            .begin_dma_quiesce(&hardware, handoff_epoch, RecoveryCause::Handoff)
            .unwrap();
        let InitPoll::Ready(proof) = lifecycle.poll_dma_quiesce(&hardware, InitInput::at(0)) else {
            panic!("disabled controller must produce the handoff proof")
        };
        lifecycle.enter_guest_owned(&hardware, proof).unwrap();

        hardware.ready.set(true);
        let return_epoch = ControllerEpoch::new(5);
        lifecycle
            .begin_dma_quiesce(&hardware, return_epoch, RecoveryCause::Handoff)
            .unwrap();
        assert_eq!(hardware.disable_count.get(), 2);
        assert!(matches!(
            lifecycle.poll_dma_quiesce(&hardware, InitInput::at(1)),
            InitPoll::Pending(_)
        ));
    }

    #[test]
    fn reinitialize_timeout_disables_the_controller_before_failure() {
        let hardware = FakeHardware::new(1);
        let mut lifecycle = NvmeLifecycle::new();
        let epoch = ControllerEpoch::new(6);
        hardware.ready.set(false);
        lifecycle
            .begin_dma_quiesce(&hardware, epoch, RecoveryCause::Handoff)
            .unwrap();
        let InitPoll::Ready(proof) = lifecycle.poll_dma_quiesce(&hardware, InitInput::at(0)) else {
            panic!("disabled controller must produce a reinitialization proof")
        };
        lifecycle.begin_reinitialize(&hardware, proof).unwrap();
        hardware.ready.set(true);
        assert!(matches!(
            lifecycle.poll_reinitialize(&hardware, InitInput::at(1)),
            InitPoll::Pending(schedule) if schedule.run_again()
        ));
        assert!(matches!(
            lifecycle.poll_reinitialize(&hardware, InitInput::at(2)),
            InitPoll::Pending(schedule) if schedule.irq_sources().contains(0)
        ));

        assert!(matches!(
            lifecycle.poll_reinitialize(
                &hardware,
                InitInput::at(2_u64.saturating_add(ADMIN_COMMAND_TIMEOUT_NS)),
            ),
            InitPoll::Pending(_)
        ));
        assert_eq!(hardware.disable_count.get(), 2);

        hardware.ready.set(false);
        assert!(matches!(
            lifecycle.poll_reinitialize(
                &hardware,
                InitInput::at(3_u64.saturating_add(ADMIN_COMMAND_TIMEOUT_NS)),
            ),
            InitPoll::Failed(InitError::TimedOut)
        ));
    }

    #[test]
    fn reinitialize_rebuilds_each_queue_only_from_irq_cached_admin_completions() {
        let hardware = FakeHardware::new(2);
        let mut lifecycle = NvmeLifecycle::new();
        let epoch = ControllerEpoch::new(3);
        hardware.ready.set(false);
        lifecycle
            .begin_dma_quiesce(&hardware, epoch, RecoveryCause::Handoff)
            .unwrap();
        let InitPoll::Ready(proof) = lifecycle.poll_dma_quiesce(&hardware, InitInput::at(0)) else {
            panic!("disabled fake controller must quiesce")
        };

        lifecycle.begin_reinitialize(&hardware, proof).unwrap();
        assert_eq!(hardware.prepare_count.get(), 1);
        hardware.ready.set(true);
        assert!(matches!(
            lifecycle.poll_reinitialize(&hardware, InitInput::at(10)),
            InitPoll::Pending(schedule) if schedule.run_again()
        ));

        let expected = [
            AdminCommand::IdentifyController,
            AdminCommand::SetQueueCount { count: 2 },
            AdminCommand::CreateCompletionQueue { queue_index: 0 },
            AdminCommand::CreateSubmissionQueue { queue_index: 0 },
            AdminCommand::CreateCompletionQueue { queue_index: 1 },
            AdminCommand::CreateSubmissionQueue { queue_index: 1 },
            AdminCommand::IdentifyNamespaceList,
            AdminCommand::IdentifyNamespace { namespace_id: 9 },
        ];
        for (index, expected_command) in expected.into_iter().enumerate() {
            let now = 20 + index as u64 * 10;
            let InitPoll::Pending(wait) =
                lifecycle.poll_reinitialize(&hardware, InitInput::at(now))
            else {
                panic!("issuing an admin command must arm IRQ plus watchdog")
            };
            assert!(wait.irq_sources().contains(0));
            assert_eq!(hardware.commands.borrow()[index], expected_command);

            let set_queue_result = if matches!(expected_command, AdminCommand::SetQueueCount { .. })
            {
                0x0001_0001
            } else {
                0
            };
            hardware.complete_last_command(set_queue_result);
            if index == 1 {
                let without_irq = lifecycle.poll_reinitialize(&hardware, InitInput::at(now + 1));
                assert!(matches!(without_irq, InitPoll::Pending(_)));
                assert_eq!(
                    hardware.completions.borrow().len(),
                    1,
                    "a deadline/run-again pass must not consume an admin CQ cache entry"
                );
            }
            let progress = lifecycle
                .poll_reinitialize(&hardware, InitInput::new(now + 2, IdList::from_bits(1)));
            if index + 1 == expected.len() {
                let InitPoll::Ready(ready) = progress else {
                    panic!("last SQ completion must publish ready proof")
                };
                assert_eq!(ready.epoch(), epoch);
                assert_eq!(ready.controller_cookie(), hardware.controller_cookie());
            } else {
                assert!(matches!(
                    progress,
                    InitPoll::Pending(schedule) if schedule.run_again()
                ));
            }
        }
    }
}
