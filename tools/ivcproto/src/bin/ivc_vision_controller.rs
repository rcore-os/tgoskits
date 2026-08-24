#[cfg(unix)]
use std::mem::MaybeUninit;
use std::{
    env,
    fs::File,
    io::{self, BufRead, BufReader, ErrorKind},
    net::{SocketAddr, UdpSocket},
    process::ExitCode,
    str::FromStr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
#[cfg(not(unix))]
use std::{sync::OnceLock, time::Instant};

use ivcproto::{
    control::AckPayload,
    vision::{ActuatorState, ActuatorStatus, VisionAction, VisionDecision},
    vision_records::parse_decision_record,
    wire::{
        FrameFlags, HEADER_LEN, Header, MAX_PAYLOAD_LEN, MessageType, decode_frame, encode_frame,
    },
};

const RESPONSE_POLL: Duration = Duration::from_millis(100);
const ACK_TIMEOUT_US: u64 = 1_000_000;
const MAX_RETRIES: u32 = 20;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("IVC-VISION-CONTROLLER-FAIL reason={error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut arguments = env::args().skip(1);
    let peer = arguments
        .next()
        .ok_or_else(usage)?
        .parse::<SocketAddr>()
        .map_err(|error| format!("invalid peer address: {error}"))?;
    let record_path = arguments.next().ok_or_else(usage)?;
    let session_id = match arguments.next() {
        Some(value) => {
            u32::from_str(&value).map_err(|error| format!("invalid session id: {error}"))?
        }
        None => generate_session_id(),
    };
    if arguments.next().is_some() {
        return Err(usage());
    }
    if session_id == 0 {
        return Err("session id must be nonzero".to_owned());
    }

    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|error| format!("bind UDP: {error}"))?;
    socket
        .connect(peer)
        .map_err(|error| format!("connect {peer}: {error}"))?;
    socket
        .set_read_timeout(Some(RESPONSE_POLL))
        .map_err(|error| format!("set response timeout: {error}"))?;

    if record_path == "-" {
        println!("VISION_CLOSED_LOOP_BEGIN peer={peer} frames=stream session_id={session_id}");
        let stdin = io::stdin();
        let applied = relay_decision_records(stdin.lock(), |sequence, decision| {
            send_decision(&socket, session_id, sequence, decision)
        })?;
        println!("VISION_CLOSED_LOOP_DONE frames={applied} applied={applied} errors=0");
        return Ok(());
    }

    let file = File::open(&record_path)
        .map_err(|error| format!("read decision records {record_path}: {error}"))?;
    let mut decisions = Vec::new();
    relay_decision_records(BufReader::new(file), |_sequence, decision| {
        decisions.push(decision);
        Ok(())
    })?;

    println!(
        "VISION_CLOSED_LOOP_BEGIN peer={peer} frames={} session_id={session_id}",
        decisions.len()
    );
    for (index, decision) in decisions.iter().copied().enumerate() {
        send_decision(&socket, session_id, (index + 1) as u32, decision)?;
    }
    println!(
        "VISION_CLOSED_LOOP_DONE frames={} applied={} errors=0",
        decisions.len(),
        decisions.len()
    );
    Ok(())
}

fn relay_decision_records<R, F>(reader: R, mut relay: F) -> Result<usize, String>
where
    R: BufRead,
    F: FnMut(u32, VisionDecision) -> Result<(), String>,
{
    let mut count = 0usize;
    let mut previous_frame_id = None;
    for (line_index, line) in reader.lines().enumerate() {
        let line_number = line_index + 1;
        let line = line.map_err(|error| format!("read record line {line_number}: {error}"))?;
        let Some(decision) = parse_decision_record(&line)
            .map_err(|error| format!("record line {line_number}: {error}"))?
        else {
            continue;
        };
        if let Some(previous) = previous_frame_id
            && previous >= decision.frame_id
        {
            return Err(format!(
                "frame ids must strictly increase, received {} after {}",
                decision.frame_id, previous
            ));
        }
        let next_count = count
            .checked_add(1)
            .ok_or_else(|| "decision record count overflow".to_owned())?;
        let sequence = u32::try_from(next_count)
            .map_err(|_| "decision record count exceeds protocol sequence range".to_owned())?;
        relay(sequence, decision)?;
        previous_frame_id = Some(decision.frame_id);
        count = next_count;
    }
    if count == 0 {
        return Err("decision record contains no VISION_DECISION_RECORD lines".to_owned());
    }
    Ok(count)
}

fn send_decision(
    socket: &UdpSocket,
    session_id: u32,
    sequence: u32,
    decision: VisionDecision,
) -> Result<(), String> {
    let sent_at_us = monotonic_us()?;
    let age_at_send_us = sent_at_us
        .checked_sub(decision.inference_finished_at_us)
        .ok_or_else(|| {
            format!(
                "frame {} inference timestamp is in the future",
                decision.frame_id
            )
        })?;
    if age_at_send_us > u64::from(decision.ttl_us) {
        return Err(format!(
            "frame {} expired before transmission: age_us={} ttl_us={}",
            decision.frame_id, age_at_send_us, decision.ttl_us
        ));
    }
    let payload = decision.encode().map_err(|error| error.to_string())?;
    let mut datagram = [0u8; HEADER_LEN + MAX_PAYLOAD_LEN];
    let mut response = [0u8; HEADER_LEN + MAX_PAYLOAD_LEN];
    let started_at_us = sent_at_us;
    let mut attempt_started_at_us = sent_at_us;
    let mut retries = 0u32;
    let mut got_ack = false;
    let mut status = None;

    loop {
        let mut header = Header::new(
            MessageType::VisionDecision,
            session_id,
            sequence,
            sent_at_us,
        );
        header.flags = if retries == 0 {
            FrameFlags::ACK_REQUIRED
        } else {
            FrameFlags::ACK_REQUIRED.union(FrameFlags::RETRANSMISSION)
        };
        let length = encode_frame(header, &payload, &mut datagram)
            .map_err(|error| format!("encode frame {sequence}: {error}"))?;
        socket
            .send(&datagram[..length])
            .map_err(|error| format!("send frame {sequence}: {error}"))?;

        loop {
            match socket.recv(&mut response) {
                Ok(received) => {
                    let frame = decode_frame(&response[..received])
                        .map_err(|error| format!("decode response {sequence}: {error}"))?;
                    if frame.header.session_id != session_id || frame.header.sequence != sequence {
                        continue;
                    }
                    match frame.header.message_type {
                        MessageType::Ack => {
                            let ack = AckPayload::decode(frame.payload)
                                .map_err(|error| error.to_string())?;
                            got_ack = ack.acknowledged_sequence == sequence;
                        }
                        MessageType::ActuatorStatus => {
                            let candidate = ActuatorStatus::decode(frame.payload)
                                .map_err(|error| error.to_string())?;
                            if candidate.applied_sequence == sequence
                                && candidate.frame_id == decision.frame_id
                            {
                                status = Some(candidate);
                            }
                        }
                        MessageType::Error => {
                            return Err(format!(
                                "RTOS rejected frame {} with {:?}",
                                decision.frame_id, frame.header.error
                            ));
                        }
                        _ => {}
                    }
                    if got_ack && status.is_some() {
                        break;
                    }
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(error) => return Err(format!("receive frame {sequence}: {error}")),
            }
            let now_us = monotonic_us()?;
            if now_us.saturating_sub(attempt_started_at_us) > ACK_TIMEOUT_US {
                break;
            }
        }
        if got_ack && status.is_some() {
            break;
        }
        if retries == MAX_RETRIES {
            return Err(format!("frame {} exhausted retries", decision.frame_id));
        }
        retries += 1;
        attempt_started_at_us = monotonic_us()?;
    }

    let completed_at_us = monotonic_us()?;
    let status = status.ok_or_else(|| {
        format!(
            "frame {} completed without actuator authorization status",
            decision.frame_id
        )
    })?;
    if status.state != ActuatorState::Applied
        || status.requested_action != decision.requested_action
        || status.actual_action != decision.requested_action
    {
        return Err(format!(
            "frame {} action mismatch: requested={} actual={} state={}",
            decision.frame_id,
            action_name(decision.requested_action),
            action_name(status.actual_action),
            state_name(status.state)
        ));
    }
    println!(
        concat!(
            "VISION_RTOS_AUTH_RECORD version=1 session_id={} sequence={} frame_id={} ",
            "requested_action={} authorized_action={} state={} retries={}"
        ),
        session_id,
        sequence,
        decision.frame_id,
        action_name(decision.requested_action),
        action_name(status.actual_action),
        state_name(status.state),
        retries
    );
    println!(
        concat!(
            "VISION_CLOSED_LOOP_EVENT session_id={} sequence={} frame={} detection={} class_id={} ",
            "confidence_q10000={} bbox={},{},{},{} requested={} actual={} state={} ",
            "inference_to_send_us={} transport_us={} end_to_end_us={} retries={}"
        ),
        session_id,
        sequence,
        decision.frame_id,
        u8::from(decision.detection_present),
        decision.class_id,
        decision.confidence_q10000,
        decision.bounding_box.left,
        decision.bounding_box.top,
        decision.bounding_box.right,
        decision.bounding_box.bottom,
        action_name(decision.requested_action),
        action_name(status.actual_action),
        state_name(status.state),
        age_at_send_us,
        completed_at_us.saturating_sub(started_at_us),
        completed_at_us.saturating_sub(decision.captured_at_us),
        retries
    );
    Ok(())
}

#[cfg(unix)]
fn monotonic_us() -> Result<u64, String> {
    let mut timestamp = MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: `timestamp` points to writable storage for one `timespec`; libc
    // initializes it before the value is read when `clock_gettime` succeeds.
    let result = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, timestamp.as_mut_ptr()) };
    if result != 0 {
        return Err(format!(
            "clock_gettime(CLOCK_MONOTONIC): {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: the successful call above initialized every field of `timespec`.
    let timestamp = unsafe { timestamp.assume_init() };
    let seconds = u64::try_from(timestamp.tv_sec)
        .map_err(|_| "monotonic clock returned negative seconds".to_owned())?;
    let nanoseconds = u64::try_from(timestamp.tv_nsec)
        .map_err(|_| "monotonic clock returned negative nanoseconds".to_owned())?;
    Ok(seconds.saturating_mul(1_000_000) + nanoseconds / 1_000)
}

#[cfg(not(unix))]
fn monotonic_us() -> Result<u64, String> {
    static CLOCK: OnceLock<Instant> = OnceLock::new();
    u64::try_from(CLOCK.get_or_init(Instant::now).elapsed().as_micros())
        .map_err(|_| "monotonic clock exceeded u64 microseconds".to_owned())
}

fn generate_session_id() -> u32 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |duration| duration.subsec_nanos());
    nanos.max(1)
}

const fn action_name(action: VisionAction) -> &'static str {
    match action {
        VisionAction::Hold => "hold",
        VisionAction::SortLeft => "left",
        VisionAction::SortRight => "right",
        VisionAction::EmergencyStop => "emergency-stop",
    }
}

const fn state_name(state: ActuatorState) -> &'static str {
    match state {
        ActuatorState::Applied => "applied",
        ActuatorState::SafeFallback => "safe-fallback",
        ActuatorState::Rejected => "rejected",
        ActuatorState::Fault => "fault",
    }
}

fn usage() -> String {
    "usage: ivc-vision-controller <peer-ip:port> <runner-log|-> [session-id]".to_owned()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::relay_decision_records;

    const RECORD_ONE: &str = concat!(
        "VISION_DECISION_RECORD version=1 frame_id=1 captured_at_us=1000 ",
        "inference_finished_at_us=1400 ttl_us=5000000 requested_action=right ",
        "safe_action=hold detection_present=1 class_id=32 confidence_q10000=7081 ",
        "region_id=2 left=479 top=706 right=773 bottom=1010\n"
    );

    #[test]
    fn relays_records_incrementally_and_ignores_human_output() {
        let input = format!("stream-rknn: started\n{RECORD_ONE}stream-rknn: progress\n");
        let mut relayed = Vec::new();
        let count = relay_decision_records(Cursor::new(input), |sequence, decision| {
            relayed.push((sequence, decision.frame_id));
            Ok(())
        })
        .unwrap();

        assert_eq!(count, 1);
        assert_eq!(relayed, vec![(1, 1)]);
    }

    #[test]
    fn preserves_prior_delivery_when_a_later_stream_record_is_invalid() {
        let input = format!("{RECORD_ONE}VISION_DECISION_RECORD version=1 frame_id=2\n");
        let mut relayed = Vec::new();
        let error = relay_decision_records(Cursor::new(input), |sequence, decision| {
            relayed.push((sequence, decision.frame_id));
            Ok(())
        })
        .unwrap_err();

        assert_eq!(relayed, vec![(1, 1)]);
        assert!(error.contains("record line 2"), "{error}");
    }

    #[test]
    fn rejects_non_increasing_frame_ids() {
        let repeated = RECORD_ONE.to_owned();
        let input = format!("{RECORD_ONE}{repeated}");
        let error =
            relay_decision_records(Cursor::new(input), |_sequence, _decision| Ok(())).unwrap_err();

        assert!(error.contains("strictly increase"), "{error}");
    }
}
