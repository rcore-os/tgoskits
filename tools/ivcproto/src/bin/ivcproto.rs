use std::{
    fs::File,
    io::{ErrorKind, Write},
    net::{SocketAddr, UdpSocket},
    path::PathBuf,
    process::ExitCode,
    str::FromStr,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ivcproto::{
    control::{
        AckPayload, ControlCommand, ControlMode, ControlOperation, ErrorReport, StatusReport,
    },
    controller_csv::{ControllerSample, write_controller_samples},
    endpoint::{ControlEndpoint, EndpointConfig},
    neural::{
        ManualFixedController, NeuralController, Policy, ScenarioMetrics, ThermalObservation,
        ThermalPlant, evaluate_policy, evaluate_policy_with_observer,
    },
    reliability::{
        AckResult, Delivery, ReceiveWindow, ReliabilityConfig, RetryAction, StopAndWaitSender,
    },
    wire::{
        ErrorCode, Frame, FrameFlags, HEADER_LEN, Header, MAX_PAYLOAD_LEN, MessageType, VERSION,
        decode_frame, encode_frame,
    },
};

const SOCKET_TIMEOUT: Duration = Duration::from_millis(100);
const DEFAULT_ACK_TIMEOUT: Duration = Duration::from_millis(100);
const BOARD_CONSOLE_RECORD_MAX_BYTES: usize = 160;
const BOARD_CONSOLE_RECORD_COPIES: usize = 2;
const BOARD_CONSOLE_RECORD_PAUSE: Duration = Duration::from_millis(10);
const BOARD_CONSOLE_SUMMARY_SETTLE: Duration = Duration::from_millis(250);
const ERROR_FAULT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);
const ERROR_FAULT_RESULT_SETTLE: Duration = Duration::from_millis(750);
const ERROR_FAULT_RESULT_RECORD_COPIES: usize = 3;
const ERROR_FAULT_RESULT_RECORD_PAUSE: Duration = Duration::from_millis(25);
const RESTART_RESULT_SETTLE: Duration = Duration::from_secs(2);
const RESTART_RESULT_RECORD_COPIES: usize = 3;
const RESTART_RESULT_RECORD_PAUSE: Duration = Duration::from_millis(100);
const ERROR_EVIDENCE_RECORD_MAX_BYTES: usize = 96;
const ERROR_FAULT_SEQUENCE_BASE: u32 = 1_000;
const RESTART_PREVIOUS_FINAL_SEQUENCE: u32 = 20;
const RESTART_DUPLICATE_SEQUENCE: u32 = 1;
const RESTART_STALE_CONTROL_SEQUENCE: u32 = RESTART_PREVIOUS_FINAL_SEQUENCE + 1;
const RESTART_PROBE_RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);
const VERSION_OFFSET: usize = 4;
const PAYLOAD_LENGTH_OFFSET: usize = 24;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InferenceBackend {
    Native,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ControllerFaultProfile {
    #[default]
    None,
    Error,
    Restart,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ErrorFaultKind {
    UnsupportedVersion,
    LengthMismatch,
    ChecksumMismatch,
    UnexpectedMessageType,
    InvalidSessionTransition,
}

impl ErrorFaultKind {
    const ALL: [Self; 5] = [
        Self::UnsupportedVersion,
        Self::LengthMismatch,
        Self::ChecksumMismatch,
        Self::UnexpectedMessageType,
        Self::InvalidSessionTransition,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::UnsupportedVersion => "unsupported-version",
            Self::LengthMismatch => "length-mismatch",
            Self::ChecksumMismatch => "checksum-mismatch",
            Self::UnexpectedMessageType => "unexpected-message-type",
            Self::InvalidSessionTransition => "invalid-session-transition",
        }
    }

    const fn expected_error(self) -> ErrorCode {
        match self {
            Self::UnsupportedVersion => ErrorCode::UnsupportedVersion,
            Self::LengthMismatch => ErrorCode::MalformedFrame,
            Self::ChecksumMismatch => ErrorCode::ChecksumMismatch,
            Self::UnexpectedMessageType => ErrorCode::InvalidControl,
            Self::InvalidSessionTransition => ErrorCode::SequenceOutsideWindow,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ErrorFaultProbe {
    kind: ErrorFaultKind,
    sequence: u32,
    offending_type: MessageType,
    expected_error: ErrorCode,
    datagram: Vec<u8>,
}

impl InferenceBackend {
    const fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ControllerArguments {
    peer: String,
    count: u32,
    policy: Policy,
    period_ms: u64,
    session_id: Option<u32>,
    backend: InferenceBackend,
    raw_csv: Option<PathBuf>,
    fault_profile: ControllerFaultProfile,
    restart_previous_session: Option<u32>,
    ack_timeout: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RestartDuplicateEvidence {
    sequence: u32,
    statuses_received: u64,
    acknowledgements_received: u64,
    stale_acknowledgements_ignored: u64,
    stale_statuses_ignored: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ControlCycleTimeline {
    cycle_started_us: u64,
    command_sent_us: u64,
    response_completed_us: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ControlCycleLatency {
    full_loop_us: u64,
    pre_send_us: u64,
    transport_us: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LatencySummary {
    p50_us: u64,
    p95_us: u64,
    p99_us: u64,
    max_us: u64,
}

impl LatencySummary {
    fn from_sorted_samples(samples: &[u64]) -> Self {
        Self {
            p50_us: percentile(samples, 50),
            p95_us: percentile(samples, 95),
            p99_us: percentile(samples, 99),
            max_us: samples.last().copied().unwrap_or(0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ControllerResultSummary {
    policy: &'static str,
    sent: u64,
    acknowledged: u64,
    errors: u64,
    timeouts: u64,
    retransmissions: u64,
    recoveries: u64,
    success_percent: f64,
    full_loop: LatencySummary,
    pre_send: LatencySummary,
    transport: LatencySummary,
    throughput_msg_s: f64,
    rmse_milli_c: f64,
    iae_milli_c_s: f64,
    max_overshoot_milli_c: i32,
}

impl ControllerResultSummary {
    fn compact_records(self) -> [String; 6] {
        [
            format!(
                "IVC-CONTROLLER-OUTCOME policy={} sent={} acknowledged={} errors={} timeouts={}",
                self.policy, self.sent, self.acknowledged, self.errors, self.timeouts
            ),
            format!(
                "IVC-CONTROLLER-RELIABILITY retransmissions={} recoveries={} success_percent={:.3}",
                self.retransmissions, self.recoveries, self.success_percent
            ),
            format!(
                "IVC-CONTROLLER-FULL-LOOP p50_us={} p95_us={} p99_us={} max_us={}",
                self.full_loop.p50_us,
                self.full_loop.p95_us,
                self.full_loop.p99_us,
                self.full_loop.max_us
            ),
            format!(
                "IVC-CONTROLLER-PRE-SEND p50_us={} p95_us={} p99_us={} max_us={}",
                self.pre_send.p50_us,
                self.pre_send.p95_us,
                self.pre_send.p99_us,
                self.pre_send.max_us
            ),
            format!(
                "IVC-CONTROLLER-TRANSPORT p50_us={} p95_us={} p99_us={} max_us={} \
                 throughput_msg_s={:.3}",
                self.transport.p50_us,
                self.transport.p95_us,
                self.transport.p99_us,
                self.transport.max_us,
                self.throughput_msg_s
            ),
            format!(
                "IVC-CONTROLLER-CONTROL rmse_milli_c={:.3} iae_milli_c_s={:.3} \
                 max_overshoot_milli_c={}",
                self.rmse_milli_c, self.iae_milli_c_s, self.max_overshoot_milli_c
            ),
        ]
    }

    fn legacy_record(self) -> String {
        format!(
            "IVC-CONTROLLER-RESULT policy={} sent={} acknowledged={} errors={} timeouts={} \
             retransmissions={} recoveries={} success_percent={:.3} full_loop_p50_us={} \
             full_loop_p95_us={} full_loop_p99_us={} full_loop_max_us={} pre_send_p50_us={} \
             pre_send_p95_us={} pre_send_p99_us={} pre_send_max_us={} transport_p50_us={} \
             transport_p95_us={} transport_p99_us={} transport_max_us={} throughput_msg_s={:.3} \
             rmse_milli_c={:.3} iae_milli_c_s={:.3} max_overshoot_milli_c={}",
            self.policy,
            self.sent,
            self.acknowledged,
            self.errors,
            self.timeouts,
            self.retransmissions,
            self.recoveries,
            self.success_percent,
            self.full_loop.p50_us,
            self.full_loop.p95_us,
            self.full_loop.p99_us,
            self.full_loop.max_us,
            self.pre_send.p50_us,
            self.pre_send.p95_us,
            self.pre_send.p99_us,
            self.pre_send.max_us,
            self.transport.p50_us,
            self.transport.p95_us,
            self.transport.p99_us,
            self.transport.max_us,
            self.throughput_msg_s,
            self.rmse_milli_c,
            self.iae_milli_c_s,
            self.max_overshoot_milli_c,
        )
    }
}

fn measure_control_cycle(timeline: ControlCycleTimeline) -> Result<ControlCycleLatency, String> {
    let pre_send_us = timeline
        .command_sent_us
        .checked_sub(timeline.cycle_started_us)
        .ok_or_else(|| "control command was sent before its cycle started".to_owned())?;
    let transport_us = timeline
        .response_completed_us
        .checked_sub(timeline.command_sent_us)
        .ok_or_else(|| "control response completed before command send".to_owned())?;
    let full_loop_us = timeline
        .response_completed_us
        .checked_sub(timeline.cycle_started_us)
        .ok_or_else(|| "control response completed before its cycle started".to_owned())?;
    Ok(ControlCycleLatency {
        full_loop_us,
        pre_send_us,
        transport_us,
    })
}

fn main() -> ExitCode {
    match run(std::env::args().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ivcproto: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(mut arguments: impl Iterator<Item = String>) -> Result<(), String> {
    match arguments.next().as_deref() {
        Some("evaluate") => evaluate(),
        Some("evaluate-csv") => {
            let output = required(&mut arguments, "output CSV path")?;
            evaluate_csv(&output)
        }
        Some("rtos-sim") => {
            let bind = required(&mut arguments, "bind address")?;
            let expected = parse(
                &required(&mut arguments, "expected command count")?,
                "count",
            )?;
            let drop_every = optional_parse(arguments.next(), "drop-every")?.unwrap_or(0);
            run_rtos_sim(&bind, expected, drop_every)
        }
        Some("controller") => run_controller(parse_controller_arguments(arguments)?),
        Some(command) => Err(format!("unsupported command '{command}'\n{}", usage())),
        None => Err(usage().to_owned()),
    }
}

fn evaluate() -> Result<(), String> {
    let manual = evaluate_policy(Policy::ManualFixed {
        actuator_permille: 500,
    })
    .map_err(|error| error.to_string())?;
    let neural = evaluate_policy(Policy::Neural).map_err(|error| error.to_string())?;
    print_scenario_metrics("manual-fixed", manual);
    print_scenario_metrics("neural", neural);
    println!(
        "comparison,rmse_improvement_percent={:.3},iae_improvement_percent={:.3}",
        improvement(manual.rmse_milli_c, neural.rmse_milli_c),
        improvement(manual.iae_milli_c_s, neural.iae_milli_c_s)
    );
    Ok(())
}

fn evaluate_csv(output: &str) -> Result<(), String> {
    let mut file = File::create(output).map_err(|error| format!("create {output}: {error}"))?;
    writeln!(
        file,
        "policy,step,elapsed_ms,setpoint_milli_c,measured_milli_c,actuator_permille,error_milli_c"
    )
    .map_err(|error| format!("write {output}: {error}"))?;

    for (name, policy) in [
        (
            "manual-fixed",
            Policy::ManualFixed {
                actuator_permille: 500,
            },
        ),
        ("neural", Policy::Neural),
    ] {
        let mut write_error = None;
        let metrics = evaluate_policy_with_observer(policy, |sample| {
            if write_error.is_none()
                && let Err(error) = writeln!(
                    file,
                    "{name},{},{},{},{},{},{}",
                    sample.step,
                    sample.elapsed_ms,
                    sample.setpoint_milli_c,
                    sample.measured_milli_c,
                    sample.actuator_permille,
                    sample.error_milli_c,
                )
            {
                write_error = Some(error);
            }
        })
        .map_err(|error| error.to_string())?;
        if let Some(error) = write_error {
            return Err(format!("write {output}: {error}"));
        }
        print_scenario_metrics(name, metrics);
    }
    file.flush()
        .map_err(|error| format!("flush {output}: {error}"))?;
    Ok(())
}

fn run_rtos_sim(bind: &str, expected: u32, drop_every: u32) -> Result<(), String> {
    if expected == 0 {
        return Err("expected command count must be nonzero".to_owned());
    }
    let socket = UdpSocket::bind(bind).map_err(|error| format!("bind {bind}: {error}"))?;
    socket
        .set_read_timeout(Some(Duration::from_millis(200)))
        .map_err(|error| format!("set read timeout: {error}"))?;
    println!("IVC-RTOS-READY bind={bind} expected={expected} drop_every={drop_every}");

    let clock = Instant::now();
    let mut receive_window = ReceiveWindow::new();
    let mut endpoint = ControlEndpoint::new(EndpointConfig::default(), 45_000)
        .map_err(|error| error.to_string())?;
    let mut plant = ThermalPlant::new(20_000);
    let mut datagram = [0u8; HEADER_LEN + MAX_PAYLOAD_LEN];
    let mut accepted = 0u32;
    let mut protocol_errors = 0u64;
    let mut acknowledgements_dropped = 0u64;
    let mut status_sent = 0u64;
    let mut last_traffic = Instant::now();
    let mut saw_traffic = false;

    loop {
        let (length, peer) = match socket.recv_from(&mut datagram) {
            Ok(received) => received,
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                if saw_traffic
                    && accepted >= expected
                    && last_traffic.elapsed() > Duration::from_secs(1)
                {
                    break;
                }
                if !saw_traffic && last_traffic.elapsed() > Duration::from_secs(300) {
                    return Err("no controller traffic received within 300 seconds".to_owned());
                }
                endpoint
                    .check_timeout(elapsed_us(clock))
                    .map_err(|error| error.to_string())?;
                continue;
            }
            Err(error) => return Err(format!("receive datagram: {error}")),
        };
        saw_traffic = true;
        last_traffic = Instant::now();
        let frame = match decode_frame(&datagram[..length]) {
            Ok(frame) => frame,
            Err(error) => {
                protocol_errors += 1;
                eprintln!("IVC-RTOS-DROP reason={error}");
                continue;
            }
        };
        if frame.header.message_type != MessageType::Control {
            protocol_errors += 1;
            send_error(
                &socket,
                peer,
                frame.header,
                ErrorCode::InvalidControl,
                clock,
            )?;
            continue;
        }
        let command = match ControlCommand::decode(frame.payload) {
            Ok(command) => command,
            Err(error) => {
                protocol_errors += 1;
                eprintln!(
                    "IVC-RTOS-ERROR seq={} reason={error}",
                    frame.header.sequence
                );
                send_error(
                    &socket,
                    peer,
                    frame.header,
                    ErrorCode::InvalidControl,
                    clock,
                )?;
                continue;
            }
        };

        let delivery = receive_window
            .observe(frame.header.session_id, frame.header.sequence)
            .map_err(|error| error.to_string())?;
        match delivery {
            Delivery::NewSession {
                out_of_order: false,
            }
            | Delivery::New {
                out_of_order: false,
            } => {
                if matches!(delivery, Delivery::NewSession { .. }) {
                    endpoint.begin_session();
                }
                let received_us = elapsed_us(clock);
                endpoint
                    // Guest monotonic clocks do not share an epoch. Local receive
                    // time is used for the safety timer; sender time is echoed for
                    // same-side end-to-end measurement by the controller.
                    .apply(frame.header.sequence, command, received_us, received_us)
                    .map_err(|error| error.to_string())?;
                plant.step(endpoint.actuator_permille(), accepted);
                accepted += 1;
                println!(
                    "IVC-RTOS-APPLIED seq={} mode={:?} actuator_permille={} measured_milli_c={}",
                    frame.header.sequence,
                    command.mode,
                    endpoint.actuator_permille(),
                    plant.temperature_milli_c()
                );
            }
            Delivery::NewSession { out_of_order: true }
            | Delivery::New { out_of_order: true }
            | Delivery::OutsideWindow
            | Delivery::SessionRejected => {
                protocol_errors += 1;
                send_error(
                    &socket,
                    peer,
                    frame.header,
                    ErrorCode::SequenceOutsideWindow,
                    clock,
                )?;
                continue;
            }
            Delivery::Duplicate => {}
        }

        send_status(
            &socket,
            peer,
            frame.header,
            endpoint.status(plant.temperature_milli_c()),
            clock,
        )?;
        status_sent += 1;

        let drop_ack = matches!(delivery, Delivery::NewSession { .. } | Delivery::New { .. })
            && drop_every != 0
            && frame.header.sequence % drop_every == 0;
        if drop_ack {
            acknowledgements_dropped += 1;
            println!("IVC-RTOS-INJECT drop_ack_seq={}", frame.header.sequence);
        } else {
            send_ack(
                &socket,
                peer,
                frame.header,
                receive_window.acknowledgement(frame.header.sequence),
                clock,
            )?;
        }
    }

    let metrics = receive_window.metrics();
    println!(
        "IVC-RTOS-RESULT accepted={} duplicates={} reordered={} outside_window={} \
         session_resets={} session_rejections={} protocol_errors={} acks_dropped={} \
         status_sent={} safe_fallback={}",
        metrics.accepted,
        metrics.duplicates,
        metrics.reordered,
        metrics.outside_window,
        metrics.session_resets,
        metrics.session_rejections,
        protocol_errors,
        acknowledgements_dropped,
        status_sent,
        endpoint.status(plant.temperature_milli_c()).state
            == ivcproto::control::StatusState::SafeFallback
    );
    Ok(())
}

fn build_error_fault_probe(
    kind: ErrorFaultKind,
    session_id: u32,
    sequence: u32,
    timestamp_us: u64,
) -> Result<ErrorFaultProbe, String> {
    let command = ControlCommand {
        operation: ControlOperation::SetActuator,
        mode: ControlMode::Neural,
        actuator_permille: 0,
        setpoint_milli_c: 45_000,
        sample_id: sequence,
    };
    let control_payload = command.encode().map_err(|error| error.to_string())?;
    let offending_type = if kind == ErrorFaultKind::UnexpectedMessageType {
        MessageType::Status
    } else {
        MessageType::Control
    };
    let probe_session_id = if kind == ErrorFaultKind::InvalidSessionTransition {
        0
    } else {
        session_id
    };
    let header = Header::new(offending_type, probe_session_id, sequence, timestamp_us);
    let payload = if offending_type == MessageType::Control {
        control_payload.as_slice()
    } else {
        &[]
    };
    let mut buffer = [0u8; HEADER_LEN + MAX_PAYLOAD_LEN];
    let length = encode_frame(header, payload, &mut buffer).map_err(|error| error.to_string())?;
    let mut datagram = buffer[..length].to_vec();
    match kind {
        ErrorFaultKind::UnsupportedVersion => {
            datagram[VERSION_OFFSET] = VERSION.wrapping_add(1);
        }
        ErrorFaultKind::LengthMismatch => {
            let declared = u16::try_from(payload.len() + 1)
                .map_err(|_| "fault payload length exceeds u16".to_owned())?;
            datagram[PAYLOAD_LENGTH_OFFSET..PAYLOAD_LENGTH_OFFSET + 2]
                .copy_from_slice(&declared.to_le_bytes());
        }
        ErrorFaultKind::ChecksumMismatch => {
            let last = datagram
                .last_mut()
                .ok_or_else(|| "fault datagram is unexpectedly empty".to_owned())?;
            *last ^= 1;
        }
        ErrorFaultKind::UnexpectedMessageType | ErrorFaultKind::InvalidSessionTransition => {}
    }
    Ok(ErrorFaultProbe {
        kind,
        sequence,
        offending_type,
        expected_error: kind.expected_error(),
        datagram,
    })
}

fn receive_error_fault_response(
    socket: &UdpSocket,
    probe: &ErrorFaultProbe,
) -> Result<ErrorCode, String> {
    let started = Instant::now();
    loop {
        let mut response = [0u8; HEADER_LEN + MAX_PAYLOAD_LEN];
        let received = match socket.recv(&mut response) {
            Ok(received) => received,
            Err(error)
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
                    && started.elapsed() < ERROR_FAULT_RESPONSE_TIMEOUT =>
            {
                continue;
            }
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err(format!(
                    "timed out waiting for {} ERROR response",
                    probe.kind.name()
                ));
            }
            Err(error) => return Err(format!("receive fault response: {error}")),
        };
        let frame = decode_frame(&response[..received])
            .map_err(|error| format!("decode {} ERROR response: {error}", probe.kind.name()))?;
        if frame.header.message_type != MessageType::Error
            || frame.header.sequence != probe.sequence
        {
            return Err(format!(
                "unexpected response while waiting for {} ERROR",
                probe.kind.name()
            ));
        }
        let report = ErrorReport::decode(frame.payload).map_err(|error| error.to_string())?;
        if frame.header.error != probe.expected_error
            || report.offending_type != probe.offending_type
            || report.offending_sequence != probe.sequence
        {
            return Err(format!(
                "{} ERROR response does not match the injected frame",
                probe.kind.name()
            ));
        }
        return Ok(frame.header.error);
    }
}

fn run_error_fault_probes(
    socket: &UdpSocket,
    session_id: u32,
    clock: Instant,
) -> Result<u32, String> {
    let mut errors_received = 0u32;
    for (index, kind) in ErrorFaultKind::ALL.into_iter().enumerate() {
        let sequence = ERROR_FAULT_SEQUENCE_BASE + index as u32 + 1;
        let probe = build_error_fault_probe(kind, session_id, sequence, elapsed_us(clock))?;
        socket
            .send(&probe.datagram)
            .map_err(|error| format!("send {} fault probe: {error}", kind.name()))?;
        let observed_error = receive_error_fault_response(socket, &probe)?;
        errors_received += 1;
        for _ in 0..BOARD_CONSOLE_RECORD_COPIES {
            report_error_fault_record(kind, sequence, observed_error)?;
        }
    }
    Ok(errors_received)
}

fn report_error_fault_record(
    kind: ErrorFaultKind,
    sequence: u32,
    observed_error: ErrorCode,
) -> Result<(), String> {
    let body = format!(
        "kind={} seq={} expected={} observed={}",
        kind.name(),
        sequence,
        kind.expected_error() as u16,
        observed_error as u16
    );
    let record = checksummed_console_record("IVC-ERROR-C ", &body);
    debug_assert!(record.len() <= ERROR_EVIDENCE_RECORD_MAX_BYTES);
    println!("{record}");
    std::io::stdout()
        .flush()
        .map_err(|error| format!("flush fault evidence: {error}"))?;
    std::thread::sleep(BOARD_CONSOLE_RECORD_PAUSE);
    Ok(())
}

fn replay_verified_error_fault_records() -> Result<(), String> {
    for _ in 0..BOARD_CONSOLE_RECORD_COPIES {
        for (index, kind) in ErrorFaultKind::ALL.into_iter().enumerate() {
            let sequence = ERROR_FAULT_SEQUENCE_BASE + index as u32 + 1;
            report_error_fault_record(kind, sequence, kind.expected_error())?;
        }
    }
    Ok(())
}

fn report_restart_records(records: &[&str]) -> Result<(), String> {
    for _ in 0..RESTART_RESULT_RECORD_COPIES {
        for record in records {
            println!("{record}");
            std::io::stdout()
                .flush()
                .map_err(|error| format!("flush restart evidence: {error}"))?;
            std::thread::sleep(RESTART_RESULT_RECORD_PAUSE);
        }
    }
    Ok(())
}

fn build_restart_duplicate_datagram(
    session_id: u32,
    command: ControlCommand,
    timestamp_us: u64,
) -> Result<Vec<u8>, String> {
    let mut datagram = vec![0u8; HEADER_LEN + MAX_PAYLOAD_LEN];
    let mut header = Header::new(
        MessageType::Control,
        session_id,
        RESTART_DUPLICATE_SEQUENCE,
        timestamp_us,
    );
    header.flags = FrameFlags::ACK_REQUIRED.union(FrameFlags::RETRANSMISSION);
    let payload = command.encode().map_err(|error| error.to_string())?;
    let length =
        encode_frame(header, &payload, &mut datagram).map_err(|error| error.to_string())?;
    datagram.truncate(length);
    Ok(datagram)
}

fn run_restart_duplicate_probe(
    socket: &UdpSocket,
    session_id: u32,
    previous_session: u32,
    command: ControlCommand,
    clock: Instant,
) -> Result<RestartDuplicateEvidence, String> {
    let datagram = build_restart_duplicate_datagram(session_id, command, elapsed_us(clock))?;
    socket
        .send(&datagram)
        .map_err(|error| format!("send current-session duplicate probe: {error}"))?;

    let mut evidence = RestartDuplicateEvidence {
        sequence: RESTART_DUPLICATE_SEQUENCE,
        statuses_received: 0,
        acknowledgements_received: 0,
        stale_acknowledgements_ignored: 0,
        stale_statuses_ignored: 0,
    };
    let response_started = Instant::now();
    loop {
        let mut response = [0u8; HEADER_LEN + MAX_PAYLOAD_LEN];
        match socket.recv(&mut response) {
            Ok(received) => {
                let frame = decode_frame(&response[..received]).map_err(|error| {
                    format!("decode current-session duplicate response: {error}")
                })?;
                observe_restart_duplicate_response(
                    &mut evidence,
                    frame,
                    session_id,
                    previous_session,
                )?;
                if restart_duplicate_probe_is_complete(evidence) {
                    return Ok(evidence);
                }
            }
            Err(error)
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
                    && response_started.elapsed() < RESTART_PROBE_RESPONSE_TIMEOUT => {}
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err("timed out waiting for current-session duplicate responses".to_owned());
            }
            Err(error) => {
                return Err(format!(
                    "receive current-session duplicate response: {error}"
                ));
            }
        }
    }
}

fn observe_restart_duplicate_response(
    evidence: &mut RestartDuplicateEvidence,
    frame: Frame<'_>,
    session_id: u32,
    previous_session: u32,
) -> Result<(), String> {
    if frame.header.session_id == session_id && frame.header.sequence == RESTART_DUPLICATE_SEQUENCE
    {
        return observe_current_session_duplicate_response(evidence, frame);
    }
    if frame.header.session_id == previous_session
        && frame.header.sequence == RESTART_PREVIOUS_FINAL_SEQUENCE
    {
        return observe_retired_session_replay(evidence, frame);
    }
    Err(format!(
        "unexpected response to current-session duplicate probe: session={} sequence={} type={:?}",
        frame.header.session_id, frame.header.sequence, frame.header.message_type
    ))
}

fn observe_current_session_duplicate_response(
    evidence: &mut RestartDuplicateEvidence,
    frame: Frame<'_>,
) -> Result<(), String> {
    match frame.header.message_type {
        MessageType::Status => observe_restart_duplicate_status(evidence, frame.payload),
        MessageType::Ack => observe_restart_duplicate_ack(evidence, frame.payload),
        MessageType::Error => Err(format!(
            "RTOS rejected duplicate sequence {} with {:?}",
            RESTART_DUPLICATE_SEQUENCE, frame.header.error
        )),
        other => Err(format!(
            "unexpected {:?} response to current-session duplicate probe",
            other
        )),
    }
}

fn observe_restart_duplicate_status(
    evidence: &mut RestartDuplicateEvidence,
    payload: &[u8],
) -> Result<(), String> {
    let status = StatusReport::decode(payload).map_err(|error| error.to_string())?;
    if status.applied_sequence != RESTART_DUPLICATE_SEQUENCE {
        return Err(format!(
            "duplicate STATUS identifies sequence {}, expected {}",
            status.applied_sequence, RESTART_DUPLICATE_SEQUENCE
        ));
    }
    evidence.statuses_received = evidence.statuses_received.saturating_add(1);
    if evidence.statuses_received != 1 {
        return Err("duplicate probe received more than one STATUS".to_owned());
    }
    Ok(())
}

fn observe_restart_duplicate_ack(
    evidence: &mut RestartDuplicateEvidence,
    payload: &[u8],
) -> Result<(), String> {
    let ack = AckPayload::decode(payload).map_err(|error| error.to_string())?;
    if ack.acknowledged_sequence != RESTART_DUPLICATE_SEQUENCE
        || ack.next_expected_sequence != RESTART_DUPLICATE_SEQUENCE + 1
    {
        return Err(format!(
            "duplicate ACK identifies sequence {}/{}, expected {}/{}",
            ack.acknowledged_sequence,
            ack.next_expected_sequence,
            RESTART_DUPLICATE_SEQUENCE,
            RESTART_DUPLICATE_SEQUENCE + 1
        ));
    }
    evidence.acknowledgements_received = evidence.acknowledgements_received.saturating_add(1);
    if evidence.acknowledgements_received != 1 {
        return Err("duplicate probe received more than one ACK".to_owned());
    }
    Ok(())
}

fn observe_retired_session_replay(
    evidence: &mut RestartDuplicateEvidence,
    frame: Frame<'_>,
) -> Result<(), String> {
    match frame.header.message_type {
        MessageType::Ack => {
            let ack = AckPayload::decode(frame.payload).map_err(|error| error.to_string())?;
            if ack.acknowledged_sequence != RESTART_PREVIOUS_FINAL_SEQUENCE {
                return Err(format!(
                    "stale ACK identifies sequence {}, expected {}",
                    ack.acknowledged_sequence, RESTART_PREVIOUS_FINAL_SEQUENCE
                ));
            }
            evidence.stale_acknowledgements_ignored =
                evidence.stale_acknowledgements_ignored.saturating_add(1);
            Ok(())
        }
        MessageType::Status => {
            let status = StatusReport::decode(frame.payload).map_err(|error| error.to_string())?;
            if status.applied_sequence != RESTART_PREVIOUS_FINAL_SEQUENCE {
                return Err(format!(
                    "stale STATUS identifies sequence {}, expected {}",
                    status.applied_sequence, RESTART_PREVIOUS_FINAL_SEQUENCE
                ));
            }
            evidence.stale_statuses_ignored = evidence.stale_statuses_ignored.saturating_add(1);
            Ok(())
        }
        other => Err(format!(
            "unexpected {:?} response from retired session",
            other
        )),
    }
}

fn restart_duplicate_probe_is_complete(evidence: RestartDuplicateEvidence) -> bool {
    evidence.statuses_received == 1 && evidence.acknowledgements_received == 1
}

fn run_restart_stale_control_probe(
    socket: &UdpSocket,
    previous_session: u32,
    command: ControlCommand,
    clock: Instant,
) -> Result<(), String> {
    let mut datagram = [0u8; HEADER_LEN + MAX_PAYLOAD_LEN];
    let mut header = Header::new(
        MessageType::Control,
        previous_session,
        RESTART_STALE_CONTROL_SEQUENCE,
        elapsed_us(clock),
    );
    header.flags = FrameFlags::ACK_REQUIRED;
    let payload = command.encode().map_err(|error| error.to_string())?;
    let length =
        encode_frame(header, &payload, &mut datagram).map_err(|error| error.to_string())?;
    socket
        .send(&datagram[..length])
        .map_err(|error| format!("send retired-session control probe: {error}"))?;

    let response_started = Instant::now();
    loop {
        let mut response = [0u8; HEADER_LEN + MAX_PAYLOAD_LEN];
        match socket.recv(&mut response) {
            Ok(received) => {
                let frame = decode_frame(&response[..received])
                    .map_err(|error| format!("decode retired-session response: {error}"))?;
                if frame.header.session_id != previous_session
                    || frame.header.sequence != RESTART_STALE_CONTROL_SEQUENCE
                    || frame.header.message_type != MessageType::Error
                {
                    return Err(format!(
                        "unexpected response to retired-session probe: session={} sequence={} \
                         type={:?}",
                        frame.header.session_id, frame.header.sequence, frame.header.message_type
                    ));
                }
                if frame.header.error != ErrorCode::SequenceOutsideWindow {
                    return Err(format!(
                        "retired-session probe returned {:?}, expected {:?}",
                        frame.header.error,
                        ErrorCode::SequenceOutsideWindow
                    ));
                }
                let report =
                    ErrorReport::decode(frame.payload).map_err(|error| error.to_string())?;
                if report.offending_type != MessageType::Control
                    || report.offending_sequence != RESTART_STALE_CONTROL_SEQUENCE
                {
                    return Err(format!(
                        "retired-session ERROR payload identifies {:?}/{} instead of Control/{}",
                        report.offending_type,
                        report.offending_sequence,
                        RESTART_STALE_CONTROL_SEQUENCE
                    ));
                }
                return Ok(());
            }
            Err(error)
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
                    && response_started.elapsed() < RESTART_PROBE_RESPONSE_TIMEOUT => {}
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err("timed out waiting for retired-session rejection".to_owned());
            }
            Err(error) => return Err(format!("receive retired-session response: {error}")),
        }
    }
}

fn run_controller(arguments: ControllerArguments) -> Result<(), String> {
    let ControllerArguments {
        peer,
        count,
        policy,
        period_ms,
        session_id,
        backend,
        raw_csv,
        fault_profile,
        restart_previous_session,
        ack_timeout,
    } = arguments;
    if count == 0 {
        return Err("command count must be nonzero".to_owned());
    }
    let peer = SocketAddr::from_str(&peer).map_err(|error| format!("peer address: {error}"))?;
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|error| format!("bind: {error}"))?;
    socket
        .connect(peer)
        .map_err(|error| format!("connect {peer}: {error}"))?;
    socket
        .set_read_timeout(Some(SOCKET_TIMEOUT))
        .map_err(|error| format!("set read timeout: {error}"))?;
    let ack_timeout_us = u64::try_from(ack_timeout.as_micros())
        .map_err(|_| "ACK timeout is too large to represent in microseconds".to_owned())?;
    let reliability =
        ReliabilityConfig::new(ack_timeout_us, 20).map_err(|error| error.to_string())?;
    let session_id = session_id.unwrap_or_else(generate_session_id);
    if session_id == 0 {
        return Err("session-id must be nonzero".to_owned());
    }
    if let Some(previous_session) = restart_previous_session {
        if previous_session == 0 {
            return Err("restart previous session must be nonzero".to_owned());
        }
        if previous_session == session_id {
            return Err("restart previous and current sessions must differ".to_owned());
        }
    }
    let mut sender =
        StopAndWaitSender::new(session_id, reliability).map_err(|error| error.to_string())?;
    let clock = Instant::now();
    let fault_errors_received = match fault_profile {
        ControllerFaultProfile::None => 0,
        ControllerFaultProfile::Error => run_error_fault_probes(&socket, session_id, clock)?,
        ControllerFaultProfile::Restart => 0,
    };
    let run_start = Instant::now();
    let mut measured_milli_c = 20_000;
    let mut previous_measured_milli_c = measured_milli_c;
    let mut previous_actuator = 0u16;
    let mut full_loop_latency_us = Vec::with_capacity(count as usize);
    let mut pre_send_latency_us = Vec::with_capacity(count as usize);
    let mut transport_latency_us = Vec::with_capacity(count as usize);
    let mut controller_samples = raw_csv.as_ref().map(|_| Vec::with_capacity(count as usize));
    let mut protocol_errors = 0u64;
    let mut stale_acknowledgements_ignored = 0u64;
    let mut stale_statuses_ignored = 0u64;
    let mut stale_controls_rejected = 0u64;
    let mut restart_duplicate_evidence = None;
    let mut sum_squared_error = 0f64;
    let mut integrated_absolute_error = 0f64;
    let mut maximum_overshoot = 0i32;

    println!(
        "IVC-CONTROLLER-START peer={peer} count={count} policy={} period_ms={period_ms} \
         session_id={session_id} backend={} ack_timeout_ms={}",
        policy_name(policy),
        backend.name(),
        ack_timeout.as_millis()
    );
    for sample in 1..=count {
        let cycle_start = Instant::now();
        let cycle_started_at_us = elapsed_us(clock);
        let sequence = sender
            .begin(elapsed_us(clock))
            .map_err(|error| error.to_string())?;
        let setpoint = setpoint_for_sample(sample, count);
        let observed_milli_c = measured_milli_c;
        let observation = ThermalObservation {
            temperature_milli_c: observed_milli_c,
            setpoint_milli_c: setpoint,
            previous_actuator_permille: previous_actuator,
            temperature_rate_milli_c_per_s: (measured_milli_c - previous_measured_milli_c) * 10,
        };
        let command = match policy {
            Policy::ManualFixed { actuator_permille } => {
                ManualFixedController::new(actuator_permille)
                    .map_err(|error| error.to_string())?
                    .command(observation, sample)
            }
            Policy::Neural => NeuralController
                .command(observation, sample)
                .map_err(|error| error.to_string())?,
        };
        let sent_at_us = elapsed_us(clock);
        let mut datagram = [0u8; HEADER_LEN + MAX_PAYLOAD_LEN];
        let mut header = Header::new(MessageType::Control, session_id, sequence, sent_at_us);
        header.flags = FrameFlags::ACK_REQUIRED;
        let payload = command.encode().map_err(|error| error.to_string())?;
        let mut length =
            encode_frame(header, &payload, &mut datagram).map_err(|error| error.to_string())?;
        socket
            .send(&datagram[..length])
            .map_err(|error| format!("send command {sequence}: {error}"))?;

        let mut got_ack = false;
        let mut status = None;
        let (status, timeline, latency) = loop {
            let mut response = [0u8; HEADER_LEN + MAX_PAYLOAD_LEN];
            match socket.recv(&mut response) {
                Ok(received) => match decode_frame(&response[..received]) {
                    Ok(frame)
                        if frame.header.session_id == session_id
                            && frame.header.sequence == sequence =>
                    {
                        match frame.header.message_type {
                            MessageType::Ack => {
                                let ack = AckPayload::decode(frame.payload)
                                    .map_err(|error| error.to_string())?;
                                got_ack = ack.acknowledged_sequence == sequence;
                            }
                            MessageType::Status => {
                                let candidate = StatusReport::decode(frame.payload)
                                    .map_err(|error| error.to_string())?;
                                if candidate.applied_sequence == sequence {
                                    status = Some(candidate);
                                }
                            }
                            MessageType::Error => {
                                return Err(format!(
                                    "RTOS rejected sequence {sequence} with {:?}",
                                    frame.header.error
                                ));
                            }
                            _ => protocol_errors += 1,
                        }
                    }
                    Ok(frame)
                        if fault_profile == ControllerFaultProfile::Restart
                            && Some(frame.header.session_id) == restart_previous_session
                            && frame.header.sequence == RESTART_PREVIOUS_FINAL_SEQUENCE =>
                    {
                        match frame.header.message_type {
                            MessageType::Ack => {
                                let ack = AckPayload::decode(frame.payload)
                                    .map_err(|error| error.to_string())?;
                                if ack.acknowledged_sequence != RESTART_PREVIOUS_FINAL_SEQUENCE {
                                    return Err(format!(
                                        "stale ACK identifies sequence {}, expected {}",
                                        ack.acknowledged_sequence, RESTART_PREVIOUS_FINAL_SEQUENCE
                                    ));
                                }
                                stale_acknowledgements_ignored =
                                    stale_acknowledgements_ignored.saturating_add(1);
                            }
                            MessageType::Status => {
                                let stale_status = StatusReport::decode(frame.payload)
                                    .map_err(|error| error.to_string())?;
                                if stale_status.applied_sequence != RESTART_PREVIOUS_FINAL_SEQUENCE
                                {
                                    return Err(format!(
                                        "stale STATUS identifies sequence {}, expected {}",
                                        stale_status.applied_sequence,
                                        RESTART_PREVIOUS_FINAL_SEQUENCE
                                    ));
                                }
                                stale_statuses_ignored = stale_statuses_ignored.saturating_add(1);
                            }
                            _ => protocol_errors += 1,
                        }
                    }
                    Ok(_) => protocol_errors += 1,
                    Err(error) => {
                        protocol_errors += 1;
                        eprintln!("IVC-CONTROLLER-DROP reason={error}");
                    }
                },
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(error) => return Err(format!("receive response: {error}")),
            }

            if got_ack && status.is_some() {
                let now_us = elapsed_us(clock);
                if !matches!(
                    sender
                        .acknowledge(session_id, sequence, now_us)
                        .map_err(|error| error.to_string())?,
                    AckResult::Acknowledged { .. }
                ) {
                    return Err(format!(
                        "acknowledgement state mismatch for sequence {sequence}"
                    ));
                }
                let timeline = ControlCycleTimeline {
                    cycle_started_us: cycle_started_at_us,
                    command_sent_us: sent_at_us,
                    response_completed_us: now_us,
                };
                let latency = measure_control_cycle(timeline)?;
                full_loop_latency_us.push(latency.full_loop_us);
                pre_send_latency_us.push(latency.pre_send_us);
                transport_latency_us.push(latency.transport_us);
                let status = status.ok_or_else(|| {
                    format!("response loop completed without status for sequence {sequence}")
                })?;
                break (status, timeline, latency);
            }

            match sender
                .poll(elapsed_us(clock))
                .map_err(|error| error.to_string())?
            {
                RetryAction::Idle | RetryAction::Wait => {}
                RetryAction::Retransmit {
                    sequence: retry_sequence,
                } if retry_sequence == sequence => {
                    header.flags = FrameFlags::ACK_REQUIRED.union(FrameFlags::RETRANSMISSION);
                    length = encode_frame(header, &payload, &mut datagram)
                        .map_err(|error| error.to_string())?;
                    socket
                        .send(&datagram[..length])
                        .map_err(|error| format!("retransmit command {sequence}: {error}"))?;
                }
                RetryAction::Retransmit {
                    sequence: retry_sequence,
                } => {
                    return Err(format!(
                        "retry state sequence {retry_sequence} does not match {sequence}"
                    ));
                }
                RetryAction::TimedOut { .. } => {
                    return Err(format!("command {sequence} timed out"));
                }
            }
        };
        if sample == 1 && fault_profile == ControllerFaultProfile::Restart {
            let previous_session = restart_previous_session
                .ok_or_else(|| "restart profile is missing its previous session".to_owned())?;
            if sequence != RESTART_DUPLICATE_SEQUENCE {
                return Err(format!(
                    "restart duplicate sequence must be {}, got {sequence}",
                    RESTART_DUPLICATE_SEQUENCE
                ));
            }
            let evidence =
                run_restart_duplicate_probe(&socket, session_id, previous_session, command, clock)?;
            stale_acknowledgements_ignored = stale_acknowledgements_ignored
                .saturating_add(evidence.stale_acknowledgements_ignored);
            stale_statuses_ignored =
                stale_statuses_ignored.saturating_add(evidence.stale_statuses_ignored);
            restart_duplicate_evidence = Some(evidence);
            run_restart_stale_control_probe(&socket, previous_session, command, clock)?;
            stale_controls_rejected = stale_controls_rejected.saturating_add(1);
        }
        previous_measured_milli_c = measured_milli_c;
        measured_milli_c = status.measured_milli_c;
        previous_actuator = status.actuator_permille;
        let error_milli_c = setpoint
            .checked_sub(measured_milli_c)
            .ok_or_else(|| format!("temperature error overflow for sequence {sequence}"))?;
        let error = i64::from(error_milli_c);
        sum_squared_error += (error * error) as f64;
        integrated_absolute_error += error.unsigned_abs() as f64 * 0.1;
        maximum_overshoot = maximum_overshoot.max(measured_milli_c - setpoint);
        if let Some(samples) = &mut controller_samples {
            samples.push(ControllerSample {
                sequence,
                cycle_started_us: timeline.cycle_started_us,
                command_sent_us: timeline.command_sent_us,
                response_completed_us: timeline.response_completed_us,
                full_loop_us: latency.full_loop_us,
                pre_send_us: latency.pre_send_us,
                transport_us: latency.transport_us,
                setpoint_milli_c: setpoint,
                observed_milli_c,
                measured_milli_c,
                command_actuator_permille: command.actuator_permille,
                status_actuator_permille: status.actuator_permille,
                error_milli_c,
            });
        }
        if should_report_progress(sample, count) {
            println!(
                "IVC-CONTROLLER-STATUS seq={sequence} mode={:?} actuator_permille={} \
                 measured_milli_c={} setpoint_milli_c={setpoint}",
                status.active_mode, status.actuator_permille, status.measured_milli_c
            );
        }

        let period = Duration::from_millis(period_ms);
        if let Some(remaining) = period.checked_sub(cycle_start.elapsed()) {
            std::thread::sleep(remaining);
        }
    }

    let elapsed = run_start.elapsed();
    if let (Some(path), Some(samples)) = (&raw_csv, &controller_samples) {
        write_controller_samples(path, samples).map_err(|error| error.to_string())?;
        println!(
            "IVC-CONTROLLER-RAW path={} samples={}",
            path.display(),
            samples.len()
        );
    }
    full_loop_latency_us.sort_unstable();
    pre_send_latency_us.sort_unstable();
    transport_latency_us.sort_unstable();
    let metrics = sender.metrics();
    let summary = ControllerResultSummary {
        policy: policy_name(policy),
        sent: metrics.started,
        acknowledged: metrics.acknowledged,
        errors: protocol_errors,
        timeouts: metrics.timeouts,
        retransmissions: metrics.retransmissions,
        recoveries: metrics.retransmissions,
        success_percent: metrics.acknowledged as f64 / metrics.started as f64 * 100.0,
        full_loop: LatencySummary::from_sorted_samples(&full_loop_latency_us),
        pre_send: LatencySummary::from_sorted_samples(&pre_send_latency_us),
        transport: LatencySummary::from_sorted_samples(&transport_latency_us),
        throughput_msg_s: f64::from(count) / elapsed.as_secs_f64(),
        rmse_milli_c: (sum_squared_error / f64::from(count)).sqrt(),
        iae_milli_c_s: integrated_absolute_error,
        max_overshoot_milli_c: maximum_overshoot,
    };
    let compact_records = summary.compact_records();
    std::thread::sleep(BOARD_CONSOLE_SUMMARY_SETTLE);
    for _ in 0..BOARD_CONSOLE_RECORD_COPIES {
        for record in &compact_records {
            debug_assert!(record.len() <= BOARD_CONSOLE_RECORD_MAX_BYTES);
            println!("{record}");
            std::io::stdout()
                .flush()
                .map_err(|error| format!("flush compact controller result: {error}"))?;
            std::thread::sleep(BOARD_CONSOLE_RECORD_PAUSE);
        }
    }
    println!("{}", summary.legacy_record());
    if fault_profile == ControllerFaultProfile::Error {
        let fault_result_body = format!(
            "profile=error injected={} received={} acknowledged={} continued=1",
            ErrorFaultKind::ALL.len(),
            fault_errors_received,
            metrics.acknowledged
        );
        let fault_result = checksummed_console_record("IVC-ERROR-RESULT ", &fault_result_body);
        debug_assert!(fault_result.len() <= ERROR_EVIDENCE_RECORD_MAX_BYTES);
        // AxVisor reports the RTOS guest shutdown asynchronously on the same
        // physical UART. Keep the terminal recovery proof outside that burst.
        replay_verified_error_fault_records()?;
        std::thread::sleep(ERROR_FAULT_RESULT_SETTLE);
        for _ in 0..ERROR_FAULT_RESULT_RECORD_COPIES {
            println!("{fault_result}");
            std::io::stdout()
                .flush()
                .map_err(|error| format!("flush fault result: {error}"))?;
            std::thread::sleep(ERROR_FAULT_RESULT_RECORD_PAUSE);
        }
    }
    if fault_profile == ControllerFaultProfile::Restart {
        let duplicate = restart_duplicate_evidence
            .ok_or_else(|| "restart profile did not execute its duplicate probe".to_owned())?;
        if duplicate.sequence != RESTART_DUPLICATE_SEQUENCE
            || duplicate.statuses_received != 1
            || duplicate.acknowledgements_received != 1
        {
            return Err(format!(
                "restart duplicate evidence mismatch: sequence={} STATUS={} ACK={}",
                duplicate.sequence,
                duplicate.statuses_received,
                duplicate.acknowledgements_received
            ));
        }
        if stale_acknowledgements_ignored != 1
            || stale_statuses_ignored != 1
            || stale_controls_rejected != 1
        {
            return Err(format!(
                "restart evidence mismatch: stale ACKs ignored={}, stale STATUS ignored={}, stale \
                 controls rejected={}",
                stale_acknowledgements_ignored, stale_statuses_ignored, stale_controls_rejected
            ));
        }
        let previous_session = restart_previous_session
            .ok_or_else(|| "restart profile is missing its previous session".to_owned())?;
        let transport_body = format!(
            "old={} new={} ack_ignored={} status_ignored={} control_rejected={}",
            previous_session,
            session_id,
            stale_acknowledgements_ignored,
            stale_statuses_ignored,
            stale_controls_rejected
        );
        let result_body = format!(
            "profile=restart sent={} acknowledged={} continued=1",
            metrics.started, metrics.acknowledged
        );
        let duplicate_body = format!(
            "seq={} status={} ack={}",
            duplicate.sequence, duplicate.statuses_received, duplicate.acknowledgements_received
        );
        let duplicate_record = checksummed_console_record("IVC-RESTART-D ", &duplicate_body);
        let transport_record = checksummed_console_record("IVC-RESTART-C ", &transport_body);
        let result_record = checksummed_console_record("IVC-RESTART-RESULT ", &result_body);
        // The RTOS guest shuts down on the same physical UART. Wait for that
        // burst to drain, then pace every restart record independently.
        std::thread::sleep(RESTART_RESULT_SETTLE);
        report_restart_records(&[&duplicate_record, &transport_record, &result_record])?;
    }
    Ok(())
}

fn send_status(
    socket: &UdpSocket,
    peer: SocketAddr,
    request: Header,
    status: StatusReport,
    clock: Instant,
) -> Result<(), String> {
    let payload = status.encode().map_err(|error| error.to_string())?;
    send_frame(
        socket,
        peer,
        Header::new(
            MessageType::Status,
            request.session_id,
            request.sequence,
            elapsed_us(clock),
        ),
        &payload,
    )
}

fn send_ack(
    socket: &UdpSocket,
    peer: SocketAddr,
    request: Header,
    acknowledgement: AckPayload,
    clock: Instant,
) -> Result<(), String> {
    send_frame(
        socket,
        peer,
        Header::new(
            MessageType::Ack,
            request.session_id,
            request.sequence,
            elapsed_us(clock),
        ),
        &acknowledgement.encode(),
    )
}

fn send_error(
    socket: &UdpSocket,
    peer: SocketAddr,
    request: Header,
    error: ErrorCode,
    clock: Instant,
) -> Result<(), String> {
    let mut header = Header::new(
        MessageType::Error,
        request.session_id,
        request.sequence,
        elapsed_us(clock),
    );
    header.error = error;
    send_frame(
        socket,
        peer,
        header,
        &ErrorReport {
            offending_type: request.message_type,
            offending_sequence: request.sequence,
        }
        .encode(),
    )
}

fn send_frame(
    socket: &UdpSocket,
    peer: SocketAddr,
    header: Header,
    payload: &[u8],
) -> Result<(), String> {
    let mut datagram = [0u8; HEADER_LEN + MAX_PAYLOAD_LEN];
    let length = encode_frame(header, payload, &mut datagram).map_err(|error| error.to_string())?;
    socket
        .send_to(&datagram[..length], peer)
        .map_err(|error| format!("send response to {peer}: {error}"))?;
    Ok(())
}

fn print_scenario_metrics(name: &str, metrics: ScenarioMetrics) {
    let settling = metrics
        .final_settling_time_ms
        .map_or_else(|| "none".to_owned(), |value| value.to_string());
    println!(
        "policy={name},samples={},rmse_milli_c={:.3},iae_milli_c_s={:.3},max_overshoot_milli_c={},\
         final_settling_time_ms={settling}",
        metrics.samples,
        metrics.rmse_milli_c,
        metrics.iae_milli_c_s,
        metrics.maximum_overshoot_milli_c,
    );
}

fn policy_name(policy: Policy) -> &'static str {
    match policy {
        Policy::ManualFixed { .. } => "manual-fixed",
        Policy::Neural => "neural",
    }
}

fn parse_policy(value: &str) -> Result<Policy, String> {
    match value {
        "manual" | "manual-fixed" => Ok(Policy::ManualFixed {
            actuator_permille: 500,
        }),
        "neural" => Ok(Policy::Neural),
        _ => Err(format!(
            "policy must be 'manual' or 'neural', got '{value}'"
        )),
    }
}

fn parse_controller_arguments(
    arguments: impl Iterator<Item = String>,
) -> Result<ControllerArguments, String> {
    let mut arguments = arguments.peekable();
    let peer = required(&mut arguments, "peer address")?;
    let count = parse(&required(&mut arguments, "command count")?, "count")?;
    let policy = parse_policy(&required(&mut arguments, "policy")?)?;
    let mut period_ms = None;
    let mut session_id = None;
    let mut backend = InferenceBackend::Native;
    let mut backend_was_set = false;
    let mut raw_csv = None;
    let mut fault_profile = ControllerFaultProfile::None;
    let mut fault_profile_was_set = false;
    let mut restart_previous_session = None;
    let mut ack_timeout_ms: Option<u64> = None;

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--backend" => {
                if backend_was_set {
                    return Err("--backend may only be specified once".to_owned());
                }
                let value = required(&mut arguments, "controller backend")?;
                backend = match value.as_str() {
                    "native" => InferenceBackend::Native,
                    "onnxruntime" => {
                        return Err("controller backend 'onnxruntime' is not available in this \
                                    build"
                            .to_owned());
                    }
                    _ => {
                        return Err(format!(
                            "controller backend must be 'native' or 'onnxruntime', got '{value}'"
                        ));
                    }
                };
                backend_was_set = true;
            }
            "--raw-csv" => {
                if raw_csv.is_some() {
                    return Err("--raw-csv may only be specified once".to_owned());
                }
                raw_csv = Some(PathBuf::from(required(
                    &mut arguments,
                    "controller raw CSV path",
                )?));
            }
            "--fault-profile" => {
                if fault_profile_was_set {
                    return Err("--fault-profile may only be specified once".to_owned());
                }
                let value = required(&mut arguments, "controller fault profile")?;
                fault_profile = match value.as_str() {
                    "none" => ControllerFaultProfile::None,
                    "error" => ControllerFaultProfile::Error,
                    "restart" => ControllerFaultProfile::Restart,
                    _ => {
                        return Err(format!(
                            "controller fault profile must be 'none', 'error', or 'restart', got \
                             '{value}'"
                        ));
                    }
                };
                fault_profile_was_set = true;
            }
            "--restart-previous-session" => {
                if restart_previous_session.is_some() {
                    return Err("--restart-previous-session may only be specified once".to_owned());
                }
                restart_previous_session = Some(parse(
                    &required(&mut arguments, "restart previous session")?,
                    "restart previous session",
                )?);
            }
            "--ack-timeout-ms" => {
                if ack_timeout_ms.is_some() {
                    return Err("--ack-timeout-ms may only be specified once".to_owned());
                }
                ack_timeout_ms = Some(parse(
                    &required(&mut arguments, "ACK timeout milliseconds")?,
                    "ACK timeout milliseconds",
                )?);
            }
            _ if argument.starts_with('-') => {
                return Err(format!(
                    "unsupported controller option '{argument}'\n{}",
                    usage()
                ));
            }
            _ if period_ms.is_none() => {
                period_ms = Some(parse(&argument, "period-ms")?);
            }
            _ if session_id.is_none() => {
                session_id = Some(parse(&argument, "session-id")?);
            }
            _ => {
                return Err(format!(
                    "unexpected controller argument '{argument}'\n{}",
                    usage()
                ));
            }
        }
    }

    let ack_timeout = match ack_timeout_ms {
        Some(0) => return Err("ACK timeout milliseconds must be nonzero".to_owned()),
        Some(milliseconds) => Duration::from_millis(milliseconds),
        None => DEFAULT_ACK_TIMEOUT,
    };

    if fault_profile == ControllerFaultProfile::Restart {
        if session_id.is_none() {
            return Err("restart profile requires an explicit current session-id".to_owned());
        }
        if restart_previous_session.is_none() {
            return Err(
                "restart profile requires --restart-previous-session <session-id>".to_owned(),
            );
        }
    } else if restart_previous_session.is_some() {
        return Err("--restart-previous-session requires --fault-profile restart".to_owned());
    }

    Ok(ControllerArguments {
        peer,
        count,
        policy,
        period_ms: period_ms.unwrap_or(0),
        session_id,
        backend,
        raw_csv,
        fault_profile,
        restart_previous_session,
        ack_timeout,
    })
}

fn setpoint_for_sample(sample: u32, count: u32) -> i32 {
    let segment = ((sample - 1) * 3) / count;
    match segment {
        0 => 45_000,
        1 => 65_000,
        _ => 50_000,
    }
}

fn should_report_progress(sample: u32, count: u32) -> bool {
    sample == 1 || sample == count || sample.is_multiple_of(100)
}

fn percentile(sorted: &[u64], percentage: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let index = ((sorted.len() - 1) * percentage) / 100;
    sorted[index]
}

fn console_evidence_crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn checksummed_console_record(prefix: &str, body: &str) -> String {
    format!(
        "{prefix}{body} crc={:08x}",
        console_evidence_crc32(body.as_bytes())
    )
}

fn elapsed_us(clock: Instant) -> u64 {
    clock.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

fn generate_session_id() -> u32 {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let folded = timestamp ^ (timestamp >> 32) ^ u128::from(std::process::id());
    let candidate = folded as u32;
    if candidate == 0 { 1 } else { candidate }
}

fn required(arguments: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    arguments
        .next()
        .ok_or_else(|| format!("missing {name}\n{}", usage()))
}

fn parse<T>(value: &str, name: &str) -> Result<T, String>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error| format!("invalid {name} '{value}': {error}"))
}

fn optional_parse<T>(value: Option<String>, name: &str) -> Result<Option<T>, String>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    value.map(|value| parse(&value, name)).transpose()
}

fn improvement(baseline: f64, candidate: f64) -> f64 {
    (baseline - candidate) / baseline * 100.0
}

fn usage() -> &'static str {
    "usage:\n  ivcproto evaluate\n  ivcproto evaluate-csv <output.csv>\n  ivcproto rtos-sim <bind> \
     <expected-count> [drop-every]\n  ivcproto controller <peer> <count> <manual|neural> \
     [period-ms] [session-id] [--backend <native|onnxruntime>] [--raw-csv <path>] [--fault-profile \
     <none|error|restart>] [--restart-previous-session <session-id>] [--ack-timeout-ms \
     <milliseconds>]"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_loop_latency_includes_inference_before_send() {
        let latency = measure_control_cycle(ControlCycleTimeline {
            cycle_started_us: 100,
            command_sent_us: 140,
            response_completed_us: 210,
        })
        .expect("monotonic timeline should be valid");

        assert_eq!(latency.full_loop_us, 110);
        assert_eq!(latency.pre_send_us, 40);
        assert_eq!(latency.transport_us, 70);
    }

    #[test]
    fn generated_session_id_never_uses_the_reserved_value() {
        assert_ne!(generate_session_id(), 0);
    }

    #[test]
    fn console_evidence_crc32_matches_the_standard_check_value() {
        assert_eq!(console_evidence_crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn restart_duplicate_probe_replays_sequence_one_with_retransmission_flags() {
        let command = ControlCommand {
            operation: ControlOperation::SetActuator,
            mode: ControlMode::Neural,
            actuator_permille: 500,
            setpoint_milli_c: 45_000,
            sample_id: RESTART_DUPLICATE_SEQUENCE,
        };

        let datagram = build_restart_duplicate_datagram(572_662_306, command, 1234)
            .expect("restart duplicate probe should be encodable");
        let frame = decode_frame(&datagram).expect("restart duplicate probe should decode");

        assert_eq!(frame.header.message_type, MessageType::Control);
        assert_eq!(frame.header.session_id, 572_662_306);
        assert_eq!(frame.header.sequence, RESTART_DUPLICATE_SEQUENCE);
        assert_eq!(frame.header.timestamp_us, 1234);
        assert!(frame.header.flags.contains(FrameFlags::ACK_REQUIRED));
        assert!(frame.header.flags.contains(FrameFlags::RETRANSMISSION));
        assert_eq!(
            ControlCommand::decode(frame.payload).expect("probe payload should decode"),
            command
        );
    }

    #[test]
    fn checksummed_error_evidence_records_fit_the_uart_mux_chunk() {
        for kind in ErrorFaultKind::ALL {
            let body = format!(
                "kind={} seq=1005 expected={} observed={}",
                kind.name(),
                kind.expected_error() as u16,
                kind.expected_error() as u16
            );
            let record = checksummed_console_record("IVC-ERROR-C ", &body);
            assert!(record.len() <= ERROR_EVIDENCE_RECORD_MAX_BYTES, "{record}");
        }
        let terminal = checksummed_console_record(
            "IVC-ERROR-RESULT ",
            "profile=error injected=5 received=5 acknowledged=100 continued=1",
        );
        assert!(
            terminal.len() <= ERROR_EVIDENCE_RECORD_MAX_BYTES,
            "{terminal}"
        );
    }

    #[test]
    fn progress_reporting_keeps_boundaries_and_hundred_sample_checkpoints() {
        assert!(should_report_progress(1, 250));
        assert!(should_report_progress(100, 250));
        assert!(should_report_progress(250, 250));
        assert!(!should_report_progress(99, 250));
    }

    #[test]
    fn compact_controller_result_records_fit_physical_uart_budget() {
        let summary = ControllerResultSummary {
            policy: "neural",
            sent: 1_800,
            acknowledged: 1_800,
            errors: 0,
            timeouts: 0,
            retransmissions: 0,
            recoveries: 0,
            success_percent: 100.0,
            full_loop: LatencySummary {
                p50_us: 6_644,
                p95_us: 11_282,
                p99_us: 11_719,
                max_us: 20_115,
            },
            pre_send: LatencySummary {
                p50_us: 17,
                p95_us: 17,
                p99_us: 17,
                max_us: 365,
            },
            transport: LatencySummary {
                p50_us: 6_628,
                p95_us: 11_266,
                p99_us: 11_702,
                max_us: 20_098,
            },
            throughput_msg_s: 9.995,
            rmse_milli_c: 5_932.491,
            iae_milli_c_s: 686_993.400,
            max_overshoot_milli_c: 13_428,
        };

        let records = summary.compact_records();

        assert_eq!(records.len(), 6);
        assert!(
            records
                .iter()
                .all(|record| record.len() <= BOARD_CONSOLE_RECORD_MAX_BYTES),
            "compact records must remain atomic-sized for the shared physical UART: {records:?}"
        );
        assert!(records[0].starts_with("IVC-CONTROLLER-OUTCOME "));
        assert!(records[5].starts_with("IVC-CONTROLLER-CONTROL "));
        assert!(
            BOARD_CONSOLE_RECORD_COPIES >= 2,
            "physical UART evidence needs a redundant compact-record copy"
        );
        assert!(
            BOARD_CONSOLE_SUMMARY_SETTLE >= Duration::from_millis(100),
            "controller summary must wait for the RTOS to finish using the shared UART"
        );
        assert!(
            ERROR_FAULT_RESULT_SETTLE >= Duration::from_millis(500),
            "error terminal evidence must outlive asynchronous RTOS shutdown logs"
        );
        assert!(
            ERROR_FAULT_RESULT_RECORD_COPIES >= 3,
            "error terminal evidence needs three copies on the multiplexed UART"
        );
        assert!(
            ERROR_FAULT_RESULT_RECORD_PAUSE >= Duration::from_millis(15),
            "error terminal records need enough time to drain at 1.5 Mbaud"
        );
        assert!(
            RESTART_RESULT_SETTLE >= Duration::from_secs(2),
            "restart terminal evidence must outlive the RTOS shutdown burst"
        );
        assert!(
            RESTART_RESULT_RECORD_COPIES >= 3,
            "restart terminal evidence needs three copies on the multiplexed UART"
        );
        assert!(
            RESTART_RESULT_RECORD_PAUSE >= Duration::from_millis(100),
            "restart terminal records need independent UART drain windows"
        );
    }

    #[test]
    fn controller_arguments_accept_a_raw_csv_without_a_forced_session_id() {
        let arguments = [
            "10.0.0.2:5500",
            "20",
            "manual",
            "100",
            "--backend",
            "native",
            "--raw-csv",
            "/var/lib/ivc/raw.csv",
        ]
        .into_iter()
        .map(str::to_owned);

        let parsed = parse_controller_arguments(arguments)
            .expect("controller arguments should accept an explicit raw CSV path");

        assert_eq!(parsed.peer, "10.0.0.2:5500");
        assert_eq!(parsed.count, 20);
        assert_eq!(
            parsed.policy,
            Policy::ManualFixed {
                actuator_permille: 500
            }
        );
        assert_eq!(parsed.period_ms, 100);
        assert_eq!(parsed.session_id, None);
        assert_eq!(parsed.backend, InferenceBackend::Native);
        assert_eq!(
            parsed.raw_csv.as_deref(),
            Some(std::path::Path::new("/var/lib/ivc/raw.csv"))
        );
        assert_eq!(parsed.fault_profile, ControllerFaultProfile::None);
        assert_eq!(parsed.ack_timeout, DEFAULT_ACK_TIMEOUT);
    }

    #[test]
    fn controller_arguments_enable_the_error_evidence_profile_explicitly() {
        let arguments = [
            "10.0.0.2:5500",
            "100",
            "neural",
            "100",
            "--fault-profile",
            "error",
        ]
        .into_iter()
        .map(str::to_owned);

        let parsed = parse_controller_arguments(arguments)
            .expect("controller arguments should accept the error profile");

        assert_eq!(parsed.fault_profile, ControllerFaultProfile::Error);
    }

    #[test]
    fn controller_arguments_require_an_explicit_retired_session_for_restart() {
        let arguments = [
            "10.0.0.2:5500",
            "100",
            "neural",
            "100",
            "572662306",
            "--fault-profile",
            "restart",
            "--restart-previous-session",
            "286331153",
            "--ack-timeout-ms",
            "1000",
        ]
        .into_iter()
        .map(str::to_owned);

        let parsed = parse_controller_arguments(arguments)
            .expect("controller arguments should accept the restart profile");

        assert_eq!(parsed.fault_profile, ControllerFaultProfile::Restart);
        assert_eq!(parsed.session_id, Some(572_662_306));
        assert_eq!(parsed.restart_previous_session, Some(286_331_153));
        assert_eq!(parsed.ack_timeout, Duration::from_secs(1));
    }

    #[test]
    fn controller_arguments_reject_a_zero_ack_timeout() {
        let arguments = [
            "10.0.0.2:5500",
            "20",
            "manual",
            "100",
            "--ack-timeout-ms",
            "0",
        ]
        .into_iter()
        .map(str::to_owned);

        let error =
            parse_controller_arguments(arguments).expect_err("zero ACK timeout should be rejected");

        assert!(error.contains("must be nonzero"));
    }

    #[test]
    fn error_profile_builds_all_five_deterministic_fault_probes() {
        let cases = [
            (
                ErrorFaultKind::UnsupportedVersion,
                ErrorCode::UnsupportedVersion,
            ),
            (ErrorFaultKind::LengthMismatch, ErrorCode::MalformedFrame),
            (
                ErrorFaultKind::ChecksumMismatch,
                ErrorCode::ChecksumMismatch,
            ),
            (
                ErrorFaultKind::UnexpectedMessageType,
                ErrorCode::InvalidControl,
            ),
            (
                ErrorFaultKind::InvalidSessionTransition,
                ErrorCode::SequenceOutsideWindow,
            ),
        ];

        for (index, (kind, expected_error)) in cases.into_iter().enumerate() {
            let probe = build_error_fault_probe(kind, 0x4354_524c, index as u32 + 1, 1234)
                .expect("fault probe should be constructible");

            assert_eq!(probe.expected_error, expected_error);
            assert_eq!(probe.sequence, index as u32 + 1);
            match kind {
                ErrorFaultKind::UnsupportedVersion => assert!(matches!(
                    decode_frame(&probe.datagram),
                    Err(ivcproto::wire::ProtocolError::UnsupportedVersion(_))
                )),
                ErrorFaultKind::LengthMismatch => assert!(matches!(
                    decode_frame(&probe.datagram),
                    Err(ivcproto::wire::ProtocolError::LengthMismatch { .. })
                )),
                ErrorFaultKind::ChecksumMismatch => assert!(matches!(
                    decode_frame(&probe.datagram),
                    Err(ivcproto::wire::ProtocolError::ChecksumMismatch { .. })
                )),
                ErrorFaultKind::UnexpectedMessageType => {
                    assert_eq!(
                        decode_frame(&probe.datagram)
                            .expect("unexpected-type probe remains a valid frame")
                            .header
                            .message_type,
                        MessageType::Status
                    );
                }
                ErrorFaultKind::InvalidSessionTransition => {
                    assert_eq!(
                        decode_frame(&probe.datagram)
                            .expect("invalid-session probe remains a valid frame")
                            .header
                            .session_id,
                        0
                    );
                }
            }
        }
    }

    #[test]
    fn controller_raw_csv_retains_timing_and_control_values() {
        let temporary = std::env::temp_dir().join(format!(
            "ivcproto-controller-raw-{}-{}.csv",
            std::process::id(),
            generate_session_id()
        ));
        let sample = ControllerSample {
            sequence: 7,
            cycle_started_us: 100,
            command_sent_us: 140,
            response_completed_us: 210,
            full_loop_us: 110,
            pre_send_us: 40,
            transport_us: 70,
            setpoint_milli_c: 45_000,
            observed_milli_c: 20_000,
            measured_milli_c: 20_123,
            command_actuator_permille: 650,
            status_actuator_permille: 650,
            error_milli_c: 24_877,
        };

        write_controller_samples(&temporary, &[sample])
            .expect("one sample should be writable as CSV");
        let csv = std::fs::read_to_string(&temporary)
            .expect("controller CSV should be readable after writing");
        std::fs::remove_file(&temporary).expect("temporary controller CSV should be removable");

        assert!(
            csv.starts_with("sequence,cycle_started_us,command_sent_us,response_completed_us,")
        );
        assert!(csv.contains("7,100,140,210,110,40,70,45000,20000,20123,650,650,24877\n"));
    }
}
