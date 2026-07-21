//! Bounded discovery-to-ready NVMe controller initialization.

use rdif_block::{IdList, InitError, InitInput, InitPoll, InitSchedule};

use crate::lifecycle::AdminCompletion;

const CONTROLLER_STATUS_CHECK_INTERVAL_NS: u64 = 1_000_000;
const ADMIN_COMMAND_TIMEOUT_NS: u64 = 30_000_000_000;

/// One serialized admin command in the initial controller transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InitialAdminCommand {
    IdentifyController,
    SetQueueCount,
    CreateCompletionQueue { queue_index: usize },
    CreateSubmissionQueue { queue_index: usize },
    IdentifyNamespaceList,
    IdentifyNamespace { namespace_id: u32 },
}

/// Narrow register/DMA boundary consumed by the portable init state machine.
pub(crate) trait InitialHardware {
    fn controller_timeout_ns(&self) -> u64;
    fn begin_controller_disable(&mut self);
    fn controller_ready(&self) -> bool;
    fn controller_fatal(&self) -> bool;
    /// Returns the admin source only after its handler and delivery path are live.
    fn live_admin_irq_source(&self) -> Option<usize>;

    /// Programs the preallocated admin queue and enables the controller.
    ///
    /// # Safety
    ///
    /// The controller must have acknowledged `CC.RDY=0`, and the OS must keep
    /// the registered initialization IRQ action live for the following admin
    /// commands.
    unsafe fn prepare_initial_enable(&mut self) -> Result<(), InitError>;

    fn submit_initial_admin(&mut self, command: InitialAdminCommand) -> Result<u16, InitError>;
    fn take_admin_completion(&mut self) -> Option<AdminCompletion>;

    /// Applies DMA output and returns the next serialized command.
    ///
    /// `Ok(None)` means namespace geometry and every retained I/O queue are
    /// ready for publication.
    fn complete_initial_admin(
        &mut self,
        command: InitialAdminCommand,
        completion: AdminCompletion,
    ) -> Result<Option<InitialAdminCommand>, InitError>;

    /// Publishes the fully validated namespace to the block-facing owner.
    ///
    /// This remains part of the initialization transaction: failure must
    /// disable the controller before the runtime can observe terminal failure.
    fn publish_ready(&mut self) -> Result<(), InitError>;
}

/// Initial controller lifecycle. It never sleeps or inspects a CQ on timeout.
pub(crate) struct NvmeInitialization {
    state: InitializationState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InitializationState {
    Discovered,
    Disabling {
        deadline_ns: u64,
    },
    Enabling {
        deadline_ns: u64,
    },
    Issue {
        command: InitialAdminCommand,
    },
    Wait {
        command: InitialAdminCommand,
        command_id: u16,
        deadline_ns: u64,
    },
    Aborting {
        error: InitError,
        deadline_ns: u64,
    },
    Ready,
    Failed,
}

impl NvmeInitialization {
    pub(crate) const fn discovered() -> Self {
        Self {
            state: InitializationState::Discovered,
        }
    }

    pub(crate) const fn is_ready(&self) -> bool {
        matches!(self.state, InitializationState::Ready)
    }

    pub(crate) fn poll(
        &mut self,
        hardware: &mut impl InitialHardware,
        input: InitInput,
    ) -> InitPoll<()> {
        match self.state {
            InitializationState::Discovered => self.begin_disable(hardware, input.now_ns),
            InitializationState::Disabling { deadline_ns } => {
                self.poll_disabling(hardware, input.now_ns, deadline_ns)
            }
            InitializationState::Enabling { deadline_ns } => {
                self.poll_enabling(hardware, input.now_ns, deadline_ns)
            }
            InitializationState::Issue { command } => {
                self.issue_admin(hardware, input.now_ns, command)
            }
            InitializationState::Wait {
                command,
                command_id,
                deadline_ns,
            } => self.poll_admin(hardware, input, command, command_id, deadline_ns),
            InitializationState::Aborting { error, deadline_ns } => {
                self.poll_aborting(hardware, input.now_ns, error, deadline_ns)
            }
            InitializationState::Ready | InitializationState::Failed => {
                InitPoll::Failed(InitError::InvalidState)
            }
        }
    }

    fn begin_disable(&mut self, hardware: &mut impl InitialHardware, now_ns: u64) -> InitPoll<()> {
        if hardware.live_admin_irq_source().is_none() {
            return self.finish_failure(InitError::MissingInterrupt);
        }
        hardware.begin_controller_disable();
        let deadline_ns = now_ns.saturating_add(hardware.controller_timeout_ns());
        self.state = InitializationState::Disabling { deadline_ns };
        InitPoll::Pending(status_check_schedule(now_ns, deadline_ns))
    }

    fn poll_disabling(
        &mut self,
        hardware: &mut impl InitialHardware,
        now_ns: u64,
        deadline_ns: u64,
    ) -> InitPoll<()> {
        if !hardware.controller_ready() {
            let result = unsafe {
                // SAFETY: RDY=0 is the controller acknowledgement required by
                // InitialHardware, and init IRQ actions were bound before the
                // runtime made the first poll call.
                hardware.prepare_initial_enable()
            };
            if let Err(error) = result {
                return self.begin_abort(hardware, now_ns, error);
            }
            let deadline_ns = now_ns.saturating_add(hardware.controller_timeout_ns());
            self.state = InitializationState::Enabling { deadline_ns };
            return InitPoll::Pending(status_check_schedule(now_ns, deadline_ns));
        }
        if now_ns >= deadline_ns {
            return self.finish_failure(InitError::TimedOut);
        }
        InitPoll::Pending(status_check_schedule(now_ns, deadline_ns))
    }

    fn poll_enabling(
        &mut self,
        hardware: &mut impl InitialHardware,
        now_ns: u64,
        deadline_ns: u64,
    ) -> InitPoll<()> {
        if hardware.controller_fatal() {
            return self.begin_abort(
                hardware,
                now_ns,
                InitError::Hardware("NVMe controller reported fatal status while enabling"),
            );
        }
        if hardware.controller_ready() {
            self.state = InitializationState::Issue {
                command: InitialAdminCommand::IdentifyController,
            };
            return InitPoll::Pending(InitSchedule::immediate());
        }
        if now_ns >= deadline_ns {
            return self.begin_abort(hardware, now_ns, InitError::TimedOut);
        }
        InitPoll::Pending(status_check_schedule(now_ns, deadline_ns))
    }

    fn issue_admin(
        &mut self,
        hardware: &mut impl InitialHardware,
        now_ns: u64,
        command: InitialAdminCommand,
    ) -> InitPoll<()> {
        let Some(source_id) = hardware.live_admin_irq_source() else {
            return self.begin_abort(hardware, now_ns, InitError::MissingInterrupt);
        };
        let command_id = match hardware.submit_initial_admin(command) {
            Ok(command_id) => command_id,
            Err(error) => return self.begin_abort(hardware, now_ns, error),
        };
        let deadline_ns = now_ns.saturating_add(ADMIN_COMMAND_TIMEOUT_NS);
        self.state = InitializationState::Wait {
            command,
            command_id,
            deadline_ns,
        };
        InitPoll::Pending(admin_wait_schedule(source_id, deadline_ns))
    }

    fn poll_admin(
        &mut self,
        hardware: &mut impl InitialHardware,
        input: InitInput,
        command: InitialAdminCommand,
        command_id: u16,
        deadline_ns: u64,
    ) -> InitPoll<()> {
        if hardware.controller_fatal() {
            return self.begin_abort(
                hardware,
                input.now_ns,
                InitError::Hardware("NVMe controller reported fatal status during initialization"),
            );
        }
        let Some(source_id) = hardware.live_admin_irq_source() else {
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
                InitError::Hardware("NVMe initialization admin command failed"),
            );
        }

        match hardware.complete_initial_admin(command, completion) {
            Ok(Some(command)) => {
                self.state = InitializationState::Issue { command };
                InitPoll::Pending(InitSchedule::immediate())
            }
            Ok(None) => {
                if let Err(error) = hardware.publish_ready() {
                    return self.begin_abort(hardware, input.now_ns, error);
                }
                self.state = InitializationState::Ready;
                InitPoll::Ready(())
            }
            Err(error) => self.begin_abort(hardware, input.now_ns, error),
        }
    }

    fn begin_abort<T>(
        &mut self,
        hardware: &mut impl InitialHardware,
        now_ns: u64,
        error: InitError,
    ) -> InitPoll<T> {
        hardware.begin_controller_disable();
        let deadline_ns = now_ns.saturating_add(hardware.controller_timeout_ns());
        self.state = InitializationState::Aborting { error, deadline_ns };
        self.poll_aborting(hardware, now_ns, error, deadline_ns)
    }

    fn poll_aborting<T>(
        &mut self,
        hardware: &mut impl InitialHardware,
        now_ns: u64,
        error: InitError,
        deadline_ns: u64,
    ) -> InitPoll<T> {
        if !hardware.controller_ready() || now_ns >= deadline_ns {
            return self.finish_failure(error);
        }
        InitPoll::Pending(status_check_schedule(now_ns, deadline_ns))
    }

    fn finish_failure<T>(&mut self, error: InitError) -> InitPoll<T> {
        self.state = InitializationState::Failed;
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

#[cfg(test)]
mod tests {
    use alloc::{collections::VecDeque, vec::Vec};
    use core::cell::{Cell, RefCell};

    use super::*;

    struct FakeHardware {
        irq_ready: Cell<bool>,
        ready: Cell<bool>,
        fatal: Cell<bool>,
        prepared: Cell<bool>,
        disable_count: Cell<usize>,
        publish_error: Cell<Option<InitError>>,
        published: Cell<bool>,
        submitted: RefCell<Vec<InitialAdminCommand>>,
        completions: RefCell<VecDeque<AdminCompletion>>,
    }

    impl FakeHardware {
        fn new() -> Self {
            Self {
                irq_ready: Cell::new(true),
                ready: Cell::new(true),
                fatal: Cell::new(false),
                prepared: Cell::new(false),
                disable_count: Cell::new(0),
                publish_error: Cell::new(None),
                published: Cell::new(false),
                submitted: RefCell::new(Vec::new()),
                completions: RefCell::new(VecDeque::new()),
            }
        }

        fn complete(&self, command_id: u16) {
            self.completions.borrow_mut().push_back(AdminCompletion {
                command_id,
                success: true,
                result: 0,
            });
        }
    }

    impl InitialHardware for FakeHardware {
        fn controller_timeout_ns(&self) -> u64 {
            100
        }

        fn begin_controller_disable(&mut self) {
            self.disable_count.set(self.disable_count.get() + 1);
        }

        fn controller_ready(&self) -> bool {
            self.ready.get()
        }

        fn controller_fatal(&self) -> bool {
            self.fatal.get()
        }

        fn live_admin_irq_source(&self) -> Option<usize> {
            self.irq_ready.get().then_some(3)
        }

        unsafe fn prepare_initial_enable(&mut self) -> Result<(), InitError> {
            self.prepared.set(true);
            Ok(())
        }

        fn submit_initial_admin(&mut self, command: InitialAdminCommand) -> Result<u16, InitError> {
            self.submitted.borrow_mut().push(command);
            Ok(7)
        }

        fn take_admin_completion(&mut self) -> Option<AdminCompletion> {
            self.completions.borrow_mut().pop_front()
        }

        fn complete_initial_admin(
            &mut self,
            command: InitialAdminCommand,
            _completion: AdminCompletion,
        ) -> Result<Option<InitialAdminCommand>, InitError> {
            match command {
                InitialAdminCommand::IdentifyController => {
                    Ok(Some(InitialAdminCommand::SetQueueCount))
                }
                InitialAdminCommand::SetQueueCount => {
                    Ok(Some(InitialAdminCommand::CreateCompletionQueue {
                        queue_index: 0,
                    }))
                }
                InitialAdminCommand::CreateCompletionQueue { queue_index } => {
                    Ok(Some(InitialAdminCommand::CreateSubmissionQueue {
                        queue_index,
                    }))
                }
                InitialAdminCommand::CreateSubmissionQueue { queue_index: 0 } => {
                    Ok(Some(InitialAdminCommand::CreateCompletionQueue {
                        queue_index: 1,
                    }))
                }
                InitialAdminCommand::CreateSubmissionQueue { queue_index: 1 } => {
                    Ok(Some(InitialAdminCommand::IdentifyNamespaceList))
                }
                InitialAdminCommand::IdentifyNamespaceList => {
                    Ok(Some(InitialAdminCommand::IdentifyNamespace {
                        namespace_id: 9,
                    }))
                }
                InitialAdminCommand::IdentifyNamespace { namespace_id: 9 } => Ok(None),
                _ => Err(InitError::InvalidState),
            }
        }

        fn publish_ready(&mut self) -> Result<(), InitError> {
            if let Some(error) = self.publish_error.get() {
                return Err(error);
            }
            self.published.set(true);
            Ok(())
        }
    }

    #[test]
    fn initialization_does_not_touch_hardware_before_irq_delivery_is_live() {
        let mut hardware = FakeHardware::new();
        hardware.irq_ready.set(false);
        let mut initialization = NvmeInitialization::discovered();

        assert!(matches!(
            initialization.poll(&mut hardware, InitInput::at(0)),
            InitPoll::Failed(InitError::MissingInterrupt)
        ));
        assert_eq!(hardware.disable_count.get(), 0);
        assert!(hardware.submitted.borrow().is_empty());
    }

    #[test]
    fn first_controller_command_occurs_only_after_disable_acknowledgement() {
        let mut hardware = FakeHardware::new();
        let mut initialization = NvmeInitialization::discovered();

        assert!(matches!(
            initialization.poll(&mut hardware, InitInput::at(0)),
            InitPoll::Pending(_)
        ));
        assert!(hardware.submitted.borrow().is_empty());

        hardware.ready.set(false);
        assert!(matches!(
            initialization.poll(&mut hardware, InitInput::at(1)),
            InitPoll::Pending(_)
        ));
        assert!(hardware.prepared.get());
        assert!(hardware.submitted.borrow().is_empty());

        hardware.ready.set(true);
        assert!(matches!(
            initialization.poll(&mut hardware, InitInput::at(2)),
            InitPoll::Pending(schedule) if schedule.run_again()
        ));
        assert!(hardware.submitted.borrow().is_empty());
        assert!(matches!(
            initialization.poll(&mut hardware, InitInput::at(3)),
            InitPoll::Pending(schedule) if schedule.irq_sources().contains(3)
        ));
        assert_eq!(
            *hardware.submitted.borrow(),
            [InitialAdminCommand::IdentifyController]
        );
    }

    #[test]
    fn deadline_does_not_consume_admin_completion_without_irq_evidence() {
        let mut hardware = FakeHardware::new();
        let mut initialization = NvmeInitialization::discovered();
        initialization.poll(&mut hardware, InitInput::at(0));
        hardware.ready.set(false);
        initialization.poll(&mut hardware, InitInput::at(1));
        hardware.ready.set(true);
        initialization.poll(&mut hardware, InitInput::at(2));
        initialization.poll(&mut hardware, InitInput::at(3));
        hardware.complete(7);

        assert!(matches!(
            initialization.poll(&mut hardware, InitInput::at(10)),
            InitPoll::Pending(_)
        ));
        assert_eq!(hardware.completions.borrow().len(), 1);

        let mut sources = IdList::none();
        sources.insert(3);
        assert!(matches!(
            initialization.poll(&mut hardware, InitInput::new(11, sources)),
            InitPoll::Pending(schedule) if schedule.run_again()
        ));
        assert!(hardware.completions.borrow().is_empty());
    }

    #[test]
    fn admin_timeout_disables_the_controller_before_reporting_failure() {
        let mut hardware = FakeHardware::new();
        let mut initialization = NvmeInitialization::discovered();
        initialization.poll(&mut hardware, InitInput::at(0));
        hardware.ready.set(false);
        initialization.poll(&mut hardware, InitInput::at(1));
        hardware.ready.set(true);
        initialization.poll(&mut hardware, InitInput::at(2));
        initialization.poll(&mut hardware, InitInput::at(3));

        assert!(matches!(
            initialization.poll(
                &mut hardware,
                InitInput::at(3_u64.saturating_add(ADMIN_COMMAND_TIMEOUT_NS)),
            ),
            InitPoll::Pending(_)
        ));
        assert_eq!(
            hardware.disable_count.get(),
            2,
            "initial disable plus failure abort must both be issued"
        );

        hardware.ready.set(false);
        assert!(matches!(
            initialization.poll(
                &mut hardware,
                InitInput::at(4_u64.saturating_add(ADMIN_COMMAND_TIMEOUT_NS)),
            ),
            InitPoll::Failed(InitError::TimedOut)
        ));
    }

    #[test]
    fn namespace_publication_failure_is_part_of_the_disable_transaction() {
        let mut hardware = FakeHardware::new();
        hardware
            .publish_error
            .set(Some(InitError::Hardware("namespace publication rejected")));
        hardware.complete(7);
        let mut initialization = NvmeInitialization {
            state: InitializationState::Wait {
                command: InitialAdminCommand::IdentifyNamespace { namespace_id: 9 },
                command_id: 7,
                deadline_ns: 100,
            },
        };
        let mut sources = IdList::none();
        sources.insert(3);

        assert!(matches!(
            initialization.poll(&mut hardware, InitInput::new(1, sources)),
            InitPoll::Pending(_)
        ));
        assert!(!hardware.published.get());
        assert_eq!(hardware.disable_count.get(), 1);

        hardware.ready.set(false);
        assert!(matches!(
            initialization.poll(&mut hardware, InitInput::at(2)),
            InitPoll::Failed(InitError::Hardware("namespace publication rejected"))
        ));
    }

    #[test]
    fn full_initialization_serializes_every_admin_command_behind_irq_evidence() {
        let mut hardware = FakeHardware::new();
        let mut initialization = NvmeInitialization::discovered();
        initialization.poll(&mut hardware, InitInput::at(0));
        hardware.ready.set(false);
        initialization.poll(&mut hardware, InitInput::at(1));
        hardware.ready.set(true);
        initialization.poll(&mut hardware, InitInput::at(2));

        let expected = [
            InitialAdminCommand::IdentifyController,
            InitialAdminCommand::SetQueueCount,
            InitialAdminCommand::CreateCompletionQueue { queue_index: 0 },
            InitialAdminCommand::CreateSubmissionQueue { queue_index: 0 },
            InitialAdminCommand::CreateCompletionQueue { queue_index: 1 },
            InitialAdminCommand::CreateSubmissionQueue { queue_index: 1 },
            InitialAdminCommand::IdentifyNamespaceList,
            InitialAdminCommand::IdentifyNamespace { namespace_id: 9 },
        ];
        for (index, expected_command) in expected.into_iter().enumerate() {
            let now_ns = 3 + index as u64 * 2;
            assert!(matches!(
                initialization.poll(&mut hardware, InitInput::at(now_ns)),
                InitPoll::Pending(schedule) if schedule.irq_sources().contains(3)
            ));
            assert_eq!(hardware.submitted.borrow()[index], expected_command);

            hardware.complete(7);
            let mut sources = IdList::none();
            sources.insert(3);
            let completion =
                initialization.poll(&mut hardware, InitInput::new(now_ns + 1, sources));
            if index + 1 == expected.len() {
                assert!(matches!(completion, InitPoll::Ready(())));
            } else {
                assert!(matches!(
                    completion,
                    InitPoll::Pending(schedule) if schedule.run_again()
                ));
            }
        }
    }
}
