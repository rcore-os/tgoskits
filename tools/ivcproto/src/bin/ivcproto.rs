use std::{
    fs::File,
    io::{ErrorKind, Write},
    net::{SocketAddr, UdpSocket},
    process::ExitCode,
    str::FromStr,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ivcproto::{
    control::{AckPayload, ControlCommand, ErrorReport, StatusReport},
    endpoint::{ControlEndpoint, EndpointConfig},
    neural::{
        ManualFixedController, NeuralController, Policy, ScenarioMetrics, ThermalObservation,
        ThermalPlant, evaluate_policy, evaluate_policy_with_observer,
    },
    reliability::{
        AckResult, Delivery, ReceiveWindow, ReliabilityConfig, RetryAction, StopAndWaitSender,
    },
    wire::{
        ErrorCode, FrameFlags, HEADER_LEN, Header, MAX_PAYLOAD_LEN, MessageType, decode_frame,
        encode_frame,
    },
};

const SOCKET_TIMEOUT: Duration = Duration::from_millis(100);

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
        Some("controller") => {
            let peer = required(&mut arguments, "peer address")?;
            let count = parse(&required(&mut arguments, "command count")?, "count")?;
            let policy = parse_policy(&required(&mut arguments, "policy")?)?;
            let period_ms = optional_parse(arguments.next(), "period-ms")?.unwrap_or(0);
            let session_id = optional_parse(arguments.next(), "session-id")?;
            run_controller(&peer, count, policy, period_ms, session_id)
        }
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

fn run_controller(
    peer: &str,
    count: u32,
    policy: Policy,
    period_ms: u64,
    session_id: Option<u32>,
) -> Result<(), String> {
    if count == 0 {
        return Err("command count must be nonzero".to_owned());
    }
    let peer = SocketAddr::from_str(peer).map_err(|error| format!("peer address: {error}"))?;
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|error| format!("bind: {error}"))?;
    socket
        .connect(peer)
        .map_err(|error| format!("connect {peer}: {error}"))?;
    socket
        .set_read_timeout(Some(SOCKET_TIMEOUT))
        .map_err(|error| format!("set read timeout: {error}"))?;
    let reliability = ReliabilityConfig::new(SOCKET_TIMEOUT.as_micros() as u64, 20)
        .map_err(|error| error.to_string())?;
    let session_id = session_id.unwrap_or_else(generate_session_id);
    let mut sender =
        StopAndWaitSender::new(session_id, reliability).map_err(|error| error.to_string())?;
    let clock = Instant::now();
    let run_start = Instant::now();
    let mut measured_milli_c = 20_000;
    let mut previous_measured_milli_c = measured_milli_c;
    let mut previous_actuator = 0u16;
    let mut full_loop_latency_us = Vec::with_capacity(count as usize);
    let mut pre_send_latency_us = Vec::with_capacity(count as usize);
    let mut transport_latency_us = Vec::with_capacity(count as usize);
    let mut protocol_errors = 0u64;
    let mut sum_squared_error = 0f64;
    let mut integrated_absolute_error = 0f64;
    let mut maximum_overshoot = 0i32;

    println!(
        "IVC-CONTROLLER-START peer={peer} count={count} policy={} period_ms={period_ms} \
         session_id={session_id}",
        policy_name(policy)
    );
    for sample in 1..=count {
        let cycle_start = Instant::now();
        let cycle_started_at_us = elapsed_us(clock);
        let sequence = sender
            .begin(elapsed_us(clock))
            .map_err(|error| error.to_string())?;
        let setpoint = setpoint_for_sample(sample, count);
        let observation = ThermalObservation {
            temperature_milli_c: measured_milli_c,
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
        loop {
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
                let latency = measure_control_cycle(ControlCycleTimeline {
                    cycle_started_us: cycle_started_at_us,
                    command_sent_us: sent_at_us,
                    response_completed_us: now_us,
                })?;
                full_loop_latency_us.push(latency.full_loop_us);
                pre_send_latency_us.push(latency.pre_send_us);
                transport_latency_us.push(latency.transport_us);
                break;
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
        }

        let status = status.ok_or_else(|| {
            format!("response loop completed without status for sequence {sequence}")
        })?;
        previous_measured_milli_c = measured_milli_c;
        measured_milli_c = status.measured_milli_c;
        previous_actuator = status.actuator_permille;
        let error = i64::from(setpoint) - i64::from(measured_milli_c);
        sum_squared_error += (error * error) as f64;
        integrated_absolute_error += error.unsigned_abs() as f64 * 0.1;
        maximum_overshoot = maximum_overshoot.max(measured_milli_c - setpoint);
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

    full_loop_latency_us.sort_unstable();
    pre_send_latency_us.sort_unstable();
    transport_latency_us.sort_unstable();
    let elapsed = run_start.elapsed();
    let metrics = sender.metrics();
    println!(
        "IVC-CONTROLLER-RESULT policy={} sent={} acknowledged={} errors={} timeouts={} \
         retransmissions={} recoveries={} success_percent={:.3} full_loop_p50_us={} \
         full_loop_p95_us={} full_loop_p99_us={} full_loop_max_us={} pre_send_p50_us={} \
         pre_send_p95_us={} pre_send_p99_us={} pre_send_max_us={} transport_p50_us={} \
         transport_p95_us={} transport_p99_us={} transport_max_us={} throughput_msg_s={:.3} \
         rmse_milli_c={:.3} iae_milli_c_s={:.3} max_overshoot_milli_c={}",
        policy_name(policy),
        metrics.started,
        metrics.acknowledged,
        protocol_errors,
        metrics.timeouts,
        metrics.retransmissions,
        metrics.retransmissions,
        metrics.acknowledged as f64 / metrics.started as f64 * 100.0,
        percentile(&full_loop_latency_us, 50),
        percentile(&full_loop_latency_us, 95),
        percentile(&full_loop_latency_us, 99),
        full_loop_latency_us.last().copied().unwrap_or(0),
        percentile(&pre_send_latency_us, 50),
        percentile(&pre_send_latency_us, 95),
        percentile(&pre_send_latency_us, 99),
        pre_send_latency_us.last().copied().unwrap_or(0),
        percentile(&transport_latency_us, 50),
        percentile(&transport_latency_us, 95),
        percentile(&transport_latency_us, 99),
        transport_latency_us.last().copied().unwrap_or(0),
        f64::from(count) / elapsed.as_secs_f64(),
        (sum_squared_error / f64::from(count)).sqrt(),
        integrated_absolute_error,
        maximum_overshoot,
    );
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
     [period-ms] [session-id]"
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
    fn progress_reporting_keeps_boundaries_and_hundred_sample_checkpoints() {
        assert!(should_report_progress(1, 250));
        assert!(should_report_progress(100, 250));
        assert!(should_report_progress(250, 250));
        assert!(!should_report_progress(99, 250));
    }
}
