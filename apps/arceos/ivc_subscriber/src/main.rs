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
    use arceos_ivshmem_pci::{EVENT_VECTOR_0, IvshmemDevice, IvshmemLayout};
    use ax_std::{
        format, println,
        string::{String, ToString},
        thread,
        time::{Duration, Instant},
    };
    use axivc::{IvshmemMessagePort, IvshmemSessionState};

    const ACK_BODY: &[u8] = b"ack from arceos subscriber";
    const IVSHMEM_PEER_ID: u32 = 1;
    const IVSHMEM_PEER_SLOT: u32 = 0;
    const SESSION_TIMEOUT: Duration = Duration::from_secs(30);
    const TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);
    const EVENT_POLL_DELAY: Duration = Duration::from_millis(1);

    pub fn run() {
        if let Err(error) = run_inner() {
            println!("ivc session failed backend=ivshmem-pci peer={IVSHMEM_PEER_ID}: {error}");
        }
    }

    fn run_inner() -> Result<(), String> {
        let device =
            IvshmemDevice::discover(IvshmemLayout::two_peer()).map_err(|e| e.to_string())?;
        if device.peer_id() != IVSHMEM_PEER_ID || device.max_peers() != 2 {
            return Err(format!(
                "unexpected ivshmem profile peer={} max_peers={}",
                device.peer_id(),
                device.max_peers()
            ));
        }
        println!(
            "ivc subscriber backend=ivshmem-pci peer={} max_peers={} event_mode=event-status-poll \
             msi_x=unavailable",
            device.peer_id(),
            device.max_peers()
        );

        let local_output = device.own_output().map_err(|e| e.to_string())?;
        let peer_output = device
            .output(IVSHMEM_PEER_SLOT)
            .map_err(|e| e.to_string())?;
        // SAFETY: the device owns both BAR2 mappings until this function
        // returns. Its page-aligned output sections are exposed only to this
        // protocol; the peer uses the reciprocal local/peer arrangement, and
        // no endpoint from an earlier session remains in this single run.
        let mut port = unsafe {
            IvshmemMessagePort::attach(
                local_output.as_ptr(),
                local_output.len(),
                peer_output.as_const_ptr(),
                peer_output.len(),
            )
        }
        .map_err(|e| e.to_string())?;
        // SAFETY: this app is the sole initializer of its output section for
        // this guest execution, and no endpoint has been created yet.
        let local_incarnation = unsafe { port.begin_session() }.map_err(|e| e.to_string())?;
        println!(
            "ivc session marker=waiting backend=ivshmem-pci role=subscriber peer={} \
             local_incarnation={local_incarnation} phase=after-publisher",
            device.peer_id()
        );

        wait_for_session(&device, &mut port, local_incarnation)?;
        let endpoints = port.try_connect().map_err(|e| e.to_string())?;
        let mut subscriber =
            MessageDemoSubscriber::new(endpoints, ACK_BODY).map_err(|e| e.to_string())?;
        let started = Instant::now();
        let mut event_observed = false;

        while !subscriber.is_complete() {
            let outcome = subscriber.poll(print_event).map_err(|e| e.to_string())?;
            if outcome.progressed() {
                ring_peer(&device)?;
            }
            event_observed |= poll_events(&device, event_observed)?;

            if !port.is_session_current() {
                return Err("ivshmem Message V1 session was lost".into());
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
            if !outcome.progressed() {
                thread::sleep(EVENT_POLL_DELAY);
            }
        }

        println!(
            "ivc session marker=complete backend=ivshmem-pci role=subscriber peer={}",
            device.peer_id()
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

    fn wait_for_session(
        device: &IvshmemDevice,
        port: &mut IvshmemMessagePort<'_>,
        local_incarnation: core::num::NonZeroU32,
    ) -> Result<(), String> {
        let started = Instant::now();
        let mut event_observed = false;
        loop {
            match port.poll_handshake().map_err(|e| e.to_string())? {
                IvshmemSessionState::Ready { local, peer } => {
                    if local != local_incarnation {
                        return Err(format!(
                            "session ready with unexpected local incarnation {local}"
                        ));
                    }
                    println!(
                        "ivc session marker=ready backend=ivshmem-pci role=subscriber peer={} \
                         local_incarnation={local} peer_incarnation={peer}",
                        device.peer_id()
                    );
                    return Ok(());
                }
                IvshmemSessionState::WaitingForPeer { .. }
                | IvshmemSessionState::WaitingForPeerAcknowledgement { .. } => {}
                state => return Err(format!("session left the waiting path: {state:?}")),
            }
            event_observed |= poll_events(device, event_observed)?;
            if started.elapsed() >= SESSION_TIMEOUT {
                return Err("peer did not join the ivshmem Message V1 session".into());
            }
            thread::sleep(EVENT_POLL_DELAY);
        }
    }

    fn poll_events(device: &IvshmemDevice, already_observed: bool) -> Result<bool, String> {
        if device
            .take_events(EVENT_VECTOR_0)
            .map_err(|e| e.to_string())?
            == 0
        {
            return Ok(false);
        }
        if !already_observed {
            println!("ivc session event backend=ivshmem-pci source=event-status");
        }
        Ok(true)
    }

    fn ring_peer(device: &IvshmemDevice) -> Result<(), String> {
        device
            .ring_doorbell(IVSHMEM_PEER_SLOT, 0)
            .map_err(|e| e.to_string())
    }
}
