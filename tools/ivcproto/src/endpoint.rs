//! RTOS-side command validation and fail-safe state transition logic.

use thiserror::Error;

use crate::{
    control::{
        ControlCommand, ControlMode, ControlOperation, MAX_ACTUATOR_PERMILLE, StatusReport,
        StatusState,
    },
    wire::ErrorCode,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EndpointConfig {
    pub safe_actuator_permille: u16,
    pub command_timeout_us: u64,
    pub maximum_command_age_us: u64,
}

impl EndpointConfig {
    pub fn validate(self) -> Result<Self, EndpointError> {
        if self.safe_actuator_permille > MAX_ACTUATOR_PERMILLE {
            return Err(EndpointError::SafeActuatorOutOfRange(
                self.safe_actuator_permille,
            ));
        }
        if self.command_timeout_us == 0 || self.maximum_command_age_us == 0 {
            return Err(EndpointError::ZeroTimeout);
        }
        Ok(self)
    }
}

impl Default for EndpointConfig {
    fn default() -> Self {
        Self {
            safe_actuator_permille: 0,
            command_timeout_us: 500_000,
            maximum_command_age_us: 250_000,
        }
    }
}

/// State owned by the RTOS actuator task after network deduplication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlEndpoint {
    config: EndpointConfig,
    active_mode: ControlMode,
    actuator_permille: u16,
    setpoint_milli_c: i32,
    last_sequence: u32,
    last_sample_id: Option<u32>,
    last_valid_command_us: Option<u64>,
    fault: ErrorCode,
}

impl ControlEndpoint {
    pub fn new(
        config: EndpointConfig,
        initial_setpoint_milli_c: i32,
    ) -> Result<Self, EndpointError> {
        let config = config.validate()?;
        Ok(Self {
            config,
            active_mode: ControlMode::Safe,
            actuator_permille: config.safe_actuator_permille,
            setpoint_milli_c: initial_setpoint_milli_c,
            last_sequence: 0,
            last_sample_id: None,
            last_valid_command_us: None,
            fault: ErrorCode::None,
        })
    }

    /// Starts replay tracking for an accepted transport session.
    ///
    /// The receive window must authorize the transition before this is called.
    /// Actuator and safety-timer state remain intact until the first command of
    /// the new session has been validated and applied.
    pub fn begin_session(&mut self) {
        self.last_sequence = 0;
        self.last_sample_id = None;
    }

    /// Applies a fresh, already de-duplicated command exactly once.
    pub fn apply(
        &mut self,
        sequence: u32,
        command: ControlCommand,
        sent_at_us: u64,
        now_us: u64,
    ) -> Result<ApplyOutcome, EndpointError> {
        command.validate().map_err(EndpointError::InvalidPayload)?;
        let age_us = now_us
            .checked_sub(sent_at_us)
            .ok_or(EndpointError::FutureTimestamp)?;
        if age_us > self.config.maximum_command_age_us {
            return Err(EndpointError::StaleTimestamp { age_us });
        }
        if sequence <= self.last_sequence {
            return Err(EndpointError::StaleSequence(sequence));
        }
        if self
            .last_sample_id
            .is_some_and(|sample_id| command.sample_id <= sample_id)
        {
            return Err(EndpointError::StaleSample(command.sample_id));
        }

        self.last_sequence = sequence;
        self.last_sample_id = Some(command.sample_id);
        self.last_valid_command_us = Some(now_us);
        self.setpoint_milli_c = command.setpoint_milli_c;
        self.fault = ErrorCode::None;
        match command.operation {
            ControlOperation::SetActuator | ControlOperation::Heartbeat => {
                self.active_mode = command.mode;
                self.actuator_permille = command.actuator_permille;
                Ok(ApplyOutcome::Applied)
            }
            ControlOperation::EnterSafeState => {
                self.enter_safe_state(ErrorCode::None);
                Ok(ApplyOutcome::EnteredSafeState)
            }
        }
    }

    /// Enters the configured safe state when the controller stops responding.
    pub fn check_timeout(&mut self, now_us: u64) -> Result<ApplyOutcome, EndpointError> {
        let Some(last_command_us) = self.last_valid_command_us else {
            return Ok(ApplyOutcome::NoChange);
        };
        let silence_us = now_us
            .checked_sub(last_command_us)
            .ok_or(EndpointError::ClockMovedBackward)?;
        if silence_us <= self.config.command_timeout_us {
            return Ok(ApplyOutcome::NoChange);
        }
        if self.active_mode == ControlMode::Safe
            && self.actuator_permille == self.config.safe_actuator_permille
            && self.fault == ErrorCode::ControllerTimeout
        {
            return Ok(ApplyOutcome::NoChange);
        }
        self.enter_safe_state(ErrorCode::ControllerTimeout);
        Ok(ApplyOutcome::TimedOutToSafeState)
    }

    pub fn status(&self, measured_milli_c: i32) -> StatusReport {
        let state = if self.fault == ErrorCode::ControllerTimeout {
            StatusState::SafeFallback
        } else if self.last_sequence == 0 {
            StatusState::Ready
        } else {
            StatusState::Applied
        };
        StatusReport {
            state,
            active_mode: self.active_mode,
            actuator_permille: self.actuator_permille,
            measured_milli_c,
            setpoint_milli_c: self.setpoint_milli_c,
            applied_sequence: self.last_sequence,
            fault: self.fault,
        }
    }

    pub const fn actuator_permille(&self) -> u16 {
        self.actuator_permille
    }

    fn enter_safe_state(&mut self, fault: ErrorCode) {
        self.active_mode = ControlMode::Safe;
        self.actuator_permille = self.config.safe_actuator_permille;
        self.fault = fault;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplyOutcome {
    Applied,
    EnteredSafeState,
    TimedOutToSafeState,
    NoChange,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum EndpointError {
    #[error("safe actuator {0} is outside 0..=1000 permille")]
    SafeActuatorOutOfRange(u16),
    #[error("command timeout and maximum command age must be nonzero")]
    ZeroTimeout,
    #[error("invalid command payload: {0}")]
    InvalidPayload(crate::control::PayloadError),
    #[error("command timestamp is in the future")]
    FutureTimestamp,
    #[error("command is stale by {age_us} microseconds")]
    StaleTimestamp { age_us: u64 },
    #[error("command sequence {0} was already applied")]
    StaleSequence(u32),
    #[error("sample {0} was already applied")]
    StaleSample(u32),
    #[error("monotonic clock moved backward")]
    ClockMovedBackward,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn neural_command(sample_id: u32, actuator_permille: u16) -> ControlCommand {
        ControlCommand {
            operation: ControlOperation::SetActuator,
            mode: ControlMode::Neural,
            actuator_permille,
            setpoint_milli_c: 55_000,
            sample_id,
        }
    }

    #[test]
    fn duplicate_side_effect_is_rejected() {
        let mut endpoint = ControlEndpoint::new(EndpointConfig::default(), 55_000).unwrap();
        assert_eq!(
            endpoint.apply(1, neural_command(1, 600), 10, 20),
            Ok(ApplyOutcome::Applied)
        );
        assert_eq!(
            endpoint.apply(1, neural_command(1, 900), 20, 30),
            Err(EndpointError::StaleSequence(1))
        );
        assert_eq!(endpoint.actuator_permille(), 600);
    }

    #[test]
    fn stale_timestamp_never_reaches_actuator() {
        let mut endpoint = ControlEndpoint::new(EndpointConfig::default(), 55_000).unwrap();
        assert_eq!(
            endpoint.apply(1, neural_command(1, 900), 0, 250_001),
            Err(EndpointError::StaleTimestamp { age_us: 250_001 })
        );
        assert_eq!(endpoint.actuator_permille(), 0);
    }

    #[test]
    fn controller_silence_triggers_observable_safe_fallback() {
        let mut endpoint = ControlEndpoint::new(EndpointConfig::default(), 55_000).unwrap();
        endpoint.apply(1, neural_command(1, 600), 10, 20).unwrap();
        assert_eq!(
            endpoint.check_timeout(500_021),
            Ok(ApplyOutcome::TimedOutToSafeState)
        );
        let status = endpoint.status(52_000);
        assert_eq!(status.state, StatusState::SafeFallback);
        assert_eq!(status.active_mode, ControlMode::Safe);
        assert_eq!(status.actuator_permille, 0);
        assert_eq!(status.fault, ErrorCode::ControllerTimeout);
    }

    #[test]
    fn accepted_controller_restart_resets_only_replay_state() {
        let mut endpoint = ControlEndpoint::new(EndpointConfig::default(), 55_000).unwrap();
        endpoint.apply(1, neural_command(1, 600), 10, 20).unwrap();

        endpoint.begin_session();
        assert_eq!(endpoint.actuator_permille(), 600);
        assert_eq!(
            endpoint.apply(1, neural_command(1, 700), 30, 40),
            Ok(ApplyOutcome::Applied)
        );
        assert_eq!(endpoint.actuator_permille(), 700);
    }
}
