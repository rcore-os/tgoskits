//! Safe control-loop boundary for the frozen RKNN userspace runtime.

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

const VERSION_CAPACITY: usize = 256;

#[repr(C)]
struct BridgeStatus {
    stage: c_int,
    vendor_status: c_int,
}

impl BridgeStatus {
    const fn new() -> Self {
        Self {
            stage: 0,
            vendor_status: 0,
        }
    }
}

#[repr(C)]
struct BridgeInfo {
    api_version: [c_char; VERSION_CAPACITY],
    driver_version: [c_char; VERSION_CAPACITY],
    init_us: u64,
}

impl BridgeInfo {
    const fn new() -> Self {
        Self {
            api_version: [0; VERSION_CAPACITY],
            driver_version: [0; VERSION_CAPACITY],
            init_us: 0,
        }
    }
}

#[repr(C)]
struct BridgeInference {
    output: f32,
    wall_ns: u64,
    device_us: i64,
}

impl BridgeInference {
    const fn new() -> Self {
        Self {
            output: 0.0,
            wall_ns: 0,
            device_us: 0,
        }
    }
}

unsafe extern "C" {
    fn ivc_rknn_create(
        model_path: *const c_char,
        core_mask: u32,
        context: *mut *mut c_void,
        info: *mut BridgeInfo,
        status: *mut BridgeStatus,
    ) -> c_int;
    fn ivc_rknn_infer(
        context: *mut c_void,
        inputs: *const f32,
        inference: *mut BridgeInference,
        status: *mut BridgeStatus,
    ) -> c_int;
    fn ivc_rknn_destroy(context: *mut c_void, status: *mut BridgeStatus) -> c_int;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RknnRuntimeInfo {
    pub api_version: String,
    pub driver_version: String,
    pub core_mask: u32,
    pub initialization_us: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RknnSummary {
    pub runtime: RknnRuntimeInfo,
    pub samples: usize,
    pub positive_device_times: usize,
    pub device_p99_us: u64,
    pub wall_p99_ns: u64,
}

pub struct RknnController {
    context: Option<NonNull<c_void>>,
    evidence_path: PathBuf,
    evidence: BufWriter<File>,
    runtime: RknnRuntimeInfo,
    device_times_us: Vec<u64>,
    wall_times_ns: Vec<u64>,
}

impl RknnController {
    pub fn new(model_path: &Path, evidence_path: &Path, core_mask: u32) -> Result<Self, RknnError> {
        let encoded_model_path = CString::new(model_path.as_os_str().as_encoded_bytes())
            .map_err(|_| RknnError::ModelPathContainsNul(model_path.to_path_buf()))?;
        let evidence_file =
            File::create(evidence_path).map_err(|source| RknnError::EvidenceIo {
                path: evidence_path.to_path_buf(),
                source,
            })?;
        let mut evidence = BufWriter::new(evidence_file);
        writeln!(
            evidence,
            "sequence,input0_bits,input1_bits,input2_bits,input3_bits,output_bits,\
             actuator_permille,wall_ns,device_us"
        )
        .map_err(|source| RknnError::EvidenceIo {
            path: evidence_path.to_path_buf(),
            source,
        })?;

        let mut raw_context = std::ptr::null_mut();
        let mut bridge_info = BridgeInfo::new();
        let mut bridge_status = BridgeStatus::new();
        // SAFETY: every pointer references writable storage for the duration of
        // the call, and the C bridge copies the NUL-terminated model path.
        let result = unsafe {
            ivc_rknn_create(
                encoded_model_path.as_ptr(),
                core_mask,
                &mut raw_context,
                &mut bridge_info,
                &mut bridge_status,
            )
        };
        check_bridge_result("initialize", result, bridge_status)?;
        let context = NonNull::new(raw_context).ok_or(RknnError::NullContext)?;
        if bridge_info.init_us == 0 {
            let mut destroy_status = BridgeStatus::new();
            // SAFETY: `context` was returned by a successful bridge create.
            unsafe {
                ivc_rknn_destroy(context.as_ptr(), &mut destroy_status);
            }
            return Err(RknnError::NonPositiveInitializationTime);
        }

        Ok(Self {
            context: Some(context),
            evidence_path: evidence_path.to_path_buf(),
            evidence,
            runtime: RknnRuntimeInfo {
                api_version: parse_version(&bridge_info.api_version, "API")?,
                driver_version: parse_version(&bridge_info.driver_version, "driver")?,
                core_mask,
                initialization_us: bridge_info.init_us,
            },
            device_times_us: Vec::new(),
            wall_times_ns: Vec::new(),
        })
    }

    pub fn runtime_info(&self) -> &RknnRuntimeInfo {
        &self.runtime
    }

    pub fn command(
        &mut self,
        observation: ThermalObservation,
        sample_id: u32,
    ) -> Result<ControlCommand, RknnError> {
        let inputs = observation.normalized()?;
        let context = self.context.ok_or(RknnError::AlreadyFinished)?;
        let mut inference = BridgeInference::new();
        let mut bridge_status = BridgeStatus::new();
        // SAFETY: the context is exclusively owned by `self`; `inputs` has the
        // four floats required by the bridge, and both output pointers are valid.
        let result = unsafe {
            ivc_rknn_infer(
                context.as_ptr(),
                inputs.as_ptr(),
                &mut inference,
                &mut bridge_status,
            )
        };
        check_bridge_result("infer", result, bridge_status)?;
        if !inference.output.is_finite() {
            return Err(RknnError::NonFiniteOutput);
        }
        if inference.wall_ns == 0 {
            return Err(RknnError::NonPositiveWallTime);
        }
        let device_us = u64::try_from(inference.device_us)
            .map_err(|_| RknnError::NonPositiveDeviceTime(inference.device_us))?;
        if device_us == 0 {
            return Err(RknnError::NonPositiveDeviceTime(inference.device_us));
        }
        let command =
            NeuralController.command_from_output(observation, sample_id, inference.output)?;
        writeln!(
            self.evidence,
            "{sample_id},{:08x},{:08x},{:08x},{:08x},{:08x},{},{},{}",
            inputs[0].to_bits(),
            inputs[1].to_bits(),
            inputs[2].to_bits(),
            inputs[3].to_bits(),
            inference.output.to_bits(),
            command.actuator_permille,
            inference.wall_ns,
            device_us,
        )
        .map_err(|source| RknnError::EvidenceIo {
            path: self.evidence_path.clone(),
            source,
        })?;
        self.device_times_us.push(device_us);
        self.wall_times_ns.push(inference.wall_ns);
        Ok(command)
    }

    pub fn finish(&mut self) -> Result<RknnSummary, RknnError> {
        self.evidence
            .flush()
            .map_err(|source| RknnError::EvidenceIo {
                path: self.evidence_path.clone(),
                source,
            })?;
        let context = self.context.take().ok_or(RknnError::AlreadyFinished)?;
        let mut bridge_status = BridgeStatus::new();
        // SAFETY: `context` is still exclusively owned and is consumed here.
        let result = unsafe { ivc_rknn_destroy(context.as_ptr(), &mut bridge_status) };
        check_bridge_result("destroy", result, bridge_status)?;

        self.device_times_us.sort_unstable();
        self.wall_times_ns.sort_unstable();
        Ok(RknnSummary {
            runtime: self.runtime.clone(),
            samples: self.device_times_us.len(),
            positive_device_times: self.device_times_us.len(),
            device_p99_us: percentile(&self.device_times_us, 99),
            wall_p99_ns: percentile(&self.wall_times_ns, 99),
        })
    }
}

impl Drop for RknnController {
    fn drop(&mut self) {
        let Some(context) = self.context.take() else {
            return;
        };
        let mut bridge_status = BridgeStatus::new();
        // SAFETY: the remaining context is exclusively owned by `self` and is
        // destroyed exactly once during this fallback cleanup path.
        unsafe {
            ivc_rknn_destroy(context.as_ptr(), &mut bridge_status);
        }
    }
}

fn check_bridge_result(
    operation: &'static str,
    result: c_int,
    status: BridgeStatus,
) -> Result<(), RknnError> {
    if result == 0 {
        return Ok(());
    }
    Err(RknnError::Bridge {
        operation,
        stage: bridge_stage_name(status.stage),
        vendor_status: status.vendor_status,
    })
}

fn bridge_stage_name(stage: c_int) -> &'static str {
    match stage {
        1 => "open-model",
        2 => "read-model",
        3 => "allocate-context",
        4 => "initialize-context",
        5 => "set-core-mask",
        6 => "query-io-counts",
        7 => "query-input-attribute",
        8 => "query-output-attribute",
        9 => "validate-tensor-contract",
        10 => "set-input",
        11 => "run",
        12 => "get-output",
        13 => "validate-output",
        14 => "query-performance",
        15 => "release-output",
        16 => "destroy-context",
        17 => "monotonic-clock",
        18 => "query-version",
        _ => "unknown",
    }
}

fn parse_version(
    value: &[c_char; VERSION_CAPACITY],
    label: &'static str,
) -> Result<String, RknnError> {
    let bytes: Vec<u8> = value
        .iter()
        .map(|character| *character as u8)
        .take_while(|character| *character != 0)
        .collect();
    if bytes.is_empty() {
        return Err(RknnError::EmptyVersion(label));
    }
    String::from_utf8(bytes).map_err(|_| RknnError::InvalidVersionEncoding(label))
}

fn percentile(sorted: &[u64], percentage: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() - 1) * percentage) / 100]
}

#[derive(Debug, Error)]
pub enum RknnError {
    #[error("RKNN model path contains an embedded NUL: {0:?}")]
    ModelPathContainsNul(PathBuf),
    #[error("RKNN evidence I/O failed for {path:?}: {source}")]
    EvidenceIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("RKNN {operation} failed at {stage} (vendor status {vendor_status})")]
    Bridge {
        operation: &'static str,
        stage: &'static str,
        vendor_status: c_int,
    },
    #[error("RKNN bridge returned a null context after successful initialization")]
    NullContext,
    #[error("RKNN bridge returned a non-positive initialization duration")]
    NonPositiveInitializationTime,
    #[error("RKNN bridge returned a non-finite output")]
    NonFiniteOutput,
    #[error("RKNN bridge returned a non-positive wall duration")]
    NonPositiveWallTime,
    #[error("RKNN bridge returned a non-positive device duration: {0}")]
    NonPositiveDeviceTime(i64),
    #[error("RKNN {0} version is empty")]
    EmptyVersion(&'static str),
    #[error("RKNN {0} version is not UTF-8")]
    InvalidVersionEncoding(&'static str),
    #[error("RKNN controller was already finalized")]
    AlreadyFinished,
    #[error(transparent)]
    Neural(#[from] NeuralError),
}
