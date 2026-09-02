//! Raw per-cycle evidence for the physical closed-loop controller.

use std::{
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

/// One acknowledged controller cycle retained after the timed control loop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControllerSample {
    /// Stop-and-wait protocol sequence number.
    pub sequence: u32,
    /// Monotonic timestamp immediately before inference starts.
    pub cycle_started_us: u64,
    /// Monotonic timestamp immediately before the command is sent.
    pub command_sent_us: u64,
    /// Monotonic timestamp after both ACK and status have arrived.
    pub response_completed_us: u64,
    /// Inference plus transport latency.
    pub full_loop_us: u64,
    /// Controller computation latency before the first send.
    pub pre_send_us: u64,
    /// Network and RTOS response latency after the first send.
    pub transport_us: u64,
    /// Target temperature for this cycle.
    pub setpoint_milli_c: i32,
    /// Temperature used as the controller observation.
    pub observed_milli_c: i32,
    /// Temperature returned by the RTOS after applying the command.
    pub measured_milli_c: i32,
    /// Actuator value requested by the controller.
    pub command_actuator_permille: u16,
    /// Actuator value reported by the RTOS.
    pub status_actuator_permille: u16,
    /// Setpoint minus the returned temperature.
    pub error_milli_c: i32,
}

/// Writes acknowledged controller samples as a stable CSV artifact.
///
/// The caller invokes this after the timed control loop so filesystem latency
/// cannot affect the measured controller or transport latency.
///
/// # Errors
///
/// Returns [`ControllerCsvError`] if the output cannot be created, written, or
/// flushed.
pub fn write_controller_samples(
    path: &Path,
    samples: &[ControllerSample],
) -> Result<(), ControllerCsvError> {
    let file = File::create(path).map_err(|source| ControllerCsvError::Create {
        path: path.to_path_buf(),
        source,
    })?;
    let mut output = BufWriter::new(file);
    writeln!(
        output,
        "sequence,cycle_started_us,command_sent_us,response_completed_us,full_loop_us,pre_send_us,\
         transport_us,setpoint_milli_c,observed_milli_c,measured_milli_c,\
         command_actuator_permille,status_actuator_permille,error_milli_c"
    )
    .map_err(|source| ControllerCsvError::WriteHeader {
        path: path.to_path_buf(),
        source,
    })?;
    for sample in samples {
        writeln!(
            output,
            "{},{},{},{},{},{},{},{},{},{},{},{},{}",
            sample.sequence,
            sample.cycle_started_us,
            sample.command_sent_us,
            sample.response_completed_us,
            sample.full_loop_us,
            sample.pre_send_us,
            sample.transport_us,
            sample.setpoint_milli_c,
            sample.observed_milli_c,
            sample.measured_milli_c,
            sample.command_actuator_permille,
            sample.status_actuator_permille,
            sample.error_milli_c,
        )
        .map_err(|source| ControllerCsvError::WriteSample {
            path: path.to_path_buf(),
            sequence: sample.sequence,
            source,
        })?;
    }
    output.flush().map_err(|source| ControllerCsvError::Flush {
        path: path.to_path_buf(),
        source,
    })
}

/// Failure while persisting physical controller samples.
#[derive(Debug, thiserror::Error)]
pub enum ControllerCsvError {
    /// The destination file could not be created.
    #[error("cannot create controller CSV {path:?}: {source}")]
    Create {
        /// Requested destination path.
        path: PathBuf,
        /// Filesystem error.
        source: std::io::Error,
    },
    /// The stable CSV header could not be written.
    #[error("cannot write controller CSV header to {path:?}: {source}")]
    WriteHeader {
        /// Requested destination path.
        path: PathBuf,
        /// Filesystem error.
        source: std::io::Error,
    },
    /// One sample row could not be written.
    #[error("cannot write controller CSV sample {sequence} to {path:?}: {source}")]
    WriteSample {
        /// Requested destination path.
        path: PathBuf,
        /// Protocol sequence being written.
        sequence: u32,
        /// Filesystem error.
        source: std::io::Error,
    },
    /// Buffered CSV data could not be flushed.
    #[error("cannot flush controller CSV {path:?}: {source}")]
    Flush {
        /// Requested destination path.
        path: PathBuf,
        /// Filesystem error.
        source: std::io::Error,
    },
}
