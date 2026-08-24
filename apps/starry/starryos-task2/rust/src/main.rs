use std::{
    env,
    ffi::{CStr, CString},
    net::{SocketAddr, UdpSocket},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use task2_net_protocol::{
    ControlAction, ControlMessage, Endpoint, EndpointState, Frame, MAX_DATAGRAM_LEN, MessageKind,
    PollEvent, ReceiveEvent, RetryPolicy, SequenceNumber, SessionId, StatusMessage,
};
use task3_model::perception::{
    PerceptionDecision, PerceptionRejectReason, YoloDetection, YoloPolicy, yolo_detection_to_target,
};

mod experiment;
mod ncnn;

use experiment::{ImageSample, RunMode, TargetSource, load_task3_ab_manifest};

const SESSION: SessionId = SessionId::new(0x5452_5432);
const POLICY: RetryPolicy = match RetryPolicy::new(500, 5, 200, 5_000) {
    Ok(policy) => policy,
    Err(_) => panic!("invalid T2N1 policy"),
};
const CONTROL_INTERVAL_MS: u64 = 1_000;
const MAX_INFERENCE_US: u64 = 30_000_000;
const CONTROL_TARGET: i32 = 500;
const NCNN_REVISION: &str = "946fe3fb14a8dff8c06df763f67be522167b2f00";
const MODEL_PARAM_SHA256: &str = "d2c0adf8939dc9ce02964ce8ada104447768ffd8e3bffad8fa11e2e61e709c1f";
const MODEL_BIN_SHA256: &str = "0ae562447923999779b12b4f91f96b9ef263add8c9902d10e22e6dd6a2932c12";
const MODEL_INPUT_SHA256: &str = "608c8a61ff0bb43e5a8613f1f6f8aa08af74b084363610ed2b526ad925e4cb6f";
const AB_MANIFEST_SHA256: &str = "3406b58bb1920dba66462b982304294324bc5d4cdb3ed80f14e4c6ba595dd47b";
const MODEL_PARAM_PATH: &CStr = c"/usr/share/task3-yolo/yolo11n.ncnn.param";
const MODEL_BIN_PATH: &CStr = c"/usr/share/task3-yolo/yolo11n.ncnn.bin";
const MODEL_INPUT_PATH: &CStr = c"/usr/share/task3-yolo/input.ppm";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InferenceRejection {
    DeadlineExceeded {
        infer_us: u64,
    },
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    NoDetection {
        infer_us: u64,
    },
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    RuntimeError {
        code: i32,
        infer_us: u64,
    },
    #[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
    UnsupportedPlatform,
    Perception {
        reason: PerceptionRejectReason,
        infer_us: u64,
    },
    InjectedInvalidOutput,
    WorkerDisconnected,
}

impl InferenceRejection {
    const fn infer_us(self) -> Option<u64> {
        match self {
            Self::DeadlineExceeded { infer_us } | Self::Perception { infer_us, .. } => {
                Some(infer_us)
            }
            #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
            Self::NoDetection { infer_us } | Self::RuntimeError { infer_us, .. } => Some(infer_us),
            Self::InjectedInvalidOutput | Self::WorkerDisconnected => None,
            #[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
            Self::UnsupportedPlatform => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InferenceOutput {
    detection: Option<YoloDetection>,
    control_value: i32,
    infer_us: u64,
    source: TargetSource,
}

struct InferenceWorker {
    receiver: Receiver<Result<InferenceOutput, InferenceRejection>>,
}

struct ControlLoop {
    mode: RunMode,
    request_id: u32,
    request_in_flight: bool,
    pending_send: bool,
    request_sent_at_ms: u64,
    next_send_at_ms: u64,
    successful_cycles: u32,
    first_success_reported: bool,
    status_received: bool,
    fault_injected: bool,
    protocol_safe_observed: bool,
    recovery_pending: bool,
    model_rejected: bool,
    inference_worker: Option<InferenceWorker>,
    discard_inference_result: bool,
    last_target: i32,
    last_state: i32,
    samples: Vec<ImageSample>,
    sample_index: usize,
    experiment_complete: bool,
}

fn main() {
    if let Err(error) = run() {
        fail(&error);
    }
}

fn run() -> Result<(), String> {
    let mode = parse_run_mode()?;
    let samples = if mode.is_ab_experiment() {
        load_task3_ab_manifest()?
    } else {
        Vec::new()
    };
    report_experiment_ready(mode, &samples);
    let initial_sample = samples.first().cloned();
    if mode.requires_model() {
        println!("TASK3_INFER_STARTED model=yolo11n.ncnn request=1 phase=startup");
    } else {
        println!("TASK3_MANUAL_STARTED request=1 target={CONTROL_TARGET} phase=startup");
    }
    let initial_inference = make_control_decision(mode, CONTROL_TARGET, initial_sample);

    if mode == RunMode::ModelOnly {
        return match initial_inference {
            Ok(inference) => {
                println!(
                    "TASK3_INFER model=yolo11n.ncnn infer_us={} request=1 elapsed_ms=0",
                    inference.infer_us
                );
                report_detection(&inference, 1);
                Ok(())
            }
            Err(reason) => {
                println!(
                    "TASK3_MODEL_REJECTED model=yolo11n.ncnn reason={reason:?} action=safe \
                     elapsed_ms=0"
                );
                Err(format!("model inference rejected: {reason:?}"))
            }
        };
    }

    let socket = UdpSocket::bind("0.0.0.0:4242").map_err(|error| format!("bind error={error}"))?;
    socket
        .set_nonblocking(true)
        .map_err(|error| format!("nonblocking error={error}"))?;
    let peer: SocketAddr = "10.0.42.2:4242"
        .parse()
        .map_err(|error| format!("peer address error={error}"))?;
    let started = Instant::now();
    let mut endpoint = Endpoint::new(SESSION, POLICY, 0);
    let mut control = ControlLoop::with_initial_inference(mode, samples, initial_inference);
    let mut inbound = [0u8; MAX_DATAGRAM_LEN];
    let mut response = [0u8; MAX_DATAGRAM_LEN];
    let mut outbound = [0u8; MAX_DATAGRAM_LEN];

    println!(
        "STARRY_T2N1_READY local=0.0.0.0:4242 peer={peer} session=0x{:08x} mode={}",
        SESSION.get(),
        mode.name()
    );
    control.send_if_due(&socket, &peer, &mut endpoint, &mut outbound, 0)?;

    loop {
        let now_ms = elapsed_ms(&started);
        receive_datagram(
            &socket,
            &peer,
            &mut endpoint,
            &mut control,
            &mut inbound,
            &mut response,
            now_ms,
        )?;
        poll_endpoint(
            &socket,
            &peer,
            &mut endpoint,
            &mut control,
            &mut outbound,
            now_ms,
        )?;
        control.send_if_due(&socket, &peer, &mut endpoint, &mut outbound, now_ms)?;
        control.report_first_success(&endpoint);
        thread::sleep(Duration::from_millis(10));
    }
}

fn parse_run_mode() -> Result<RunMode, String> {
    let mut arguments = env::args().skip(1);
    let mode_argument = arguments.next();
    let mode = RunMode::parse(mode_argument.as_deref()).map_err(str::to_owned)?;
    if arguments.next().is_some() {
        return Err("only one run-mode argument is accepted".to_owned());
    }
    Ok(mode)
}

fn report_experiment_ready(mode: RunMode, samples: &[ImageSample]) {
    if mode == RunMode::Manual {
        println!(
            "TASK3_EXPERIMENT_READY run_mode=manual source=manual frozen_target={CONTROL_TARGET} \
             samples={}",
            samples.len()
        );
        return;
    }
    println!(
        "TASK3_MODEL_READY model=yolo11n.ncnn runtime=ncnn ncnn_revision={NCNN_REVISION} \
         param_sha256={MODEL_PARAM_SHA256} bin_sha256={MODEL_BIN_SHA256} \
         input_sha256={MODEL_INPUT_SHA256} input_manifest_sha256={AB_MANIFEST_SHA256} \
         mode=in-guest run_mode={} samples={}",
        mode.name(),
        samples.len()
    );
}

fn report_detection(inference: &InferenceOutput, request_id: u32) {
    match inference.detection {
        Some(detection) => println!(
            "TASK3_DETECTION model=yolo11n.ncnn class={} confidence_milli={} center_x_milli={} \
             area_milli={} target={} request={request_id}",
            detection.class_id,
            detection.confidence_milli,
            detection.center_x_milli,
            detection.area_milli,
            inference.control_value
        ),
        None => println!(
            "TASK3_MANUAL_TARGET source=manual target={} infer_us=0 request={request_id}",
            inference.control_value
        ),
    }
}

fn format_truth_target(target: Option<i32>) -> String {
    target.map_or_else(|| "none".to_owned(), |value| value.to_string())
}

fn receive_datagram(
    socket: &UdpSocket,
    peer: &SocketAddr,
    endpoint: &mut Endpoint,
    control: &mut ControlLoop,
    inbound: &mut [u8; MAX_DATAGRAM_LEN],
    response: &mut [u8; MAX_DATAGRAM_LEN],
    now_ms: u64,
) -> Result<(), String> {
    let (length, source) = match socket.recv_from(inbound) {
        Ok(received) => received,
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
        Err(error) => return Err(format!("recv error={error}")),
    };
    let state_before_receive = endpoint.state();
    let result = endpoint
        .receive(&inbound[..length], now_ms, response)
        .map_err(|error| format!("receive error={error}"))?;
    if result.response_len > 0 {
        send_datagram(
            socket,
            &source,
            &response[..result.response_len],
            "response",
        )?;
    }
    control.handle_receive_event(result.event, endpoint, now_ms);

    match (state_before_receive, endpoint.state()) {
        (EndpointState::Active, EndpointState::Safe) => {
            control.enter_protocol_safe("receive", now_ms);
        }
        (EndpointState::Safe, EndpointState::Active) => {
            control.recover(now_ms);
        }
        _ => {}
    }
    if source != *peer {
        println!("STARRY_T2N1_EVENT unexpected_source={source}");
    }
    Ok(())
}

fn poll_endpoint(
    socket: &UdpSocket,
    peer: &SocketAddr,
    endpoint: &mut Endpoint,
    control: &mut ControlLoop,
    outbound: &mut [u8; MAX_DATAGRAM_LEN],
    now_ms: u64,
) -> Result<(), String> {
    let poll = endpoint
        .poll(now_ms, outbound)
        .map_err(|error| format!("timer error={error}"))?;
    if poll.datagram_len > 0 {
        send_datagram(socket, peer, &outbound[..poll.datagram_len], "timer")?;
    }
    match poll.event {
        PollEvent::Retransmit { sequence, attempt } => println!(
            "STARRY_T2N1_RETRANSMIT seq={} attempt={attempt}",
            sequence.get()
        ),
        PollEvent::RetryExhausted { sequence } => {
            println!(
                "STARRY_T2N1_RETRY_EXHAUSTED seq={} elapsed_ms={now_ms}",
                sequence.get()
            );
            control.enter_protocol_safe("RetryExhausted", now_ms);
        }
        PollEvent::HeartbeatTimeout => {
            control.enter_protocol_safe("HeartbeatTimeout", now_ms);
        }
        PollEvent::Idle | PollEvent::HeartbeatSent => {}
    }
    Ok(())
}

impl ControlLoop {
    const fn new(mode: RunMode) -> Self {
        Self {
            mode,
            request_id: 1,
            request_in_flight: false,
            pending_send: false,
            request_sent_at_ms: 0,
            next_send_at_ms: 0,
            successful_cycles: 0,
            first_success_reported: false,
            status_received: false,
            fault_injected: false,
            protocol_safe_observed: false,
            recovery_pending: false,
            model_rejected: false,
            inference_worker: None,
            discard_inference_result: false,
            last_target: CONTROL_TARGET,
            last_state: 300,
            samples: Vec::new(),
            sample_index: 0,
            experiment_complete: false,
        }
    }

    fn with_initial_inference(
        mode: RunMode,
        samples: Vec<ImageSample>,
        initial_inference: Result<InferenceOutput, InferenceRejection>,
    ) -> Self {
        let mut control = Self::new(mode);
        control.samples = samples;
        control.inference_worker = Some(InferenceWorker::completed(initial_inference));
        control
    }

    fn send_if_due(
        &mut self,
        socket: &UdpSocket,
        peer: &SocketAddr,
        endpoint: &mut Endpoint,
        outbound: &mut [u8; MAX_DATAGRAM_LEN],
        now_ms: u64,
    ) -> Result<(), String> {
        if endpoint.state() != EndpointState::Active
            || self.request_in_flight
            || self.model_rejected
            || self.experiment_complete
            || now_ms < self.next_send_at_ms
        {
            return Ok(());
        }
        if endpoint.has_pending_frame() {
            if !self.pending_send {
                println!("STARRY_T2N1_CONTROL_DEFERRED awaiting_ack=true");
            }
            self.pending_send = true;
            return Ok(());
        }
        self.pending_send = false;

        if !self.fault_injected
            && matches!(self.mode, RunMode::OutOfOrder | RunMode::InvalidParameter)
        {
            return self.send_fault(socket, peer, outbound, now_ms);
        }
        self.send_inference_control(socket, peer, endpoint, outbound, now_ms)
    }

    fn send_fault(
        &mut self,
        socket: &UdpSocket,
        peer: &SocketAddr,
        outbound: &mut [u8; MAX_DATAGRAM_LEN],
        now_ms: u64,
    ) -> Result<(), String> {
        let (datagram_len, sequence) = encode_fault_frame(self.mode, self.request_id, outbound)?
            .ok_or_else(|| "run mode does not encode a protocol fault".to_owned())?;
        send_datagram(socket, peer, &outbound[..datagram_len], "fault")?;
        self.fault_injected = true;
        self.request_in_flight = true;
        self.request_sent_at_ms = now_ms;
        println!(
            "STARRY_T2N1_FAULT_SENT mode={} seq={} request={} elapsed_ms={now_ms}",
            self.mode.name(),
            sequence.get(),
            self.request_id
        );
        Ok(())
    }

    fn send_inference_control(
        &mut self,
        socket: &UdpSocket,
        peer: &SocketAddr,
        endpoint: &mut Endpoint,
        outbound: &mut [u8; MAX_DATAGRAM_LEN],
        now_ms: u64,
    ) -> Result<(), String> {
        if self.inference_worker.is_none() {
            self.start_inference(now_ms);
            return Ok(());
        }
        let Some(inference_result) = self
            .inference_worker
            .as_ref()
            .and_then(InferenceWorker::try_finish)
        else {
            return Ok(());
        };
        self.inference_worker = None;
        if self.discard_inference_result {
            self.discard_inference_result = false;
            println!("TASK3_INFER_DISCARDED reason=protocol_safe elapsed_ms={now_ms}");
            self.start_inference(now_ms);
            return Ok(());
        }

        let inference = match inference_result {
            Ok(inference) => inference,
            Err(reason) => {
                if self.mode == RunMode::Yolo {
                    if let Some(infer_us) = reason.infer_us() {
                        println!(
                            "TASK3_INFER model=yolo11n.ncnn source=yolo infer_us={infer_us} \
                             request={} elapsed_ms={now_ms}",
                            self.request_id
                        );
                    }
                    self.report_current_sample("rejected", now_ms);
                    println!(
                        "TASK3_MODEL_REJECTED model=yolo11n.ncnn reason={reason:?} action=safe \
                         request={} elapsed_ms={now_ms}",
                        self.request_id
                    );
                    println!("STARRY_T2N1_SAFE source=model reason={reason:?} elapsed_ms={now_ms}");
                    self.finish_rejected_ab_sample(now_ms);
                    return Ok(());
                }
                self.model_rejected = true;
                println!(
                    "TASK3_MODEL_REJECTED model=yolo11n.ncnn reason={reason:?} action=safe \
                     elapsed_ms={now_ms}"
                );
                println!("STARRY_T2N1_SAFE source=model reason={reason:?} elapsed_ms={now_ms}");
                return Ok(());
            }
        };
        self.report_current_sample("accepted", now_ms);
        if inference.source == TargetSource::Yolo {
            println!(
                "TASK3_INFER model=yolo11n.ncnn source=yolo infer_us={} request={} \
                 elapsed_ms={now_ms}",
                inference.infer_us, self.request_id
            );
        } else {
            println!(
                "TASK3_MANUAL_INPUT source=manual target={} infer_us=0 request={} \
                 elapsed_ms={now_ms}",
                inference.control_value, self.request_id
            );
        }
        report_detection(&inference, self.request_id);

        let mut payload = [0u8; 12];
        let command = ControlMessage::new(
            ControlAction::SetOutput,
            inference.control_value,
            self.request_id,
        )
        .map_err(|error| format!("control construction error={error}"))?;
        let payload_len = command
            .encode(&mut payload)
            .map_err(|error| format!("control encoding error={error}"))?;
        let transmission = endpoint
            .queue_reliable(
                MessageKind::Control,
                &payload[..payload_len],
                now_ms,
                outbound,
            )
            .map_err(|error| format!("queue error={error}"))?;
        send_datagram(
            socket,
            peer,
            &outbound[..transmission.datagram_len()],
            "control",
        )?;
        self.request_in_flight = true;
        self.request_sent_at_ms = now_ms;
        self.status_received = false;
        self.last_target = inference.control_value;
        println!(
            "STARRY_T2N1_CONTROL_SENT seq={} value={} request={} elapsed_ms={now_ms}",
            transmission.sequence().get(),
            inference.control_value,
            self.request_id
        );
        if let Some(sample) = self.current_sample() {
            println!(
                "TASK3_CONTROL_SENT elapsed_ms={now_ms} sample={} image_id={} source={} \
                 request={} value={} truth_target={} state={} seq={}",
                self.sample_index + 1,
                sample.id,
                inference.source.name(),
                self.request_id,
                inference.control_value,
                format_truth_target(sample.truth_target),
                self.last_state,
                transmission.sequence().get()
            );
        }
        Ok(())
    }

    fn start_inference(&mut self, now_ms: u64) {
        if self.mode.requires_model() {
            println!(
                "TASK3_INFER_STARTED model=yolo11n.ncnn request={} elapsed_ms={now_ms}",
                self.request_id
            );
        } else {
            println!(
                "TASK3_MANUAL_STARTED request={} target={CONTROL_TARGET} elapsed_ms={now_ms}",
                self.request_id
            );
        }
        self.inference_worker = Some(InferenceWorker::start(
            self.mode,
            self.last_target,
            self.current_sample().cloned(),
        ));
    }

    fn handle_receive_event(&mut self, event: ReceiveEvent<'_>, endpoint: &Endpoint, now_ms: u64) {
        match event {
            ReceiveEvent::Acknowledged { sequence } => {
                println!("STARRY_T2N1_ACK seq={}", sequence.get());
            }
            ReceiveEvent::Delivered { frame } if frame.kind() == MessageKind::Status => {
                match StatusMessage::decode(frame.payload()) {
                    Ok(status)
                        if self.request_in_flight
                            && status.last_control_request() == self.request_id =>
                    {
                        let rtt_ms = now_ms.saturating_sub(self.request_sent_at_ms);
                        self.request_in_flight = false;
                        self.status_received = true;
                        self.successful_cycles = self.successful_cycles.saturating_add(1);
                        self.next_send_at_ms = now_ms + CONTROL_INTERVAL_MS;
                        let state_before = self.last_state;
                        self.last_state = status.value();
                        println!(
                            "STARRY_T2N1_STATUS_DELIVERED seq={} bytes={} request={} state={:?} \
                             value={} rtt_ms={rtt_ms}",
                            frame.sequence().get(),
                            frame.payload().len(),
                            status.last_control_request(),
                            status.state(),
                            status.value()
                        );
                        if let Some(sample) = self.current_sample() {
                            println!(
                                "TASK3_STATUS_RECEIVED elapsed_ms={now_ms} sample={} image_id={} \
                                 request={} value={} state_before={} state_after={} \
                                 rtt_ms={rtt_ms}",
                                self.sample_index + 1,
                                sample.id,
                                status.last_control_request(),
                                self.last_target,
                                state_before,
                                status.value()
                            );
                        }
                        if self.recovery_pending {
                            println!(
                                "STARRY_T2N1_FAULT_RECOVERY_COMPLETE mode={} request={} \
                                 safe_observed=true recovered=true elapsed_ms={now_ms}",
                                self.mode.name(),
                                status.last_control_request()
                            );
                            self.recovery_pending = false;
                            self.protocol_safe_observed = false;
                        }
                        self.request_id = next_request_id(self.request_id);
                        if self.mode.is_ab_experiment() {
                            self.finish_ab_sample(now_ms);
                        }
                    }
                    Ok(status) => println!(
                        "STARRY_T2N1_STATUS_IGNORED seq={} request={} in_flight={}",
                        frame.sequence().get(),
                        status.last_control_request(),
                        self.request_in_flight
                    ),
                    Err(error) => println!("STARRY_T2N1_EVENT status_decode_error={error}"),
                }
            }
            ReceiveEvent::RemoteError { code, sequence } => println!(
                "STARRY_T2N1_REMOTE_ERROR code={code:?} sequence={} elapsed_ms={now_ms}",
                sequence.get()
            ),
            ReceiveEvent::OutOfOrder { sequence, expected } => println!(
                "STARRY_T2N1_EVENT out_of_order={} expected={}",
                sequence.get(),
                expected.get()
            ),
            ReceiveEvent::InvalidPayload { error } => {
                println!("STARRY_T2N1_EVENT invalid_payload={error}");
            }
            ReceiveEvent::Duplicate { sequence } => {
                println!("STARRY_T2N1_DUPLICATE seq={}", sequence.get());
            }
            ReceiveEvent::DuplicateAcknowledgement { sequence } => {
                println!("STARRY_T2N1_DUPLICATE_ACK seq={}", sequence.get());
            }
            ReceiveEvent::Rejected { error } => {
                println!("STARRY_T2N1_REJECTED error={error}");
            }
            ReceiveEvent::SessionMismatch => println!("STARRY_T2N1_REJECTED session_mismatch"),
            ReceiveEvent::Heartbeat { .. } | ReceiveEvent::Delivered { .. } => {}
        }
        self.report_first_success(endpoint);
    }

    fn enter_protocol_safe(&mut self, reason: &str, now_ms: u64) {
        self.protocol_safe_observed = true;
        if self.request_in_flight {
            self.request_id = next_request_id(self.request_id);
        }
        self.request_in_flight = false;
        self.pending_send = false;
        self.status_received = false;
        self.discard_inference_result = self.inference_worker.is_some();
        println!("STARRY_T2N1_SAFE source=protocol reason={reason} elapsed_ms={now_ms}");
    }

    fn recover(&mut self, now_ms: u64) {
        if self.protocol_safe_observed {
            self.recovery_pending = true;
        }
        self.request_in_flight = false;
        self.pending_send = false;
        self.status_received = false;
        self.next_send_at_ms = if self.recovery_pending {
            now_ms + CONTROL_INTERVAL_MS
        } else {
            now_ms
        };
        println!("STARRY_T2N1_RECOVERED state=Active elapsed_ms={now_ms}");
    }

    fn report_first_success(&mut self, endpoint: &Endpoint) {
        if !self.first_success_reported
            && self.successful_cycles >= 1
            && self.status_received
            && !endpoint.has_pending_frame()
            && endpoint.state() == EndpointState::Active
        {
            self.first_success_reported = true;
            println!("STARRY_T2N1_PASS");
        }
    }

    fn current_sample(&self) -> Option<&ImageSample> {
        self.samples.get(self.sample_index)
    }

    fn report_current_sample(&self, outcome: &str, now_ms: u64) {
        let Some(sample) = self.current_sample() else {
            return;
        };
        println!(
            "TASK3_SAMPLE sample={} image_id={} image_sha256={} truth_target={} expected={} \
             source={} outcome={outcome} request={} elapsed_ms={now_ms}",
            self.sample_index + 1,
            sample.id,
            sample.sha256,
            format_truth_target(sample.truth_target),
            sample.expected.name(),
            if self.mode == RunMode::Manual {
                "manual"
            } else {
                "yolo"
            },
            self.request_id
        );
    }

    fn finish_ab_sample(&mut self, now_ms: u64) {
        self.sample_index += 1;
        self.next_send_at_ms = now_ms + CONTROL_INTERVAL_MS;
        if self.sample_index >= self.samples.len() {
            self.experiment_complete = true;
            println!(
                "TASK3_EXPERIMENT_COMPLETE run_mode={} samples={} elapsed_ms={now_ms}",
                self.mode.name(),
                self.samples.len()
            );
        }
    }

    fn finish_rejected_ab_sample(&mut self, now_ms: u64) {
        self.request_id = next_request_id(self.request_id);
        self.finish_ab_sample(now_ms);
    }
}

impl InferenceWorker {
    fn completed(result: Result<InferenceOutput, InferenceRejection>) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        sender
            .send(result)
            .expect("new inference channel must have its receiver");
        Self { receiver }
    }

    fn start(mode: RunMode, current_target: i32, sample: Option<ImageSample>) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            // A Safe transition may intentionally drop the receiver while ncnn
            // finishes the non-cancellable forward pass.
            let _ = sender.send(make_control_decision(mode, current_target, sample));
        });
        Self { receiver }
    }

    fn try_finish(&self) -> Option<Result<InferenceOutput, InferenceRejection>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(InferenceRejection::WorkerDisconnected)),
        }
    }
}

fn run_inference(
    mode: RunMode,
    current_target: i32,
) -> Result<InferenceOutput, InferenceRejection> {
    run_inference_with_input(mode, current_target, MODEL_INPUT_PATH)
}

fn make_control_decision(
    mode: RunMode,
    current_target: i32,
    sample: Option<ImageSample>,
) -> Result<InferenceOutput, InferenceRejection> {
    if mode == RunMode::Manual {
        return Ok(InferenceOutput {
            detection: None,
            control_value: CONTROL_TARGET,
            infer_us: 0,
            source: TargetSource::Manual,
        });
    }
    if mode != RunMode::Yolo {
        return run_inference(mode, current_target);
    }
    let sample = sample.ok_or(InferenceRejection::InjectedInvalidOutput)?;
    let input_path = CString::new(sample.installed_path())
        .map_err(|_| InferenceRejection::InjectedInvalidOutput)?;
    run_inference_with_input(mode, current_target, &input_path)
}

fn run_inference_with_input(
    mode: RunMode,
    current_target: i32,
    input_path: &CStr,
) -> Result<InferenceOutput, InferenceRejection> {
    if mode == RunMode::ModelRejected {
        return Err(InferenceRejection::InjectedInvalidOutput);
    }
    let (raw_detection, infer_us) = ncnn::infer(MODEL_PARAM_PATH, MODEL_BIN_PATH, input_path)
        .map_err(|error| match error {
            #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
            ncnn::Error::NoDetection { infer_us } => InferenceRejection::NoDetection { infer_us },
            #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
            ncnn::Error::Runtime { code, infer_us } => {
                InferenceRejection::RuntimeError { code, infer_us }
            }
            #[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
            ncnn::Error::UnsupportedPlatform => InferenceRejection::UnsupportedPlatform,
        })?;
    let detection = YoloDetection {
        class_id: raw_detection.class_id,
        confidence_milli: raw_detection.confidence_milli,
        center_x_milli: raw_detection.center_x_milli,
        area_milli: raw_detection.area_milli,
    };
    validate_detection(detection, infer_us, current_target, mode)
}

fn validate_detection(
    detection: YoloDetection,
    infer_us: u64,
    current_target: i32,
    mode: RunMode,
) -> Result<InferenceOutput, InferenceRejection> {
    // model-only is a latency-probe driver: it must report the inference
    // result even when the shared hypervisor scheduler makes it slow.
    if infer_us > MAX_INFERENCE_US && mode != RunMode::ModelOnly {
        return Err(InferenceRejection::DeadlineExceeded { infer_us });
    }
    match yolo_detection_to_target(detection, current_target, YoloPolicy::task3_default()) {
        PerceptionDecision::Target { target, .. } => Ok(InferenceOutput {
            detection: Some(detection),
            control_value: target,
            infer_us,
            source: TargetSource::Yolo,
        }),
        PerceptionDecision::Reject(reason) => {
            Err(InferenceRejection::Perception { reason, infer_us })
        }
    }
}

fn encode_fault_frame(
    mode: RunMode,
    request_id: u32,
    output: &mut [u8; MAX_DATAGRAM_LEN],
) -> Result<Option<(usize, SequenceNumber)>, String> {
    let sequence = match mode {
        RunMode::OutOfOrder => SequenceNumber::from_wire(2),
        RunMode::InvalidParameter => SequenceNumber::FIRST,
        RunMode::Normal
        | RunMode::ModelOnly
        | RunMode::Manual
        | RunMode::Yolo
        | RunMode::ModelRejected => return Ok(None),
    };
    let mut payload = [0u8; 12];
    match mode {
        RunMode::OutOfOrder => {
            ControlMessage::new(ControlAction::SetOutput, CONTROL_TARGET, request_id)
                .map_err(|error| format!("fault control construction error={error}"))?
                .encode(&mut payload)
                .map_err(|error| format!("fault control encoding error={error}"))?;
        }
        RunMode::InvalidParameter => {
            payload[0] = ControlAction::SetOutput as u8;
            payload[4..8].copy_from_slice(&(ControlMessage::MAX_OUTPUT_VALUE + 1).to_be_bytes());
            payload[8..12].copy_from_slice(&request_id.to_be_bytes());
        }
        RunMode::Normal
        | RunMode::ModelOnly
        | RunMode::Manual
        | RunMode::Yolo
        | RunMode::ModelRejected => unreachable!(),
    }
    let frame = Frame::reliable(MessageKind::Control, SESSION, sequence, &payload)
        .map_err(|error| format!("fault frame construction error={error}"))?;
    let datagram_len = frame
        .encode(output)
        .map_err(|error| format!("fault frame encoding error={error}"))?;
    Ok(Some((datagram_len, sequence)))
}

fn send_datagram(
    socket: &UdpSocket,
    destination: &SocketAddr,
    datagram: &[u8],
    operation: &str,
) -> Result<(), String> {
    let sent = socket
        .send_to(datagram, destination)
        .map_err(|error| format!("send {operation} error={error}"))?;
    if sent != datagram.len() {
        return Err(format!(
            "send {operation} short datagram sent={sent} expected={}",
            datagram.len()
        ));
    }
    Ok(())
}

const fn next_request_id(current: u32) -> u32 {
    let next = current.wrapping_add(1);
    if next == 0 { 1 } else { next }
}

fn elapsed_ms(started: &Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

fn fail(message: &str) -> ! {
    println!("STARRY_T2N1_FAIL {message}");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use task2_net_protocol::PayloadError;

    use super::*;
    use crate::experiment::ExpectedBehavior;

    #[test]
    fn parses_supported_run_modes() {
        assert_eq!(RunMode::parse(None), Ok(RunMode::Normal));
        assert_eq!(RunMode::parse(Some("normal")), Ok(RunMode::Normal));
        assert_eq!(RunMode::parse(Some("manual")), Ok(RunMode::Manual));
        assert_eq!(RunMode::parse(Some("yolo")), Ok(RunMode::Yolo));
        assert_eq!(
            RunMode::parse(Some("out-of-order")),
            Ok(RunMode::OutOfOrder)
        );
        assert_eq!(
            RunMode::parse(Some("invalid-parameter")),
            Ok(RunMode::InvalidParameter)
        );
        assert_eq!(
            RunMode::parse(Some("model-rejected")),
            Ok(RunMode::ModelRejected)
        );
        assert!(RunMode::parse(Some("unknown")).is_err());
    }

    #[test]
    fn validates_yolo_detection_before_control() {
        let detection = YoloDetection {
            class_id: 75,
            confidence_milli: 843,
            center_x_milli: 421,
            area_milli: 63,
        };

        assert_eq!(
            validate_detection(detection, 13_000_000, CONTROL_TARGET, RunMode::Normal,),
            Ok(InferenceOutput {
                detection: Some(detection),
                control_value: 421,
                infer_us: 13_000_000,
                source: TargetSource::Yolo,
            })
        );
        assert_eq!(
            validate_detection(
                detection,
                MAX_INFERENCE_US + 1,
                CONTROL_TARGET,
                RunMode::Normal,
            ),
            Err(InferenceRejection::DeadlineExceeded {
                infer_us: MAX_INFERENCE_US + 1,
            })
        );

        let low_confidence = YoloDetection {
            confidence_milli: 599,
            ..detection
        };
        assert_eq!(
            validate_detection(low_confidence, 1, CONTROL_TARGET, RunMode::Normal),
            Err(InferenceRejection::Perception {
                reason: PerceptionRejectReason::LowConfidence,
                infer_us: 1,
            })
        );
    }

    #[test]
    fn rejected_ab_sample_advances_request_identity() {
        let mut control = ControlLoop::new(RunMode::Yolo);
        control.samples = load_task3_ab_manifest().unwrap();
        control.sample_index = 3;
        control.request_id = 4;

        control.finish_rejected_ab_sample(100);

        assert_eq!(control.sample_index, 4);
        assert_eq!(control.request_id, 5);
        assert!(!control.experiment_complete);
    }

    #[test]
    fn task3_ab_manifest_freezes_order_truth_and_expected_behavior() {
        let samples = load_task3_ab_manifest().unwrap();

        assert_eq!(
            samples
                .iter()
                .map(|sample| sample.id.as_str())
                .collect::<Vec<_>>(),
            [
                "vase-left",
                "vase-center",
                "vase-right",
                "no-target",
                "small-target"
            ]
        );
        assert_eq!(samples[0].truth_target, Some(321));
        assert_eq!(samples[1].truth_target, Some(421));
        assert_eq!(samples[2].truth_target, Some(521));
        assert_eq!(samples[3].truth_target, None);
        assert_eq!(samples[4].truth_target, Some(500));
        assert_eq!(samples[0].expected, ExpectedBehavior::Accept);
        assert_eq!(samples[3].expected, ExpectedBehavior::Reject);
        assert_eq!(samples[4].expected, ExpectedBehavior::Reject);
    }

    #[test]
    fn manual_mode_returns_frozen_target_without_a_detection() {
        let sample = load_task3_ab_manifest().unwrap().remove(0);

        let output = make_control_decision(RunMode::Manual, CONTROL_TARGET, Some(sample)).unwrap();

        assert_eq!(output.control_value, CONTROL_TARGET);
        assert_eq!(output.infer_us, 0);
        assert_eq!(output.detection, None);
        assert_eq!(output.source, TargetSource::Manual);
    }

    #[test]
    fn model_rejection_mode_does_not_call_the_runtime() {
        assert_eq!(
            run_inference(RunMode::ModelRejected, CONTROL_TARGET),
            Err(InferenceRejection::InjectedInvalidOutput)
        );
    }

    #[test]
    fn out_of_order_mode_emits_valid_sequence_two_control() {
        let mut datagram = [0; MAX_DATAGRAM_LEN];
        let (length, sequence) = encode_fault_frame(RunMode::OutOfOrder, 7, &mut datagram)
            .unwrap()
            .unwrap();
        let frame = Frame::parse(&datagram[..length]).unwrap();

        assert_eq!(sequence, SequenceNumber::from_wire(2));
        assert_eq!(frame.sequence(), SequenceNumber::from_wire(2));
        assert_eq!(frame.kind(), MessageKind::Control);
        assert_eq!(
            ControlMessage::decode(frame.payload())
                .unwrap()
                .request_id(),
            7
        );
    }

    #[test]
    fn invalid_parameter_mode_preserves_framing_and_crc() {
        let mut datagram = [0; MAX_DATAGRAM_LEN];
        let (length, sequence) = encode_fault_frame(RunMode::InvalidParameter, 9, &mut datagram)
            .unwrap()
            .unwrap();
        let frame = Frame::parse(&datagram[..length]).unwrap();

        assert_eq!(sequence, SequenceNumber::FIRST);
        assert_eq!(frame.sequence(), SequenceNumber::FIRST);
        assert_eq!(
            ControlMessage::decode(frame.payload()),
            Err(PayloadError::InvalidControlValue)
        );
    }

    #[test]
    fn safe_and_recovery_reset_application_request_lifecycle() {
        let mut control = ControlLoop::new(RunMode::Normal);
        control.request_id = 41;
        control.request_in_flight = true;
        control.pending_send = true;
        control.status_received = true;

        control.enter_protocol_safe("test", 100);

        assert_eq!(control.request_id, 42);
        assert!(!control.request_in_flight);
        assert!(!control.pending_send);
        assert!(!control.status_received);

        control.next_send_at_ms = 999;
        control.recover(123);

        assert_eq!(control.next_send_at_ms, 123 + CONTROL_INTERVAL_MS);
        assert!(!control.request_in_flight);
        assert!(!control.pending_send);
        assert!(control.protocol_safe_observed);
        assert!(control.recovery_pending);
    }
}
