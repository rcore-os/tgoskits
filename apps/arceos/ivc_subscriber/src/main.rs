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
    use arceos_ivshmem_pci::{IvcDevice, IvcPeerRole};
    use ax_std::{
        format, println,
        string::{String, ToString},
        time::{Duration, Instant},
    };

    const ACK_BODY: &[u8] = b"ack from arceos subscriber";
    const SESSION_TIMEOUT: Duration = Duration::from_secs(30);
    const TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);

    pub fn run() {
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
        let started = Instant::now();

        while !subscriber.is_complete() {
            let outcome = subscriber.poll(print_event).map_err(stringify)?;
            if !outcome.progressed() {
                let observed = subscriber.wait_for_progress().map_err(stringify)?;
                if observed && !event_observed {
                    println!("ivc session event backend=ivshmem-pci source=event-status");
                }
                event_observed |= observed;
            }

            if outcome.is_complete() {
                break;
            }
            if started.elapsed() >= TRANSFER_TIMEOUT {
                return Err(format!(
                    "full-duplex exchange timed out sent_data={} acked={} received_requests={}",
                    subscriber.data_count(),
                    subscriber.ack_count(),
                    subscriber.request_count()
                ));
            }
        }

        while !subscriber.outbound_drained().map_err(stringify)? {
            if started.elapsed() >= TRANSFER_TIMEOUT {
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

    fn print_event(event: MessageDemoEvent<'_>) {
        match event {
            MessageDemoEvent::RequestReceived {
                sequence,
                len,
                body,
            } => {
                let text = core::str::from_utf8(body).unwrap_or("<non-utf8>");
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

    fn stringify(error: impl ToString) -> String {
        error.to_string()
    }
}
