use alloc::string::{String, ToString};

use super::{StopReason, VmStatus};
use crate::{AxVmError, AxVmResult};

pub enum Machine<R, H = ()> {
    Ready(R),
    Running {
        resources: R,
        runtime: H,
    },
    Pausing {
        resources: R,
        runtime: H,
    },
    Paused {
        resources: R,
        runtime: H,
    },
    Stopping {
        resources: Option<R>,
        runtime: Option<H>,
        reason: StopReason,
    },
    Stopped {
        resources: Option<R>,
        runtime: Option<H>,
        reason: StopReason,
    },
    Destroying,
    Destroyed,
    Failed(String),
    Switching,
}

impl<R, H> Machine<R, H> {
    pub fn status(&self) -> VmStatus {
        match self {
            Machine::Ready(_) => VmStatus::Ready,
            Machine::Running { .. } => VmStatus::Running,
            Machine::Pausing { .. } => VmStatus::Pausing,
            Machine::Paused { .. } => VmStatus::Paused,
            Machine::Stopping { .. } => VmStatus::Stopping,
            Machine::Stopped { .. } => VmStatus::Stopped,
            Machine::Destroying => VmStatus::Destroying,
            Machine::Destroyed => VmStatus::Destroyed,
            Machine::Failed(_) => VmStatus::Failed,
            Machine::Switching => VmStatus::Failed,
        }
    }

    pub fn resources(&self) -> Option<&R> {
        match self {
            Machine::Ready(resources)
            | Machine::Running { resources, .. }
            | Machine::Pausing { resources, .. }
            | Machine::Paused { resources, .. } => Some(resources),
            Machine::Stopping { resources, .. } | Machine::Stopped { resources, .. } => {
                resources.as_ref()
            }
            _ => None,
        }
    }

    pub fn resources_mut(&mut self) -> Option<&mut R> {
        match self {
            Machine::Ready(resources)
            | Machine::Running { resources, .. }
            | Machine::Pausing { resources, .. }
            | Machine::Paused { resources, .. } => Some(resources),
            Machine::Stopping { resources, .. } | Machine::Stopped { resources, .. } => {
                resources.as_mut()
            }
            _ => None,
        }
    }

    pub fn runtime(&self) -> Option<&H> {
        match self {
            Machine::Running { runtime, .. }
            | Machine::Pausing { runtime, .. }
            | Machine::Paused { runtime, .. } => Some(runtime),
            Machine::Stopping { runtime, .. } => runtime.as_ref(),
            _ => None,
        }
    }

    pub fn runtime_mut(&mut self) -> Option<&mut H> {
        match self {
            Machine::Running { runtime, .. }
            | Machine::Pausing { runtime, .. }
            | Machine::Paused { runtime, .. } => Some(runtime),
            Machine::Stopping { runtime, .. } => runtime.as_mut(),
            _ => None,
        }
    }

    pub fn start_with<F>(&mut self, f: F) -> AxVmResult
    where
        F: FnOnce(&mut R) -> AxVmResult<H>,
    {
        let old = core::mem::replace(self, Machine::Switching);
        match old {
            Machine::Ready(mut resources) => match f(&mut resources) {
                Ok(runtime) => {
                    *self = Machine::Running { resources, runtime };
                    Ok(())
                }
                Err(err) => {
                    *self = Machine::Failed(err.to_string());
                    Err(err)
                }
            },
            Machine::Stopped {
                resources: Some(mut resources),
                runtime: None,
                reason,
            } => match f(&mut resources) {
                Ok(runtime) => {
                    *self = Machine::Running { resources, runtime };
                    Ok(())
                }
                Err(err) => {
                    *self = Machine::Stopped {
                        resources: Some(resources),
                        runtime: None,
                        reason,
                    };
                    Err(err)
                }
            },
            Machine::Stopped {
                resources,
                runtime: Some(runtime),
                reason,
            } => {
                *self = Machine::Stopped {
                    resources,
                    runtime: Some(runtime),
                    reason,
                };
                Err(AxVmError::invalid_transition(
                    VmStatus::Stopped,
                    VmStatus::Running,
                    "start",
                ))
            }
            other => {
                let from = other.status();
                *self = other;
                Err(AxVmError::invalid_transition(
                    from,
                    VmStatus::Running,
                    "start",
                ))
            }
        }
    }

    pub fn pause(&mut self) -> AxVmResult {
        let old = core::mem::replace(self, Machine::Switching);
        match old {
            Machine::Running { resources, runtime } => {
                *self = Machine::Paused { resources, runtime };
                Ok(())
            }
            other => {
                let from = other.status();
                *self = other;
                Err(AxVmError::invalid_transition(
                    from,
                    VmStatus::Paused,
                    "pause",
                ))
            }
        }
    }

    pub fn resume(&mut self) -> AxVmResult {
        let old = core::mem::replace(self, Machine::Switching);
        match old {
            Machine::Paused { resources, runtime } => {
                *self = Machine::Running { resources, runtime };
                Ok(())
            }
            other => {
                let from = other.status();
                *self = other;
                Err(AxVmError::invalid_transition(
                    from,
                    VmStatus::Running,
                    "resume",
                ))
            }
        }
    }

    pub fn stop_with<F>(&mut self, reason: StopReason, f: F) -> AxVmResult
    where
        F: FnOnce(Option<&mut R>, &StopReason) -> AxVmResult,
    {
        let old = core::mem::replace(self, Machine::Switching);
        match old {
            Machine::Ready(resources) => {
                let mut resources = Some(resources);
                if let Err(err) = f(resources.as_mut(), &reason) {
                    *self = Machine::Failed(err.to_string());
                    return Err(err);
                }
                *self = Machine::Stopped {
                    resources,
                    runtime: None,
                    reason,
                };
                Ok(())
            }
            Machine::Running { resources, runtime } => {
                *self = Machine::Running { resources, runtime };
                Err(AxVmError::invalid_transition(
                    VmStatus::Running,
                    VmStatus::Stopped,
                    "stop",
                ))
            }
            Machine::Pausing { resources, runtime } => {
                *self = Machine::Pausing { resources, runtime };
                Err(AxVmError::invalid_transition(
                    VmStatus::Pausing,
                    VmStatus::Stopped,
                    "stop",
                ))
            }
            Machine::Paused { resources, runtime } => {
                *self = Machine::Paused { resources, runtime };
                Err(AxVmError::invalid_transition(
                    VmStatus::Paused,
                    VmStatus::Stopped,
                    "stop",
                ))
            }
            Machine::Stopped {
                resources,
                runtime,
                reason,
            } => {
                *self = Machine::Stopped {
                    resources,
                    runtime,
                    reason,
                };
                Ok(())
            }
            other => {
                let from = other.status();
                *self = other;
                Err(AxVmError::invalid_transition(
                    from,
                    VmStatus::Stopped,
                    "stop",
                ))
            }
        }
    }

    pub fn request_stop_with<F>(&mut self, reason: StopReason, f: F) -> AxVmResult
    where
        F: FnOnce(Option<&mut R>, &StopReason) -> AxVmResult,
    {
        let old = core::mem::replace(self, Machine::Switching);
        match old {
            Machine::Ready(mut resources) => {
                f(Some(&mut resources), &reason)?;
                *self = Machine::Stopped {
                    resources: Some(resources),
                    runtime: None,
                    reason,
                };
                Ok(())
            }
            Machine::Running {
                mut resources,
                runtime,
            }
            | Machine::Pausing {
                mut resources,
                runtime,
            }
            | Machine::Paused {
                mut resources,
                runtime,
            } => {
                f(Some(&mut resources), &reason)?;
                *self = Machine::Stopping {
                    resources: Some(resources),
                    runtime: Some(runtime),
                    reason,
                };
                Ok(())
            }
            Machine::Stopping {
                resources,
                runtime,
                reason,
            } => {
                *self = Machine::Stopping {
                    resources,
                    runtime,
                    reason,
                };
                Ok(())
            }
            Machine::Stopped {
                resources,
                runtime,
                reason,
            } => {
                *self = Machine::Stopped {
                    resources,
                    runtime,
                    reason,
                };
                Ok(())
            }
            other => {
                let from = other.status();
                *self = other;
                Err(AxVmError::invalid_transition(
                    from,
                    VmStatus::Stopping,
                    "request_stop",
                ))
            }
        }
    }

    pub fn finish_stop(&mut self) -> AxVmResult {
        let old = core::mem::replace(self, Machine::Switching);
        match old {
            Machine::Stopping {
                resources,
                runtime,
                reason,
            } => {
                *self = Machine::Stopped {
                    resources,
                    runtime,
                    reason,
                };
                Ok(())
            }
            Machine::Stopped {
                resources,
                runtime,
                reason,
            } => {
                *self = Machine::Stopped {
                    resources,
                    runtime,
                    reason,
                };
                Ok(())
            }
            other => {
                let from = other.status();
                *self = other;
                Err(AxVmError::invalid_transition(
                    from,
                    VmStatus::Stopped,
                    "finish_stop",
                ))
            }
        }
    }

    pub fn take_stopped_runtime(&mut self) -> Option<H> {
        match self {
            Machine::Stopped { runtime, .. } => runtime.take(),
            _ => None,
        }
    }

    pub fn reset_with<F>(&mut self, f: F) -> AxVmResult
    where
        F: FnOnce(&mut R) -> AxVmResult,
    {
        let old = core::mem::replace(self, Machine::Switching);
        match old {
            Machine::Ready(mut resources) => {
                f(&mut resources)?;
                *self = Machine::Ready(resources);
                Ok(())
            }
            Machine::Stopped {
                resources: Some(mut resources),
                runtime: None,
                ..
            } => {
                f(&mut resources)?;
                *self = Machine::Ready(resources);
                Ok(())
            }
            Machine::Stopping {
                resources,
                runtime,
                reason,
            } => {
                *self = Machine::Stopping {
                    resources,
                    runtime,
                    reason,
                };
                Err(AxVmError::invalid_transition(
                    VmStatus::Stopping,
                    VmStatus::Ready,
                    "reset",
                ))
            }
            Machine::Running { resources, runtime } => {
                *self = Machine::Running { resources, runtime };
                Err(AxVmError::invalid_transition(
                    VmStatus::Running,
                    VmStatus::Ready,
                    "reset",
                ))
            }
            Machine::Paused { resources, runtime } => {
                *self = Machine::Paused { resources, runtime };
                Err(AxVmError::invalid_transition(
                    VmStatus::Paused,
                    VmStatus::Ready,
                    "reset",
                ))
            }
            Machine::Stopped {
                resources,
                runtime: Some(runtime),
                reason,
            } => {
                *self = Machine::Stopped {
                    resources,
                    runtime: Some(runtime),
                    reason,
                };
                Err(AxVmError::invalid_transition(
                    VmStatus::Stopped,
                    VmStatus::Ready,
                    "reset",
                ))
            }
            other => {
                let from = other.status();
                *self = other;
                Err(AxVmError::invalid_transition(
                    from,
                    VmStatus::Ready,
                    "reset",
                ))
            }
        }
    }

    pub fn destroy_with<F>(&mut self, f: F) -> AxVmResult
    where
        F: FnOnce(Option<R>) -> AxVmResult,
    {
        let old = core::mem::replace(self, Machine::Destroying);
        match old {
            Machine::Destroyed => {
                *self = Machine::Destroyed;
                Ok(())
            }
            Machine::Ready(resources) => {
                f(Some(resources))?;
                *self = Machine::Destroyed;
                Ok(())
            }
            Machine::Running { resources, runtime } => {
                *self = Machine::Running { resources, runtime };
                Err(AxVmError::invalid_transition(
                    VmStatus::Running,
                    VmStatus::Destroyed,
                    "destroy",
                ))
            }
            Machine::Pausing { resources, runtime } => {
                *self = Machine::Pausing { resources, runtime };
                Err(AxVmError::invalid_transition(
                    VmStatus::Pausing,
                    VmStatus::Destroyed,
                    "destroy",
                ))
            }
            Machine::Paused { resources, runtime } => {
                *self = Machine::Paused { resources, runtime };
                Err(AxVmError::invalid_transition(
                    VmStatus::Paused,
                    VmStatus::Destroyed,
                    "destroy",
                ))
            }
            Machine::Stopping {
                resources,
                runtime,
                reason,
            } => {
                *self = Machine::Stopping {
                    resources,
                    runtime,
                    reason,
                };
                Err(AxVmError::invalid_transition(
                    VmStatus::Stopping,
                    VmStatus::Destroyed,
                    "destroy",
                ))
            }
            Machine::Stopped {
                resources,
                runtime: Some(runtime),
                reason,
            } => {
                *self = Machine::Stopped {
                    resources,
                    runtime: Some(runtime),
                    reason,
                };
                Err(AxVmError::invalid_transition(
                    VmStatus::Stopped,
                    VmStatus::Destroyed,
                    "destroy",
                ))
            }
            Machine::Stopped {
                resources,
                runtime: None,
                ..
            } => {
                f(resources)?;
                *self = Machine::Destroyed;
                Ok(())
            }
            Machine::Failed(_) | Machine::Switching | Machine::Destroying => {
                f(None)?;
                *self = Machine::Destroyed;
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_allows_start_pause_resume_stop_destroy_from_ready() {
        let mut machine = Machine::Ready(7usize);
        assert_eq!(machine.status(), VmStatus::Ready);

        machine
            .start_with(|resources| {
                *resources += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(machine.status(), VmStatus::Running);

        machine.pause().unwrap();
        assert_eq!(machine.status(), VmStatus::Paused);

        machine.resume().unwrap();
        assert_eq!(machine.status(), VmStatus::Running);

        machine
            .request_stop_with(StopReason::Clean, |resources, _| {
                *resources.unwrap() += 1;
                Ok(())
            })
            .unwrap();
        machine.finish_stop().unwrap();
        assert_eq!(machine.take_stopped_runtime(), Some(()));
        assert_eq!(machine.status(), VmStatus::Stopped);

        machine
            .destroy_with(|resources| {
                assert_eq!(resources, Some(9));
                Ok(())
            })
            .unwrap();
        assert_eq!(machine.status(), VmStatus::Destroyed);
    }

    #[test]
    fn lifecycle_rejects_invalid_transitions_without_changing_state() {
        let mut machine = Machine::<usize>::Ready(1);
        let err = machine.resume().unwrap_err();
        assert!(matches!(
            err,
            AxVmError::InvalidTransition {
                from: VmStatus::Ready,
                to: VmStatus::Running,
                operation: "resume"
            }
        ));
        assert_eq!(machine.status(), VmStatus::Ready);
    }

    #[test]
    fn lifecycle_reset_drops_runtime_and_returns_to_ready() {
        let mut machine = Machine::Ready(7usize);
        machine.start_with(|resources| Ok(*resources + 1)).unwrap();
        assert_eq!(machine.status(), VmStatus::Running);
        machine
            .request_stop_with(StopReason::Forced, |_, _| Ok(()))
            .unwrap();
        machine.finish_stop().unwrap();
        assert_eq!(machine.take_stopped_runtime(), Some(8));

        machine
            .reset_with(|resources| {
                *resources += 10;
                Ok(())
            })
            .unwrap();

        assert_eq!(machine.status(), VmStatus::Ready);
        assert_eq!(machine.resources(), Some(&17));
        assert!(machine.runtime().is_none());
    }

    #[test]
    fn lifecycle_rejects_reset_while_runtime_is_live() {
        let mut machine = Machine::Ready(7usize);
        machine.start_with(|resources| Ok(*resources + 1)).unwrap();

        let err = machine.reset_with(|_| Ok(())).unwrap_err();

        assert!(matches!(
            err,
            AxVmError::InvalidTransition {
                from: VmStatus::Running,
                to: VmStatus::Ready,
                operation: "reset"
            }
        ));
        assert_eq!(machine.status(), VmStatus::Running);
        assert_eq!(machine.resources(), Some(&7));
        assert_eq!(machine.runtime(), Some(&8));
    }

    #[test]
    fn lifecycle_rejects_destroy_while_runtime_is_live() {
        let mut machine = Machine::Ready(7usize);
        machine.start_with(|resources| Ok(*resources + 1)).unwrap();

        let err = machine.destroy_with(|_| Ok(())).unwrap_err();

        assert!(matches!(
            err,
            AxVmError::InvalidTransition {
                from: VmStatus::Running,
                to: VmStatus::Destroyed,
                operation: "destroy"
            }
        ));
        assert_eq!(machine.status(), VmStatus::Running);
        assert_eq!(machine.resources(), Some(&7));
        assert_eq!(machine.runtime(), Some(&8));
    }

    #[test]
    fn lifecycle_requires_runtime_cleanup_before_restarting_stopped_vm() {
        let mut machine = Machine::Ready(7usize);
        machine.start_with(|resources| Ok(*resources + 1)).unwrap();
        machine
            .request_stop_with(StopReason::Forced, |_, _| Ok(()))
            .unwrap();
        machine.finish_stop().unwrap();

        let err = machine.start_with(|_| Ok(9usize)).unwrap_err();

        assert!(matches!(
            err,
            AxVmError::InvalidTransition {
                from: VmStatus::Stopped,
                to: VmStatus::Running,
                operation: "start"
            }
        ));
        assert_eq!(machine.status(), VmStatus::Stopped);
        assert_eq!(machine.resources(), Some(&7));
        assert!(machine.runtime().is_none());
        assert_eq!(machine.take_stopped_runtime(), Some(8));
    }
}
