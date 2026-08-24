//! Safe control-loop boundary for the frozen ONNX Runtime CPU backend.

use std::{
    ffi::{CString, c_char, c_int, c_void},
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    ptr::NonNull,
};

use thiserror::Error;

use crate::{
    control::ControlCommand,
    neural::{NeuralController, NeuralError, ThermalObservation},
};

const IDENTITY_CAPACITY: usize = 64;

#[repr(C)]
struct BridgeStatus {
    stage: c_int,
    runtime_status: c_int,
}

impl BridgeStatus {
    const fn new() -> Self {
        Self {
            stage: 0,
            runtime_status: 0,
        }
    }
}

#[repr(C)]
struct BridgeInfo {
    runtime_version: [c_char; IDENTITY_CAPACITY],
    provider: [c_char; IDENTITY_CAPACITY],
    init_us: u64,
}

impl BridgeInfo {
    const fn new() -> Self {
        Self {
            runtime_version: [0; IDENTITY_CAPACITY],
            provider: [0; IDENTITY_CAPACITY],
            init_us: 0,
        }
    }
}

#[repr(C)]
struct BridgeInference {
    output: f32,
    wall_ns: u64,
}

impl BridgeInference {
    const fn new() -> Self {
        Self {
            output: 0.0,
            wall_ns: 0,
        }
    }
}

unsafe extern "C" {
    fn ivc_ort_create(
        model_path: *const c_char,
        context: *mut *mut c_void,
        info: *mut BridgeInfo,
        status: *mut BridgeStatus,
    ) -> c_int;
    fn ivc_ort_infer(
        context: *mut c_void,
        inputs: *const f32,
        inference: *mut BridgeInference,
        status: *mut BridgeStatus,
    ) -> c_int;
    fn ivc_ort_destroy(context: *mut c_void, status: *mut BridgeStatus) -> c_int;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrtRuntimeInfo {
    pub runtime_version: String,
    pub provider: String,
    pub initialization_us: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrtSummary {
    pub runtime: OrtRuntimeInfo,
    pub samples: usize,
    pub wall_p99_ns: u64,
}

pub struct OrtController {
    context: Option<NonNull<c_void>>,
    evidence_path: PathBuf,
    evidence: BufWriter<File>,
    runtime: OrtRuntimeInfo,
    wall_times_ns: Vec<u64>,
}

impl OrtController {
    pub fn new(model_path: &Path, evidence_path: &Path) -> Result<Self, OrtError> {
        let encoded_model_path = CString::new(model_path.as_os_str().as_encoded_bytes())
            .map_err(|_| OrtError::ModelPathContainsNul(model_path.to_path_buf()))?;
        let evidence_file = File::create(evidence_path).map_err(|source| OrtError::EvidenceIo {
            path: evidence_path.to_path_buf(),
            source,
        })?;
        let mut evidence = BufWriter::new(evidence_file);
        writeln!(
            evidence,
            "sequence,input0_bits,input1_bits,input2_bits,input3_bits,output_bits,\
             actuator_permille,wall_ns"
        )
        .map_err(|source| OrtError::EvidenceIo {
            path: evidence_path.to_path_buf(),
            source,
        })?;

        let mut raw_context = std::ptr::null_mut();
        let mut bridge_info = BridgeInfo::new();
        let mut bridge_status = BridgeStatus::new();
        // SAFETY: every output pointer references writable storage for the
        // duration of the call, and the bridge only reads the C model path.
        let result = unsafe {
            ivc_ort_create(
                encoded_model_path.as_ptr(),
                &mut raw_context,
                &mut bridge_info,
                &mut bridge_status,
            )
        };
        check_bridge_result("initialize", result, bridge_status)?;
        let context = NonNull::new(raw_context).ok_or(OrtError::NullContext)?;
        if bridge_info.init_us == 0 {
            let mut destroy_status = BridgeStatus::new();
            // SAFETY: `context` was returned by a successful bridge create.
            unsafe {
                ivc_ort_destroy(context.as_ptr(), &mut destroy_status);
            }
            return Err(OrtError::NonPositiveInitializationTime);
        }

        Ok(Self {
            context: Some(context),
            evidence_path: evidence_path.to_path_buf(),
            evidence,
            runtime: OrtRuntimeInfo {
                runtime_version: parse_identity(&bridge_info.runtime_version, "runtime version")?,
                provider: parse_identity(&bridge_info.provider, "execution provider")?,
                initialization_us: bridge_info.init_us,
            },
            wall_times_ns: Vec::new(),
        })
    }

    pub fn runtime_info(&self) -> &OrtRuntimeInfo {
        &self.runtime
    }

    pub fn command(
        &mut self,
        observation: ThermalObservation,
        sample_id: u32,
    ) -> Result<ControlCommand, OrtError> {
        let inputs = observation.normalized()?;
        let context = self.context.ok_or(OrtError::AlreadyFinished)?;
        let mut inference = BridgeInference::new();
        let mut bridge_status = BridgeStatus::new();
        // SAFETY: `self` exclusively owns the live context; `inputs` contains
        // the four floats required by the model, and both outputs are writable.
        let result = unsafe {
            ivc_ort_infer(
                context.as_ptr(),
                inputs.as_ptr(),
                &mut inference,
                &mut bridge_status,
            )
        };
        check_bridge_result("infer", result, bridge_status)?;
        if !inference.output.is_finite() {
            return Err(OrtError::NonFiniteOutput);
        }
        if inference.wall_ns == 0 {
            return Err(OrtError::NonPositiveWallTime);
        }
        let command =
            NeuralController.command_from_output(observation, sample_id, inference.output)?;
        writeln!(
            self.evidence,
            "{sample_id},{:08x},{:08x},{:08x},{:08x},{:08x},{},{}",
            inputs[0].to_bits(),
            inputs[1].to_bits(),
            inputs[2].to_bits(),
            inputs[3].to_bits(),
            inference.output.to_bits(),
            command.actuator_permille,
            inference.wall_ns,
        )
        .map_err(|source| OrtError::EvidenceIo {
            path: self.evidence_path.clone(),
            source,
        })?;
        self.wall_times_ns.push(inference.wall_ns);
        Ok(command)
    }

    pub fn finish(&mut self) -> Result<OrtSummary, OrtError> {
        self.evidence
            .flush()
            .map_err(|source| OrtError::EvidenceIo {
                path: self.evidence_path.clone(),
                source,
            })?;
        let context = self.context.take().ok_or(OrtError::AlreadyFinished)?;
        let mut bridge_status = BridgeStatus::new();
        // SAFETY: `context` is still exclusively owned and is consumed here.
        let result = unsafe { ivc_ort_destroy(context.as_ptr(), &mut bridge_status) };
        check_bridge_result("destroy", result, bridge_status)?;

        self.wall_times_ns.sort_unstable();
        Ok(OrtSummary {
            runtime: self.runtime.clone(),
            samples: self.wall_times_ns.len(),
            wall_p99_ns: percentile(&self.wall_times_ns, 99),
        })
    }
}

impl Drop for OrtController {
    fn drop(&mut self) {
        let Some(context) = self.context.take() else {
            return;
        };
        let mut bridge_status = BridgeStatus::new();
        // SAFETY: the remaining context is exclusively owned by `self` and is
        // destroyed exactly once during this fallback cleanup path.
        unsafe {
            ivc_ort_destroy(context.as_ptr(), &mut bridge_status);
        }
    }
}

fn check_bridge_result(
    operation: &'static str,
    result: c_int,
    status: BridgeStatus,
) -> Result<(), OrtError> {
    if result == 0 {
        return Ok(());
    }
    Err(OrtError::Bridge {
        operation,
        stage: bridge_stage_name(status.stage),
        runtime_status: status.runtime_status,
    })
}

fn bridge_stage_name(stage: c_int) -> &'static str {
    match stage {
        1 => "validate-arguments",
        2 => "validate-runtime-version",
        3 => "allocate-context",
        4 => "create-environment",
        5 => "configure-session",
        6 => "create-session",
        7 => "validate-tensor-contract",
        8 => "create-memory-info",
        9 => "create-input-tensor",
        10 => "run",
        11 => "validate-output",
        12 => "monotonic-clock",
        13 => "destroy-context",
        _ => "unknown",
    }
}

fn parse_identity(
    value: &[c_char; IDENTITY_CAPACITY],
    label: &'static str,
) -> Result<String, OrtError> {
    let bytes: Vec<u8> = value
        .iter()
        .map(|character| character.to_ne_bytes()[0])
        .take_while(|character| *character != 0)
        .collect();
    if bytes.is_empty() {
        return Err(OrtError::EmptyIdentity(label));
    }
    String::from_utf8(bytes).map_err(|_| OrtError::InvalidIdentityEncoding(label))
}

fn percentile(sorted: &[u64], percentage: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() - 1) * percentage) / 100]
}

#[derive(Debug, Error)]
pub enum OrtError {
    #[error("ORT model path contains an embedded NUL: {0:?}")]
    ModelPathContainsNul(PathBuf),
    #[error("ORT evidence I/O failed for {path:?}: {source}")]
    EvidenceIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("ORT {operation} failed at {stage} (runtime status {runtime_status})")]
    Bridge {
        operation: &'static str,
        stage: &'static str,
        runtime_status: c_int,
    },
    #[error("ORT bridge returned a null context after successful initialization")]
    NullContext,
    #[error("ORT bridge returned a non-positive initialization duration")]
    NonPositiveInitializationTime,
    #[error("ORT bridge returned a non-finite output")]
    NonFiniteOutput,
    #[error("ORT bridge returned a non-positive wall duration")]
    NonPositiveWallTime,
    #[error("ORT {0} is empty")]
    EmptyIdentity(&'static str),
    #[error("ORT {0} is not UTF-8")]
    InvalidIdentityEncoding(&'static str),
    #[error("ORT controller was already finalized")]
    AlreadyFinished,
    #[error(transparent)]
    Neural(#[from] NeuralError),
}
