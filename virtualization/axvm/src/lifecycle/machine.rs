use alloc::{
    boxed::Box,
    string::{String, ToString},
};

use super::{StopReason, VmLifecycleError, VmLifecycleResult, VmStatus};
use crate::config::AxVMConfig;

pub enum Machine<R, H = ()> {
    Uninit(Box<AxVMConfig>),
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
            Machine::Uninit(_) => VmStatus::Uninit,
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

    pub fn config_mut(&mut self) -> Option<&mut AxVMConfig> {
        match self {
            Machine::Uninit(config) => Some(config.as_mut()),
            _ => None,
        }
    }

    pub fn prepare_with<F>(&mut self, f: F) -> VmLifecycleResult
    where
        F: FnOnce(AxVMConfig) -> VmLifecycleResult<R>,
    {
        let old = core::mem::replace(self, Machine::Switching);
        match old {
            Machine::Uninit(config) => match f(*config) {
                Ok(resources) => {
                    *self = Machine::Ready(resources);
                    Ok(())
                }
                Err(err) => {
                    *self = Machine::Failed(err.to_string());
                    Err(err)
                }
            },
            other => {
                let from = other.status();
                *self = other;
                Err(VmLifecycleError::invalid_transition(
                    from,
                    VmStatus::Ready,
                    "prepare",
                ))
            }
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

    pub fn start_with<F>(&mut self, f: F) -> VmLifecycleResult
    where
        F: FnOnce(&mut R) -> VmLifecycleResult<H>,
    {
        let old = core::mem::replace(self, Machine::Switching);
        match old {
            Machine::Ready(mut resources)
            | Machine::Stopped {
                resources: Some(mut resources),
                ..
            } => match f(&mut resources) {
                Ok(runtime) => {
                    *self = Machine::Running { resources, runtime };
                    Ok(())
                }
                Err(err) => {
                    *self = Machine::Failed(err.to_string());
                    Err(err)
                }
            },
            other => {
                let from = other.status();
                *self = other;
                Err(VmLifecycleError::invalid_transition(
                    from,
                    VmStatus::Running,
                    "start",
                ))
            }
        }
    }

    pub fn pause(&mut self) -> VmLifecycleResult {
        let old = core::mem::replace(self, Machine::Switching);
        match old {
            Machine::Running { resources, runtime } => {
                *self = Machine::Paused { resources, runtime };
                Ok(())
            }
            other => {
                let from = other.status();
                *self = other;
                Err(VmLifecycleError::invalid_transition(
                    from,
                    VmStatus::Paused,
                    "pause",
                ))
            }
        }
    }

    pub fn resume(&mut self) -> VmLifecycleResult {
        let old = core::mem::replace(self, Machine::Switching);
        match old {
            Machine::Paused { resources, runtime } => {
                *self = Machine::Running { resources, runtime };
                Ok(())
            }
            other => {
                let from = other.status();
                *self = other;
                Err(VmLifecycleError::invalid_transition(
                    from,
                    VmStatus::Running,
                    "resume",
                ))
            }
        }
    }

    pub fn stop_with<F>(&mut self, reason: StopReason, f: F) -> VmLifecycleResult
    where
        F: FnOnce(Option<&mut R>, &StopReason) -> VmLifecycleResult,
    {
        let old = core::mem::replace(self, Machine::Switching);
        match old {
            Machine::Ready(resources) => {
                let mut resources = Some(resources);
                if let Err(err) = f(resources.as_mut(), &reason) {
                    *self = Machine::Failed(err.to_string());
                    return Err(err);
                }
                *self = Machine::Stopped { resources, reason };
                Ok(())
            }
            Machine::Running { resources, .. } | Machine::Paused { resources, .. } => {
                let mut resources = Some(resources);
                if let Err(err) = f(resources.as_mut(), &reason) {
                    *self = Machine::Failed(err.to_string());
                    return Err(err);
                }
                *self = Machine::Stopped { resources, reason };
                Ok(())
            }
            Machine::Stopped { resources, reason } => {
                *self = Machine::Stopped { resources, reason };
                Ok(())
            }
            other => {
                let from = other.status();
                *self = other;
                Err(VmLifecycleError::invalid_transition(
                    from,
                    VmStatus::Stopped,
                    "stop",
                ))
            }
        }
    }

    pub fn request_stop_with<F>(&mut self, reason: StopReason, f: F) -> VmLifecycleResult
    where
        F: FnOnce(Option<&mut R>, &StopReason) -> VmLifecycleResult,
    {
        let old = core::mem::replace(self, Machine::Switching);
        match old {
            Machine::Ready(mut resources) => {
                f(Some(&mut resources), &reason)?;
                *self = Machine::Stopped {
                    resources: Some(resources),
                    reason,
                };
                Ok(())
            }
            Machine::Running {
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
            Machine::Stopped { resources, reason } => {
                *self = Machine::Stopped { resources, reason };
                Ok(())
            }
            other => {
                let from = other.status();
                *self = other;
                Err(VmLifecycleError::invalid_transition(
                    from,
                    VmStatus::Stopping,
                    "request_stop",
                ))
            }
        }
    }

    pub fn finish_stop(&mut self) -> VmLifecycleResult {
        let old = core::mem::replace(self, Machine::Switching);
        match old {
            Machine::Stopping {
                resources, reason, ..
            } => {
                *self = Machine::Stopped { resources, reason };
                Ok(())
            }
            Machine::Stopped { resources, reason } => {
                *self = Machine::Stopped { resources, reason };
                Ok(())
            }
            other => {
                let from = other.status();
                *self = other;
                Err(VmLifecycleError::invalid_transition(
                    from,
                    VmStatus::Stopped,
                    "finish_stop",
                ))
            }
        }
    }

    pub fn reset_with<F>(&mut self, f: F) -> VmLifecycleResult
    where
        F: FnOnce(&mut R) -> VmLifecycleResult,
    {
        let old = core::mem::replace(self, Machine::Switching);
        match old {
            Machine::Ready(mut resources)
            | Machine::Running { mut resources, .. }
            | Machine::Paused { mut resources, .. }
            | Machine::Stopped {
                resources: Some(mut resources),
                ..
            } => {
                f(&mut resources)?;
                *self = Machine::Ready(resources);
                Ok(())
            }
            Machine::Stopping {
                resources: Some(mut resources),
                ..
            } => {
                f(&mut resources)?;
                *self = Machine::Ready(resources);
                Ok(())
            }
            other => {
                let from = other.status();
                *self = other;
                Err(VmLifecycleError::invalid_transition(
                    from,
                    VmStatus::Ready,
                    "reset",
                ))
            }
        }
    }

    pub fn destroy_with<F>(&mut self, f: F) -> VmLifecycleResult
    where
        F: FnOnce(Option<R>) -> VmLifecycleResult,
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
            Machine::Running { resources, .. }
            | Machine::Pausing { resources, .. }
            | Machine::Paused { resources, .. } => {
                f(Some(resources))?;
                *self = Machine::Destroyed;
                Ok(())
            }
            Machine::Stopping { resources, .. } | Machine::Stopped { resources, .. } => {
                f(resources)?;
                *self = Machine::Destroyed;
                Ok(())
            }
            Machine::Uninit(_) | Machine::Failed(_) | Machine::Switching | Machine::Destroying => {
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

    fn config() -> AxVMConfig {
        AxVMConfig::default_for_test(1, "lifecycle-test")
    }

    #[test]
    fn lifecycle_allows_prepare_start_pause_resume_stop_destroy() {
        let mut machine = Machine::Uninit(Box::new(config()));
        assert_eq!(machine.status(), VmStatus::Uninit);

        machine.prepare_with(|_| Ok(7usize)).unwrap();
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
            .stop_with(StopReason::Clean, |resources, _| {
                *resources.unwrap() += 1;
                Ok(())
            })
            .unwrap();
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
        let mut machine = Machine::<usize>::Uninit(Box::new(config()));
        let err = machine.start_with(|_| Ok(())).unwrap_err();
        assert!(matches!(
            err,
            VmLifecycleError::InvalidTransition {
                from: VmStatus::Uninit,
                to: VmStatus::Running,
                op: "start"
            }
        ));
        assert_eq!(machine.status(), VmStatus::Uninit);

        machine.prepare_with(|_| Ok(1)).unwrap();
        let err = machine.resume().unwrap_err();
        assert!(matches!(
            err,
            VmLifecycleError::InvalidTransition {
                from: VmStatus::Ready,
                to: VmStatus::Running,
                op: "resume"
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
            .reset_with(|resources| {
                *resources += 10;
                Ok(())
            })
            .unwrap();

        assert_eq!(machine.status(), VmStatus::Ready);
        assert_eq!(machine.resources(), Some(&17));
        assert!(machine.runtime().is_none());
    }
}
