mod ivshmem;

use std::{
    process::ExitCode,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use arceos_ivc_subscriber::{MessageDemoEvent, MessageDemoSubscriber};
use axivc::IvshmemSessionState;

use crate::ivshmem::{ExclusiveDevice, MessageSession, WaitResult};

const ACK_BODY: &[u8] = b"ack from linux subscriber";
const SESSION_TIMEOUT: Duration = Duration::from_secs(30);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);
const BACKPRESSURE_TIMEOUT: Duration = Duration::from_secs(15);
const EVENT_WAIT_MS: i32 = 25;

fn main() -> ExitCode {
    match run_demo() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("linux ivc demo failed: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_demo() -> Result<()> {
    let mut device = ExclusiveDevice::open()?;
    let peer_id = device.peer_id();
    let max_peers = device.max_peers();
    println!(
        "linux ivc profile backend=ivshmem-pci peer={peer_id} max_peers={max_peers} bar2=0x10000"
    );

    let mut session = device.take_session()?;

    let local_incarnation = session.begin_session()?;
    session.ring_peer()?;
    println!(
        "linux ivc session marker=waiting backend=ivshmem-pci role=subscriber peer={} \
         local_incarnation={local_incarnation}",
        peer_id
    );

    let mut observed_events = 0;
    wait_for_session(&mut session, &mut observed_events)?;
    let endpoints = session.try_connect()?;
    let mut subscriber =
        MessageDemoSubscriber::new(endpoints, ACK_BODY).context("failed to create subscriber")?;

    let started = Instant::now();
    let mut last_progress = Instant::now();
    while !subscriber.is_complete() {
        let outcome = subscriber
            .poll(print_event)
            .context("Message V1 subscriber poll failed")?;
        if outcome.progressed() {
            last_progress = Instant::now();
            session.ring_peer()?;
        }

        let state = session.session_state()?;
        if !matches!(state, IvshmemSessionState::Ready { .. }) {
            bail!("ivshmem Message V1 session was lost: {state:?}");
        }
        if outcome.is_complete() || subscriber.is_complete() {
            break;
        }
        if started.elapsed() >= TRANSFER_TIMEOUT {
            let state = session.session_state()?;
            bail!(
                "transfer timeout: requests={} data={} acks={} state={state:?}",
                subscriber.request_count(),
                subscriber.data_count(),
                subscriber.ack_count()
            );
        }
        if last_progress.elapsed() >= BACKPRESSURE_TIMEOUT {
            let state = session.session_state()?;
            bail!(
                "state/backpressure timeout: requests={} data={} acks={} state={state:?}",
                subscriber.request_count(),
                subscriber.data_count(),
                subscriber.ack_count()
            );
        }

        observe_event(&session, &mut observed_events)?;
    }

    if observed_events == 0 {
        bail!("no MSI-X/UIO interrupt was observed");
    }
    let state = session.session_state()?;
    if !matches!(state, IvshmemSessionState::Ready { .. }) {
        bail!("ivshmem Message V1 session closed before completion: {state:?}");
    }

    println!(
        "linux ivc session marker=complete backend=ivshmem-pci role=subscriber peer={} \
         local_incarnation={local_incarnation}",
        peer_id
    );
    println!("linux ivc demo pass backend=ivshmem-pci");
    Ok(())
}

fn wait_for_session(session: &mut MessageSession<'_>, observed_events: &mut usize) -> Result<()> {
    let started = Instant::now();
    let mut previous_state = None;
    loop {
        let state = session.poll_handshake()?;
        if previous_state != Some(state) {
            session.ring_peer()?;
            previous_state = Some(state);
        }
        match state {
            IvshmemSessionState::Ready { local, peer } => {
                println!(
                    "linux ivc session marker=ready backend=ivshmem-pci role=subscriber peer=1 \
                     local_incarnation={local} peer_incarnation={peer}"
                );
                return Ok(());
            }
            IvshmemSessionState::WaitingForPeer { .. }
            | IvshmemSessionState::WaitingForPeerAcknowledgement { .. } => {}
            state => bail!("ivshmem Message V1 handshake left the waiting path: {state:?}"),
        }

        if started.elapsed() >= SESSION_TIMEOUT {
            let state = session
                .poll_handshake()
                .context("failed to recheck the ivshmem Message V1 handshake")?;
            bail!("session state/backpressure timeout after 30s: {state:?}");
        }

        // Reaching the wait always follows a queue/state check above. A timeout
        // returns to the top of the loop, which polls the handshake again.
        observe_event(session, observed_events)?;
    }
}

fn observe_event(session: &MessageSession<'_>, observed_events: &mut usize) -> Result<()> {
    if session.wait_event(EVENT_WAIT_MS)? == WaitResult::Interrupt {
        *observed_events += 1;
        if *observed_events == 1 {
            println!("linux ivc irq backend=ivshmem-pci source=uio-msix actual=1");
        }
    }
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
            println!("linux ivc recv request seq={sequence} len={len} msg={text}");
        }
        MessageDemoEvent::DataSent { sequence, len } => {
            println!("linux ivc send data seq={sequence} len={len}");
        }
        MessageDemoEvent::AckSent { sequence } => {
            println!("linux ivc send ack seq={sequence}");
        }
    }
}
