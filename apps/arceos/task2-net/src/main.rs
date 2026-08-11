//! Task-3 controller/managed UDP endpoint.
//!
//! The endpoint intentionally uses a nonblocking socket so retransmission and
//! heartbeat timers keep progressing while the peer is silent. Role and static
//! addresses are compile-time inputs supplied by the ArceOS build config; the
//! host build uses the same state machine for deterministic local testing.
//!
//! In Task-3 controller mode the endpoint runs a request-response control
//! loop: one reliable CONTROL is sent after the previous STATUS completes, so
//! the protocol's single-pending-frame constraint becomes the control rate
//! limiter.  A fixed target trajectory and a fixed P controller provide the
//! baseline; the AI mode (M3) replaces only the output computation.

use core::net::{Ipv4Addr, SocketAddr};
#[cfg(not(feature = "arceos"))]
use std::{
    net::UdpSocket,
    thread,
    time::{Duration, Instant},
};

#[cfg(feature = "arceos")]
use ax_std::{
    net::UdpSocket,
    thread,
    time::{Duration, Instant},
};
use task2_net_protocol::{
    ControlAction, ControlMessage, Endpoint, EndpointState, MAX_DATAGRAM_LEN, MessageKind,
    PollEvent, ReceiveEvent, RetryPolicy, SessionError, SessionId, StatusMessage, StatusState,
};

const LOCAL_PORT: u16 = match option_env!("TASK2_LOCAL_PORT") {
    None => 4242,
    Some(port) => {
        let mut value: u16 = 0;
        let mut index = 0;
        let bytes = port.as_bytes();
        while index < bytes.len() {
            if !bytes[index].is_ascii_digit() {
                panic!("TASK2_LOCAL_PORT must be a valid UDP port");
            }
            value = value * 10 + (bytes[index] - b'0') as u16;
            if value == 0 && index != 0 {
                panic!("TASK2_LOCAL_PORT must be a valid UDP port");
            }
            index += 1;
        }
        if value == 0 || index > 5 {
            panic!("TASK2_LOCAL_PORT must be a valid UDP port");
        }
        value
    }
};
const SESSION_ID: SessionId = SessionId::new(0x5452_5432);
// Device discovery and first ARP resolution happen after the endpoint starts.
// Keep the initial reliable exchange alive long enough for that real link
// setup; otherwise a healthy Guest pair can exhaust retries before the first
// packet reaches the peer.
const POLICY: RetryPolicy = match RetryPolicy::new(500, 5, 200, 5_000) {
    Ok(policy) => policy,
    Err(_) => panic!("task-2 protocol policy constants must be valid"),
};

const ROLE: &str = match option_env!("TASK2_ROLE") {
    Some(role) => role,
    None => "managed",
};
const LOCAL_IP: &str = match option_env!("TASK2_LOCAL_IP") {
    Some(ip) => ip,
    None => "10.0.42.2",
};
const PEER_IP: &str = match option_env!("TASK2_PEER_IP") {
    Some(ip) => ip,
    None => "10.0.42.1",
};
// Presence of this build-time variable enables the legacy raw UDP probe. It
// is intentionally absent from P2/P3 builds so those runs contain only T2N1
// frames; keeping the switch at compile time prevents a runtime test mode
// from accidentally changing the protocol evidence.
const SEND_P1_PROBE: bool = option_env!("TASK2_SEND_P1_PROBE").is_some();

// Presence of this build-time variable enables the Task-3 control loop in the
// controller role.  Task-2 default behaviour (single CONTROL then liveness)
// stays available so the Task-2 evidence remains reproducible.
const TASK3_CONTROL: bool = option_env!("TASK3_CONTROL_LOOP").is_some();

// Presence of this build-time variable selects the AI controller inside the
// Task-3 loop: the output is the frozen P term plus the model's learned
// loss/disturbance compensation (see task3-model).  Absent, the loop uses the
// pure P baseline with identical scenario and protocol behaviour.
const TASK3_AI: bool = option_env!("TASK3_AI").is_some();

/// Fixed Task-3 scenario parameters (frozen in M0, see
/// book/design/task3-ai-control-todo.md).  Values must not be tuned after
/// baseline/AI comparison runs start.
mod scenario {
    pub const STATE_MIN: i32 = 0;
    pub const STATE_MAX: i32 = 1000;
    pub const OUTPUT_MIN: i32 = 0;
    pub const OUTPUT_MAX: i32 = 1000;

    /// Baseline P controller: output = Kp * error + bias, clamped.
    pub const KP: i32 = 2;
    pub const KP_DEN: i32 = 1;
    pub const BIAS: i32 = 0;

    /// History window feeding the model input (M3).
    pub const HISTORY_LEN: usize = 64;

    /// Minimum interval between two Task-3 CONTROL frames (request-response
    /// rate limiter).  The MVP control period is 5-10 Hz, so the loop never
    /// sends a CONTROL faster than this even when the peer RTT is short.
    pub const MIN_CYCLE_MS: u64 = 100;
}

/// Trajectory of the fixed target: `[(start_ms, value), ...]`.
const TARGET_STEPS: [(u64, i32); 3] = [(0, 300), (5_000, 800), (15_000, 500)];

fn main() {
    if let Err(message) = run() {
        report_failure(message);
    }
}

fn report_failure(message: &'static str) -> ! {
    println!("TASK2_ERROR={message}");
    #[cfg(feature = "arceos")]
    {
        ax_std::process::exit(1);
    }
    #[cfg(not(feature = "arceos"))]
    {
        std::process::exit(1);
    }
}

fn run() -> Result<(), &'static str> {
    let local_ip = parse_ipv4(LOCAL_IP).ok_or("TASK2_LOCAL_IP is invalid")?;
    let peer_ip = parse_ipv4(PEER_IP).ok_or("TASK2_PEER_IP is invalid")?;
    configure_network(local_ip)?;

    let socket = UdpSocket::bind(SocketAddr::from((local_ip, LOCAL_PORT)))
        .map_err(|_| "failed to bind UDP socket")?;
    let peer = SocketAddr::from((peer_ip, LOCAL_PORT));
    let start = Instant::now();
    let mut endpoint = Endpoint::new(SESSION_ID, POLICY, 0);
    let mut inbound = [0; MAX_DATAGRAM_LEN];
    let mut response = [0; MAX_DATAGRAM_LEN];
    let mut outbound = [0; MAX_DATAGRAM_LEN];
    let mut control = Controller::new();

    println!("TASK2_READY role={ROLE} local={LOCAL_IP}:{LOCAL_PORT} peer={PEER_IP}:{LOCAL_PORT}");
    if ROLE == "controller" {
        if TASK3_CONTROL {
            control
                .send_next(&socket, &peer, &mut endpoint, &mut outbound, now_ms(&start))
                .map_err(|_| "failed to send first Task-3 control")?;
        } else {
            send_control(&socket, &peer, &mut endpoint, &mut outbound, now_ms(&start))?;
        }
        if SEND_P1_PROBE {
            let probe_len = socket
                .send_to(b"TASK2_P1_PROBE", peer)
                .map_err(|_| "failed to send P1 UDP probe")?;
            println!("TASK2_P1_PROBE_SENT bytes={probe_len}");
        }
        flush_network();
        #[cfg(feature = "arceos")]
        for stats in ax_net::net_dev_stats() {
            println!(
                "TASK2_NET_STATS name={} tx_packets={} tx_errors={} tx_dropped={} rx_packets={}",
                stats.name, stats.tx_packets, stats.tx_errors, stats.tx_dropped, stats.rx_packets
            );
        }
    }
    socket
        .set_nonblocking(true)
        .map_err(|_| "failed to enable nonblocking UDP mode")?;

    loop {
        let now = now_ms(&start);
        match socket.recv_from(&mut inbound) {
            Ok((length, source)) => {
                let state_before_receive = endpoint.state();
                let result = endpoint
                    .receive(&inbound[..length], now, &mut response)
                    .map_err(|_| "protocol receive failed")?;
                if result.response_len > 0 {
                    send_datagram(&socket, &source, &response[..result.response_len])
                        .map_err(|_| "failed to send protocol response")?;
                    flush_network();
                }
                if TASK3_CONTROL && ROLE == "controller" {
                    handle_receive_event_task3(
                        result.event,
                        &socket,
                        &peer,
                        &mut endpoint,
                        &mut outbound,
                        &mut control,
                        now,
                    )?;
                } else {
                    handle_receive_event(
                        result.event,
                        &socket,
                        &peer,
                        &mut endpoint,
                        &mut outbound,
                        now,
                    )?;
                }
                if state_before_receive == EndpointState::Safe
                    && endpoint.state() == EndpointState::Active
                {
                    println!("TASK2_RECOVERED state=Active elapsed_ms={now}");
                    if TASK3_CONTROL && ROLE == "controller" {
                        // A pure request-response loop would stall after link
                        // recovery: the peer only answers a CONTROL, but a new
                        // CONTROL is only sent on STATUS delivery.  Resend the
                        // next CONTROL right after the Safe->Active transition.
                        control.send_next_or_defer(
                            &socket,
                            &peer,
                            &mut endpoint,
                            &mut outbound,
                            now,
                        )?;
                    }
                }
            }
            Err(error) if is_would_block(&error) => {}
            Err(_) => return Err("UDP receive failed"),
        }

        let poll = endpoint
            .poll(now, &mut outbound)
            .map_err(|_| "protocol timer failed")?;
        if poll.datagram_len > 0 {
            send_datagram(&socket, &peer, &outbound[..poll.datagram_len])
                .map_err(|_| "failed to send protocol timer frame")?;
            flush_network();
        }
        if let PollEvent::Retransmit { sequence, attempt } = poll.event {
            println!(
                "TASK2_RETRANSMIT seq={} attempt={}",
                sequence.get(),
                attempt
            );
        }
        if matches!(
            poll.event,
            PollEvent::RetryExhausted { .. } | PollEvent::HeartbeatTimeout
        ) {
            println!(
                "TASK2_SAFE state={:?} event={:?}",
                endpoint.state(),
                poll.event
            );
            if TASK3_CONTROL && ROLE == "controller" {
                // Entering Safe means the outstanding request can never
                // complete: RetryExhausted has dropped the protocol pending
                // frame, and HeartbeatTimeout means no STATUS is coming for
                // the current request (a pending frame would have exhausted
                // its retries first).  Without clearing this marker the
                // TASK2_RECOVERED resend below returns early and the
                // request-response loop stays stalled after link recovery.
                control.request_in_flight = false;
            }
        }
        #[cfg(feature = "arceos")]
        ax_net::request_poll();
        thread::sleep(Duration::from_millis(10));
    }
}

/// Task-3 control-loop state on the controller side.
///
/// The loop is request-response: a new CONTROL is queued only after the
/// previous STATUS completes.  `request_in_flight` tracks whether a CONTROL is
/// currently awaiting its STATUS; history keeps the last state samples for the
/// model input (M3).
struct Controller {
    request_id: u32,
    request_in_flight: bool,
    request_sent_at_ms: u64,
    next_send_at_ms: u64,
    state_history: [i32; scenario::HISTORY_LEN],
    history_len: usize,
    output_history: [i32; scenario::HISTORY_LEN],
    output_len: usize,
    last_state: i32,
    prev_output: i32,
    sample_count: u32,
    pending_send: bool,
}

/// Why queueing the next Task-3 CONTROL did not complete immediately.
enum SendNextError {
    /// The previous CONTROL is still awaiting its ACK.  STATUS can be
    /// delivered before the ACK is processed under the host-side relay, so
    /// the caller defers and resumes on the Acknowledged event.
    PendingAck,
    /// Queueing or transmission failed permanently.
    Fatal(&'static str),
}

impl Controller {
    fn new() -> Self {
        Self {
            request_id: 0,
            request_in_flight: false,
            request_sent_at_ms: 0,
            next_send_at_ms: 0,
            state_history: [0; scenario::HISTORY_LEN],
            history_len: 0,
            output_history: [0; scenario::HISTORY_LEN],
            output_len: 0,
            last_state: 300,
            prev_output: 0,
            sample_count: 0,
            pending_send: false,
        }
    }

    fn target_for(&self, now_ms: u64) -> i32 {
        let mut target = TARGET_STEPS[0].1;
        for &(start_ms, value) in &TARGET_STEPS {
            if now_ms >= start_ms {
                target = value;
            }
        }
        target
    }

    fn push_state(&mut self, state: i32) {
        if self.history_len < scenario::HISTORY_LEN {
            self.state_history[self.history_len] = state;
            self.history_len += 1;
        } else {
            self.state_history.copy_within(1.., 0);
            self.state_history[scenario::HISTORY_LEN - 1] = state;
        }
    }

    fn push_output(&mut self, output: i32) {
        if self.output_len < scenario::HISTORY_LEN {
            self.output_history[self.output_len] = output;
            self.output_len += 1;
        } else {
            self.output_history.copy_within(1.., 0);
            self.output_history[scenario::HISTORY_LEN - 1] = output;
        }
    }

    /// Baseline P controller output for the current target and state.
    fn baseline_output(&self, target: i32, state: i32) -> i32 {
        let error = target - state;
        let output = scenario::BIAS + error * scenario::KP / scenario::KP_DEN;
        output.clamp(scenario::OUTPUT_MIN, scenario::OUTPUT_MAX)
    }

    /// AI controller output: the frozen P term plus the model's learned
    /// loss/disturbance compensation.  The P term keeps the loop stable where
    /// the model is inaccurate; the model adds the feedforward the P term
    /// cannot produce.  Feature windows are built by the shared contract
    /// (`task3_model::build_features`, mirrored from the training pipeline).
    fn ai_output(&mut self, target: i32) -> (i32, u64) {
        let features = task3_model::build_features(
            &self.state_history[..self.history_len],
            &self.output_history[..self.output_len],
            target,
            self.last_state,
            self.prev_output,
        );
        let infer_start = Instant::now();
        // The model emits the normalised correction (label scale /1000), the
        // same value the golden vectors pin against the torch reference.
        let correction = task3_model::forward(&features) * scenario::STATE_MAX as f64;
        let infer_us = infer_start.elapsed().as_micros() as u64;
        let output =
            (self.baseline_output(target, self.last_state) as f64 + correction).round() as i64;
        (
            output.clamp(scenario::OUTPUT_MIN as i64, scenario::OUTPUT_MAX as i64) as i32,
            infer_us,
        )
    }

    fn send_next(
        &mut self,
        socket: &UdpSocket,
        peer: &SocketAddr,
        endpoint: &mut Endpoint,
        outbound: &mut [u8; MAX_DATAGRAM_LEN],
        now_ms: u64,
    ) -> Result<(), SendNextError> {
        if self.request_in_flight {
            return Ok(());
        }
        if now_ms < self.next_send_at_ms {
            thread::sleep(Duration::from_millis(self.next_send_at_ms - now_ms));
        }
        self.request_id = self.request_id.wrapping_add(1);
        let target = self.target_for(now_ms);
        let output = if TASK3_AI {
            let (output, infer_us) = self.ai_output(target);
            println!(
                "TASK3_INFER elapsed_ms={now_ms} sample={} output={} infer_us={infer_us}",
                self.request_id, output
            );
            output
        } else {
            self.baseline_output(target, self.last_state)
        };
        self.push_output(output);

        let mut payload = [0; 12];
        let command = ControlMessage::new(ControlAction::SetOutput, output, self.request_id)
            .map_err(|_| SendNextError::Fatal("invalid Task-3 control command"))?;
        let payload_len = command
            .encode(&mut payload)
            .map_err(|_| SendNextError::Fatal("failed to encode Task-3 control"))?;
        let transmission = match endpoint.queue_reliable(
            MessageKind::Control,
            &payload[..payload_len],
            now_ms,
            outbound,
        ) {
            Ok(transmission) => transmission,
            Err(SessionError::ReliableFramePending) => return Err(SendNextError::PendingAck),
            Err(_) => return Err(SendNextError::Fatal("failed to queue Task-3 control")),
        };
        send_datagram(socket, peer, &outbound[..transmission.datagram_len()])
            .map_err(SendNextError::Fatal)?;
        flush_network();
        self.request_in_flight = true;
        self.request_sent_at_ms = now_ms;
        self.next_send_at_ms = now_ms + scenario::MIN_CYCLE_MS;
        self.pending_send = false;
        println!(
            "TASK3_CONTROL_SENT elapsed_ms={now_ms} request={} value={} target={} state={} seq={}",
            self.request_id,
            output,
            target,
            self.last_state,
            transmission.sequence().get()
        );
        Ok(())
    }

    /// Sends the next CONTROL, deferring while the previous ACK is in flight.
    ///
    /// STATUS can be delivered before the ACK of the CONTROL it answers, so
    /// `queue_reliable` may still see the previous frame pending.  That is a
    /// normal race under the host-side relay, not a failure: remember the
    /// request and resume it from the Acknowledged event.
    fn send_next_or_defer(
        &mut self,
        socket: &UdpSocket,
        peer: &SocketAddr,
        endpoint: &mut Endpoint,
        outbound: &mut [u8; MAX_DATAGRAM_LEN],
        now_ms: u64,
    ) -> Result<(), &'static str> {
        match self.send_next(socket, peer, endpoint, outbound, now_ms) {
            Ok(()) => Ok(()),
            Err(SendNextError::PendingAck) => {
                self.pending_send = true;
                println!("TASK2_CONTROL_DEFERRED awaiting previous ACK");
                Ok(())
            }
            Err(SendNextError::Fatal(message)) => Err(message),
        }
    }

    fn on_status(&mut self, status: &StatusMessage, now_ms: u64) -> Result<(), &'static str> {
        self.request_in_flight = false;
        self.sample_count = self.sample_count.wrapping_add(1);
        let state = status
            .value()
            .clamp(scenario::STATE_MIN, scenario::STATE_MAX);
        let rtt_ms = now_ms.saturating_sub(self.request_sent_at_ms);
        self.last_state = state;
        self.push_state(state);
        println!(
            "TASK3_STATUS_RECEIVED elapsed_ms={now_ms} request={} value={} state={} sample={} \
             rtt_ms={}",
            status.last_control_request(),
            status.value(),
            state,
            self.sample_count,
            rtt_ms
        );
        Ok(())
    }
}

fn handle_receive_event_task3(
    event: ReceiveEvent<'_>,
    socket: &UdpSocket,
    peer: &SocketAddr,
    endpoint: &mut Endpoint,
    outbound: &mut [u8; MAX_DATAGRAM_LEN],
    control: &mut Controller,
    now_ms: u64,
) -> Result<(), &'static str> {
    match event {
        ReceiveEvent::Delivered { frame } if frame.kind() == MessageKind::Status => {
            let status = StatusMessage::decode(frame.payload())
                .map_err(|_| "validated status payload failed to decode")?;
            control.on_status(&status, now_ms)?;
            control.send_next_or_defer(socket, peer, endpoint, outbound, now_ms)?;
        }
        ReceiveEvent::Delivered { frame } if frame.kind() == MessageKind::Control => {
            let command = ControlMessage::decode(frame.payload())
                .map_err(|_| "validated control payload failed to decode")?;
            println!(
                "TASK2_CONTROL_RECEIVED seq={} request={} action={:?} value={}",
                frame.sequence().get(),
                command.request_id(),
                command.action(),
                command.value()
            );
        }
        ReceiveEvent::Acknowledged { sequence } => {
            println!("TASK2_ACK seq={}", sequence.get());
            if control.pending_send {
                control.pending_send = false;
                control.send_next_or_defer(socket, peer, endpoint, outbound, now_ms)?;
            }
        }
        ReceiveEvent::DuplicateAcknowledgement { sequence } => {
            println!("TASK2_DUPLICATE_ACK seq={}", sequence.get());
        }
        ReceiveEvent::InvalidPayload { error } => {
            println!("TASK2_PROTOCOL_ERROR invalid_payload={error}");
        }
        ReceiveEvent::OutOfOrder { sequence, expected } => {
            println!(
                "TASK2_PROTOCOL_ERROR out_of_order={} expected={}",
                sequence.get(),
                expected.get()
            );
        }
        ReceiveEvent::RemoteError { code, sequence } => {
            println!(
                "TASK2_REMOTE_ERROR code={code:?} sequence={}",
                sequence.get()
            );
        }
        ReceiveEvent::Heartbeat { message } => {
            println!(
                "TASK2_HEARTBEAT_RECEIVED peer_uptime_ms={}",
                message.uptime_ms()
            );
        }
        ReceiveEvent::Duplicate { sequence } => {
            println!("TASK2_DUPLICATE seq={}", sequence.get());
        }
        ReceiveEvent::Rejected { error } => println!("TASK2_REJECTED error={error}"),
        ReceiveEvent::SessionMismatch => println!("TASK2_REJECTED session_mismatch"),
        ReceiveEvent::Delivered { .. } => {}
    }
    Ok(())
}

fn send_control(
    socket: &UdpSocket,
    peer: &SocketAddr,
    endpoint: &mut Endpoint,
    outbound: &mut [u8; MAX_DATAGRAM_LEN],
    now_ms: u64,
) -> Result<(), &'static str> {
    let mut payload = [0; 12];
    let command = ControlMessage::new(ControlAction::SetOutput, 100, 1)
        .map_err(|_| "invalid built-in control command")?;
    let payload_len = command
        .encode(&mut payload)
        .map_err(|_| "failed to encode control command")?;
    let transmission = endpoint
        .queue_reliable(
            MessageKind::Control,
            &payload[..payload_len],
            now_ms,
            outbound,
        )
        .map_err(|_| "failed to queue control command")?;
    send_datagram(socket, peer, &outbound[..transmission.datagram_len()])
        .map_err(|_| "failed to send control command")?;
    flush_network();
    println!(
        "TASK2_CONTROL_SENT seq={} request=1",
        transmission.sequence().get()
    );
    Ok(())
}

fn handle_receive_event(
    event: ReceiveEvent<'_>,
    socket: &UdpSocket,
    peer: &SocketAddr,
    endpoint: &mut Endpoint,
    outbound: &mut [u8; MAX_DATAGRAM_LEN],
    now_ms: u64,
) -> Result<(), &'static str> {
    match event {
        ReceiveEvent::Delivered { frame } if frame.kind() == MessageKind::Control => {
            let command = ControlMessage::decode(frame.payload())
                .map_err(|_| "validated control payload failed to decode")?;
            println!(
                "TASK2_CONTROL_RECEIVED seq={} request={} action={:?} value={}",
                frame.sequence().get(),
                command.request_id(),
                command.action(),
                command.value()
            );
            let mut status_payload = [0; 12];
            let status = StatusMessage::new(
                if command.action() == ControlAction::Stop {
                    StatusState::Stopped
                } else {
                    StatusState::Active
                },
                0,
                command.value(),
                command.request_id(),
            )
            .map_err(|_| "invalid status state")?;
            let status_len = status
                .encode(&mut status_payload)
                .map_err(|_| "failed to encode status")?;
            let transmission = endpoint
                .queue_reliable(
                    MessageKind::Status,
                    &status_payload[..status_len],
                    now_ms,
                    outbound,
                )
                .map_err(|_| "failed to queue status")?;
            send_datagram(socket, peer, &outbound[..transmission.datagram_len()])
                .map_err(|_| "failed to send status")?;
            flush_network();
            println!("TASK2_STATUS_SENT seq={}", transmission.sequence().get());
        }
        ReceiveEvent::Delivered { frame } if frame.kind() == MessageKind::Status => {
            let status = StatusMessage::decode(frame.payload())
                .map_err(|_| "validated status payload failed to decode")?;
            println!(
                "TASK2_STATUS_RECEIVED seq={} state={:?} request={}",
                frame.sequence().get(),
                status.state(),
                status.last_control_request()
            );
        }
        ReceiveEvent::Acknowledged { sequence } => {
            println!("TASK2_ACK seq={}", sequence.get());
        }
        ReceiveEvent::DuplicateAcknowledgement { sequence } => {
            println!("TASK2_DUPLICATE_ACK seq={}", sequence.get());
        }
        ReceiveEvent::InvalidPayload { error } => {
            println!("TASK2_PROTOCOL_ERROR invalid_payload={error}");
        }
        ReceiveEvent::OutOfOrder { sequence, expected } => {
            println!(
                "TASK2_PROTOCOL_ERROR out_of_order={} expected={}",
                sequence.get(),
                expected.get()
            );
        }
        ReceiveEvent::RemoteError { code, sequence } => {
            println!(
                "TASK2_REMOTE_ERROR code={code:?} sequence={}",
                sequence.get()
            );
        }
        ReceiveEvent::Heartbeat { message } => {
            println!(
                "TASK2_HEARTBEAT_RECEIVED peer_uptime_ms={}",
                message.uptime_ms()
            );
        }
        ReceiveEvent::Duplicate { sequence } => {
            println!("TASK2_DUPLICATE seq={}", sequence.get());
        }
        ReceiveEvent::Rejected { error } => println!("TASK2_REJECTED error={error}"),
        ReceiveEvent::SessionMismatch => println!("TASK2_REJECTED session_mismatch"),
        ReceiveEvent::Delivered { .. } => {}
    }
    Ok(())
}

#[inline]
fn flush_network() {
    #[cfg(feature = "arceos")]
    {
        ax_net::flush_egress();
        thread::yield_now();
    }
}

/// Sends one datagram, tolerating transient WouldBlock backpressure.
///
/// The socket is nonblocking; under TCG emulation with a host-side relay the
/// TX path can intermittently report WouldBlock.  Retry briefly instead of
/// treating a transient backpressure signal as a fatal error.
fn send_datagram(
    socket: &UdpSocket,
    destination: &SocketAddr,
    datagram: &[u8],
) -> Result<(), &'static str> {
    let started = Instant::now();
    loop {
        match socket.send_to(datagram, destination) {
            Ok(_) => return Ok(()),
            Err(error)
                if is_would_block(&error) && started.elapsed() < Duration::from_millis(500) =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                println!("TASK2_SEND_ERROR kind=datagram error={error:?}");
                return Err("UDP send failed");
            }
        }
    }
}

#[cfg(feature = "arceos")]
fn configure_network(local_ip: Ipv4Addr) -> Result<(), &'static str> {
    if option_env!("TASK2_USE_DHCP").is_some() {
        println!("TASK2_NET_DHCP mode=dhcp requested_ip={local_ip}");
        return Ok(());
    }
    let interface = ax_net::interfaces()
        .into_iter()
        .find(|interface| matches!(interface.kind, ax_net::InterfaceKind::Ethernet))
        .ok_or("no Ethernet interface discovered")?;
    if let Some(current) = ax_net::ipv4_config(&interface.name) {
        if current.address.address().octets() == local_ip.octets()
            && current.address.prefix_len() == 24
        {
            println!(
                "TASK2_NET_CONFIGURED interface={} ip={local_ip}/24",
                interface.name
            );
            return Ok(());
        }
        ax_net::remove_interface_ipv4(
            interface.id,
            current.address.address(),
            current.address.prefix_len(),
        )
        .map_err(|_| "failed to remove dynamic IPv4")?;
    }
    ax_net::set_interface_ipv4(interface.id, local_ip, 24)
        .map_err(|_| "failed to configure static IPv4")?;
    println!(
        "TASK2_NET_CONFIGURED interface={} ip={local_ip}/24",
        interface.name
    );
    Ok(())
}

#[cfg(not(feature = "arceos"))]
fn configure_network(_local_ip: Ipv4Addr) -> Result<(), &'static str> {
    Ok(())
}

fn now_ms(start: &Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

fn parse_ipv4(value: &str) -> Option<Ipv4Addr> {
    let mut octets = [0; 4];
    let mut index = 0;
    let mut current = 0u16;
    let mut has_digit = false;
    for byte in value.bytes() {
        match byte {
            b'0'..=b'9' => {
                has_digit = true;
                current = current
                    .checked_mul(10)?
                    .checked_add(u16::from(byte - b'0'))?;
                if current > 255 {
                    return None;
                }
            }
            b'.' if has_digit && index < 3 => {
                octets[index] = current as u8;
                index += 1;
                current = 0;
                has_digit = false;
            }
            _ => return None,
        }
    }
    if !has_digit || index != 3 || current > 255 {
        return None;
    }
    octets[3] = current as u8;
    Some(Ipv4Addr::from(octets))
}

#[cfg(feature = "arceos")]
fn is_would_block(error: &ax_errno::AxError) -> bool {
    *error == ax_errno::AxError::WouldBlock
}

#[cfg(not(feature = "arceos"))]
fn is_would_block(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
}
