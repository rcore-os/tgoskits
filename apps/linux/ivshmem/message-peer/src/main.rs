use std::{
    process::ExitCode,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, bail};
use arceos_ivc_subscriber::{MessageDemoEvent, MessageDemoSubscriber};
use linux_ivc::ivshmem::ExclusiveDevice;

const ACK_BODY: &[u8] = b"ack from linux subscriber";
const SESSION_TIMEOUT: Duration = Duration::from_secs(30);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);
const BACKPRESSURE_TIMEOUT: Duration = Duration::from_secs(15);

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
    let common_size = device.common_size();
    let output_size = device.output_size();
    let bar2_size = device.bar2_size();
    println!(
        "linux ivc profile backend=ivshmem-pci peer={peer_id} max_peers={max_peers} \
         bar2={bar2_size:#x} common={common_size:#x} output={output_size:#x}"
    );

    let pending = device.take_endpoint()?;
    println!(
        "linux ivc session marker=waiting backend=ivshmem-pci role=subscriber peer={} \
         local_incarnation={}",
        peer_id,
        pending.local_incarnation()
    );

    let endpoint = pending.wait_ready(SESSION_TIMEOUT)?;
    let local_incarnation = endpoint.local_incarnation();
    let peer_incarnation = endpoint.peer_incarnation();
    let mut observed_events = endpoint.observed_events();
    let mut event_reported = false;
    println!(
        "linux ivc session marker=ready backend=ivshmem-pci role=subscriber peer={peer_id} \
         local_incarnation={local_incarnation} peer_incarnation={peer_incarnation}"
    );
    if observed_events > 0 {
        println!("linux ivc irq backend=ivshmem-pci source=uio-msix actual=1");
        event_reported = true;
    }

    let mut subscriber = MessageDemoSubscriber::new(endpoint, ACK_BODY)
        .map_err(|error| anyhow!("failed to create subscriber: {error}"))?;

    let started = Instant::now();
    let mut last_progress = Instant::now();
    while !subscriber.is_complete() {
        let outcome = subscriber
            .poll(print_event)
            .map_err(|error| anyhow!("Message V1 subscriber poll failed: {error}"))?;
        if outcome.progressed() {
            last_progress = Instant::now();
        }
        if outcome.is_complete() || subscriber.is_complete() {
            break;
        }
        if started.elapsed() >= TRANSFER_TIMEOUT {
            bail!(
                "transfer timeout: requests={} data={} acks={}",
                subscriber.request_count(),
                subscriber.data_count(),
                subscriber.ack_count()
            );
        }
        if last_progress.elapsed() >= BACKPRESSURE_TIMEOUT {
            bail!(
                "state/backpressure timeout: requests={} data={} acks={}",
                subscriber.request_count(),
                subscriber.data_count(),
                subscriber.ack_count()
            );
        }

        if subscriber
            .wait_for_progress()
            .map_err(|error| anyhow!("Message V1 subscriber wait failed: {error}"))?
        {
            observed_events += 1;
            if !event_reported {
                println!("linux ivc irq backend=ivshmem-pci source=uio-msix actual=1");
                event_reported = true;
            }
        }
    }

    while !subscriber
        .outbound_drained()
        .map_err(|error| anyhow!("Message V1 outbound drain check failed: {error}"))?
    {
        if started.elapsed() >= TRANSFER_TIMEOUT {
            bail!(
                "ACK delivery timeout: requests={} data={} acks={}",
                subscriber.request_count(),
                subscriber.data_count(),
                subscriber.ack_count()
            );
        }
        if subscriber
            .wait_for_progress()
            .map_err(|error| anyhow!("Message V1 subscriber wait failed: {error}"))?
        {
            observed_events += 1;
            if !event_reported {
                println!("linux ivc irq backend=ivshmem-pci source=uio-msix actual=1");
                event_reported = true;
            }
        }
    }

    if observed_events == 0 {
        bail!("no MSI-X/UIO interrupt was observed");
    }

    println!(
        "linux ivc session marker=complete backend=ivshmem-pci role=subscriber peer={peer_id} \
         local_incarnation={local_incarnation}"
    );
    println!("linux ivc demo pass backend=ivshmem-pci");
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
