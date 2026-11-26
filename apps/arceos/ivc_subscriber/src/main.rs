#![cfg_attr(feature = "arceos", no_main)]
#![cfg_attr(feature = "arceos", no_std)]

#[cfg(feature = "arceos")]
use ax_std as _;

#[cfg_attr(feature = "arceos", unsafe(no_mangle))]
#[cfg(feature = "arceos")]
fn main() {
    subscriber::run();
}

#[cfg(not(feature = "arceos"))]
fn main() {}

#[cfg(feature = "arceos")]
mod subscriber {
    use arceos_ivc_subscriber::{MessageDemoEvent, MessageDemoSubscriber};
    use arceos_ivshmem_pci::{
        IvcDevice, IvcError, IvcPeerRole, ivc_close, ivc_open, ivc_recv, ivc_send,
    };
    use ax_std::{
        format, println,
        string::{String, ToString},
        time::{Duration, Instant},
    };

    const ACK_BODY: &[u8] = b"ack from arceos subscriber";
    const LOG_BODY_LIMIT: usize = 256;
    const SESSION_TIMEOUT: Duration = Duration::from_secs(30);
    const TRANSFER_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
    const MESSAGE_SMOKE_MARKER: &str = "axvisor.ivc_case=arceos-message-smoke";
    const MESSAGE_SMOKE_TIMEOUT_MS: i32 = 30_000;
    const MESSAGE_SMOKE_REQUEST: [u8; 48] = smoke_payload(0x20);
    const MESSAGE_SMOKE_RESPONSE: [u8; 48] = smoke_payload(0x80);
    const MESSAGE_SMOKE_FINAL: [u8; 48] = smoke_payload(0xc0);

    pub fn run() {
        if message_smoke_enabled() {
            if let Err(error) = run_message_smoke() {
                println!("ivc message smoke failed backend=ivshmem-pci role=subscriber: {error}");
            }
            return;
        }

        if let Err(error) = run_inner() {
            println!(
                "ivc session failed backend=ivshmem-pci role={}: {error}",
                IvcPeerRole::Subscriber.as_str()
            );
        }
    }

    fn run_inner() -> Result<(), String> {
        let mut device = IvcDevice::open(IvcPeerRole::Subscriber).map_err(stringify)?;
        let device_info = device.info();
        println!(
            "ivc subscriber backend=ivshmem-pci peer={} max_peers={} event_mode=event-status-poll \
             msi_x=unavailable",
            device_info.peer_id(),
            device_info.max_peers()
        );

        let pending = device.open_endpoint().map_err(stringify)?;
        let pending_info = pending.info();
        println!(
            "ivc session marker=waiting backend=ivshmem-pci role=subscriber peer={} \
             local_incarnation={} phase=after-publisher",
            pending_info.device().peer_id(),
            pending_info.local_incarnation()
        );

        let endpoint = pending.wait_ready(SESSION_TIMEOUT).map_err(stringify)?;
        let ready = endpoint.info();
        println!(
            "ivc session marker=ready backend=ivshmem-pci role=subscriber peer={} \
             local_incarnation={} peer_incarnation={}",
            ready.device().peer_id(),
            ready.local_incarnation(),
            ready.peer_incarnation()
        );
        let mut event_observed = ready.observed_event();
        if event_observed {
            println!("ivc session event backend=ivshmem-pci source=event-status");
        }

        let mut subscriber = MessageDemoSubscriber::new(endpoint, ACK_BODY).map_err(stringify)?;
        let mut last_progress = Instant::now();

        while !subscriber.is_complete() {
            let outcome = subscriber.poll(print_event).map_err(stringify)?;
            if outcome.progressed() {
                last_progress = Instant::now();
            }
            if outcome.is_complete() {
                break;
            }
            if last_progress.elapsed() >= TRANSFER_IDLE_TIMEOUT {
                return Err(format!(
                    "full-duplex exchange timed out sent_data={} acked={} received_requests={}",
                    subscriber.data_count(),
                    subscriber.ack_count(),
                    subscriber.request_count()
                ));
            }
            if !outcome.progressed() {
                let observed = subscriber.wait_for_progress().map_err(stringify)?;
                if observed && !event_observed {
                    println!("ivc session event backend=ivshmem-pci source=event-status");
                }
                event_observed |= observed;
            }
        }

        while !subscriber.outbound_drained().map_err(stringify)? {
            if last_progress.elapsed() >= TRANSFER_IDLE_TIMEOUT {
                return Err(format!(
                    "ACK delivery timed out sent_data={} acked={} received_requests={}",
                    subscriber.data_count(),
                    subscriber.ack_count(),
                    subscriber.request_count()
                ));
            }
            let observed = subscriber.wait_for_progress().map_err(stringify)?;
            if observed && !event_observed {
                println!("ivc session event backend=ivshmem-pci source=event-status");
            }
            event_observed |= observed;
        }

        println!(
            "ivc session marker=complete backend=ivshmem-pci role=subscriber peer={}",
            ready.device().peer_id()
        );
        println!("ivc subscriber full-duplex demo complete backend=ivshmem-pci");
        Ok(())
    }

    fn run_message_smoke() -> Result<(), String> {
        let mut device = IvcDevice::discover().map_err(stringify)?;
        let info = device.info();
        println!(
            "ivc message smoke start backend=ivshmem-pci role={} peer={} max_peers={}",
            info.role().as_str(),
            info.peer_id(),
            info.max_peers()
        );

        let mut session = ivc_open(&mut device).map_err(stringify)?;
        if !matches!(
            ivc_send(&mut session, &[], 0),
            Err(IvcError::InvalidArgument { .. })
        ) {
            return Err("empty send did not return InvalidArgument".into());
        }
        let oversized = [0u8; 49];
        if !matches!(
            ivc_send(&mut session, &oversized, 0),
            Err(IvcError::MessageTooLarge { len: 49 })
        ) {
            return Err("oversized send did not return MessageTooLarge".into());
        }
        if !matches!(
            ivc_recv(&mut session, &mut [], 0),
            Err(IvcError::InvalidArgument { .. })
        ) {
            return Err("empty receive buffer did not return InvalidArgument".into());
        }
        if !matches!(
            ivc_recv(&mut session, &mut [0u8; 48], 60_001),
            Err(IvcError::InvalidArgument { .. })
        ) {
            return Err("out-of-range timeout did not return InvalidArgument".into());
        }

        let mut short = [0u8; 47];
        if !matches!(
            ivc_recv(&mut session, &mut short, MESSAGE_SMOKE_TIMEOUT_MS),
            Err(IvcError::BufferTooSmall {
                required: 48,
                available: 47
            })
        ) {
            return Err("short receive buffer did not preserve the 48-byte record".into());
        }
        let mut request = [0u8; 48];
        let request_len =
            ivc_recv(&mut session, &mut request, MESSAGE_SMOKE_TIMEOUT_MS).map_err(stringify)?;
        if request_len != request.len() || request != MESSAGE_SMOKE_REQUEST {
            return Err(format!(
                "request mismatch len={request_len} expected_len={}",
                MESSAGE_SMOKE_REQUEST.len()
            ));
        }
        ivc_send(
            &mut session,
            &MESSAGE_SMOKE_RESPONSE,
            MESSAGE_SMOKE_TIMEOUT_MS,
        )
        .map_err(stringify)?;

        let mut final_message = [0u8; 48];
        let final_len = ivc_recv(&mut session, &mut final_message, MESSAGE_SMOKE_TIMEOUT_MS)
            .map_err(stringify)?;
        if final_len != final_message.len() || final_message != MESSAGE_SMOKE_FINAL {
            return Err(format!(
                "final message mismatch len={final_len} expected_len={}",
                MESSAGE_SMOKE_FINAL.len()
            ));
        }
        ivc_close(session).map_err(stringify)?;

        println!(
            "ivc message smoke pass backend=ivshmem-pci role={} peer={} len={request_len} \
             path=48-byte",
            info.role().as_str(),
            info.peer_id()
        );
        Ok(())
    }

    fn print_event(event: MessageDemoEvent<'_>) {
        match event {
            MessageDemoEvent::RequestReceived {
                sequence,
                len,
                body,
            } => {
                let text = body_for_serial_log(body);
                println!("ivc recv request seq={sequence} len={len} msg={text}");
            }
            MessageDemoEvent::DataSent { sequence, len } => {
                println!("ivc send data seq={sequence} len={len}");
            }
            MessageDemoEvent::AckSent { sequence } => {
                println!("ivc ack pub seq={sequence}");
            }
        }
    }

    fn body_for_serial_log(body: &[u8]) -> &str {
        if body.len() > LOG_BODY_LIMIT {
            "<omitted>"
        } else {
            core::str::from_utf8(body).unwrap_or("<non-utf8>")
        }
    }

    const fn smoke_payload(seed: u8) -> [u8; 48] {
        let mut payload = [0u8; 48];
        let mut index = 0;
        while index < payload.len() {
            payload[index] = seed.wrapping_add((index as u8).wrapping_mul(3));
            index += 1;
        }
        payload
    }

    fn message_smoke_enabled() -> bool {
        ax_std::os::arceos::modules::ax_hal::boot::bootargs()
            .is_some_and(|bootargs| bootargs.contains(MESSAGE_SMOKE_MARKER))
    }

    fn stringify(error: impl ToString) -> String {
        error.to_string()
    }
}
