//! IVC application-layer protocol over UDP for inter-guest communication (Task 2).
//!
//! Wire header (16 bytes, little-endian):
//!   magic:u16=0xA1B2 | version:u8=1 | msg_type:u8 | seq:u32 |
//!   timestamp_ms:u32 | payload_len:u16 | checksum:u16
//! checksum = ones'-complement sum over the header (checksum field zeroed) + payload.
//!
//! Message types: 1=DATA 2=ACK 3=CONTROL 4=STATUS 5=ERROR.
//!
//! Reliability (UDP): the client sends DATA with a monotonically increasing seq
//! and waits for an ACK with a per-message timeout, retransmitting up to a cap.
//! The server verifies the checksum (replying ERROR on mismatch), de-duplicates
//! by seq (so retransmits are counted once), tolerates reordering, and ACKs every
//! valid DATA. A `lossy=K` server mode drops the first ACK for every Kth distinct
//! seq to force the client's retransmit path and the server's dedup path to run.
//!
//! Roles:
//!   ivcproto server <bind_addr> [lossy=K]
//!   ivcproto client <peer_addr> <count>
//! Output lines are prefixed PROTO- for the automated test harness to scrape.

use std::collections::HashMap;
use std::net::UdpSocket;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAGIC: u16 = 0xA1B2;
const VERSION: u8 = 1;
const HDR: usize = 16;

const T_DATA: u8 = 1;
const T_ACK: u8 = 2;
const T_CONTROL: u8 = 3;
const T_STATUS: u8 = 4;
const T_ERROR: u8 = 5;

const RETRANSMIT_CAP: u32 = 6;
const ACK_TIMEOUT: Duration = Duration::from_millis(400);

fn now_ms() -> u32 {
    (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
        & 0xffff_ffff) as u32
}

/// ones'-complement 16-bit checksum (Internet-style) over the bytes.
fn checksum(bytes: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < bytes.len() {
        sum += u16::from_le_bytes([bytes[i], bytes[i + 1]]) as u32;
        i += 2;
    }
    if i < bytes.len() {
        sum += bytes[i] as u32;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn encode(msg_type: u8, seq: u32, payload: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; HDR + payload.len()];
    buf[0..2].copy_from_slice(&MAGIC.to_le_bytes());
    buf[2] = VERSION;
    buf[3] = msg_type;
    buf[4..8].copy_from_slice(&seq.to_le_bytes());
    buf[8..12].copy_from_slice(&now_ms().to_le_bytes());
    buf[12..14].copy_from_slice(&(payload.len() as u16).to_le_bytes());
    // checksum field (14..16) stays zero while computing
    buf[HDR..].copy_from_slice(payload);
    let ck = checksum(&buf);
    buf[14..16].copy_from_slice(&ck.to_le_bytes());
    buf
}

struct Msg {
    version: u8,
    msg_type: u8,
    seq: u32,
    payload: Vec<u8>,
    checksum_ok: bool,
}

fn decode(buf: &[u8]) -> Option<Msg> {
    if buf.len() < HDR {
        return None;
    }
    if u16::from_le_bytes([buf[0], buf[1]]) != MAGIC {
        return None;
    }
    let version = buf[2];
    let msg_type = buf[3];
    let seq = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let plen = u16::from_le_bytes([buf[12], buf[13]]) as usize;
    if buf.len() < HDR + plen {
        return None;
    }
    let recv_ck = u16::from_le_bytes([buf[14], buf[15]]);
    let mut check = buf[..HDR + plen].to_vec();
    check[14] = 0;
    check[15] = 0;
    let checksum_ok = checksum(&check) == recv_ck;
    Some(Msg {
        version,
        msg_type,
        seq,
        payload: buf[HDR..HDR + plen].to_vec(),
        checksum_ok,
    })
}

fn run_server(bind: &str, lossy: u32) -> std::io::Result<()> {
    let sock = UdpSocket::bind(bind)?;
    println!("PROTO-SERVER listening on {bind} lossy={lossy}");
    let mut seen: HashMap<u32, u32> = HashMap::new(); // seq -> times received
    let mut dropped_once: HashMap<u32, bool> = HashMap::new();
    let mut unique = 0u64;
    let mut dups = 0u64;
    let mut corrupt = 0u64;
    let mut acks_dropped = 0u64;
    let mut buf = [0u8; 2048];
    let mut last = Instant::now();
    loop {
        sock.set_read_timeout(Some(Duration::from_secs(3)))?;
        let (n, peer) = match sock.recv_from(&mut buf) {
            Ok(v) => v,
            Err(_) => {
                // idle: if we've seen traffic and then 3s silence, summarize and exit.
                if unique > 0 && last.elapsed() > Duration::from_secs(3) {
                    break;
                }
                continue;
            }
        };
        last = Instant::now();
        let Some(msg) = decode(&buf[..n]) else { continue };
        if msg.version != VERSION {
            let e = encode(T_ERROR, msg.seq, b"version");
            let _ = sock.send_to(&e, peer);
            continue;
        }
        match msg.msg_type {
            T_CONTROL => {
                // session start/stop control message
                let s = encode(T_STATUS, msg.seq, b"ready");
                let _ = sock.send_to(&s, peer);
            }
            T_DATA => {
                if !msg.checksum_ok {
                    corrupt += 1;
                    let e = encode(T_ERROR, msg.seq, b"checksum");
                    let _ = sock.send_to(&e, peer);
                    continue;
                }
                let cnt = seen.entry(msg.seq).or_insert(0);
                *cnt += 1;
                if *cnt == 1 {
                    unique += 1;
                } else {
                    dups += 1; // reorder/retransmit de-duplicated
                }
                // lossy mode: drop the FIRST ack for every Kth distinct seq
                if lossy > 0 && msg.seq % lossy == 0 && !dropped_once.get(&msg.seq).copied().unwrap_or(false) {
                    dropped_once.insert(msg.seq, true);
                    acks_dropped += 1;
                    continue; // force a client retransmit
                }
                let ack = encode(T_ACK, msg.seq, &msg.payload[..msg.payload.len().min(4)]);
                let _ = sock.send_to(&ack, peer);
            }
            _ => {}
        }
    }
    println!(
        "PROTO-SERVER-RESULT unique={unique} dups={dups} corrupt={corrupt} acks_dropped={acks_dropped}"
    );
    Ok(())
}

fn run_client(peer: &str, count: u32) -> std::io::Result<()> {
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.connect(peer)?;
    sock.set_read_timeout(Some(ACK_TIMEOUT))?;
    println!("PROTO-CLIENT -> {peer} count={count}");

    // CONTROL handshake (with its own retransmit) to confirm the server is up.
    // Be patient: the peer guest may still be finishing boot / bringing up eth0.
    let mut ready = false;
    for _ in 0..150 {
        let _ = sock.send(&encode(T_CONTROL, 0, b"hello"));
        let mut b = [0u8; 2048];
        if let Ok(n) = sock.recv(&mut b) {
            if let Some(m) = decode(&b[..n]) {
                if m.msg_type == T_STATUS {
                    ready = true;
                    break;
                }
            }
        }
    }
    if !ready {
        println!("PROTO-CLIENT-RESULT ERROR server-not-ready");
        return Ok(());
    }

    let mut acked = 0u64;
    let mut retransmits = 0u64;
    let mut lost = 0u64;
    let mut rtt_sum = 0f64;
    let mut rtt_n = 0u64;
    let start = Instant::now();
    for seq in 1..=count {
        let payload = format!("ivc-msg-{seq:08}-payloadcheck").into_bytes();
        let pkt = encode(T_DATA, seq, &payload);
        let mut got = false;
        let t0 = Instant::now();
        for attempt in 0..RETRANSMIT_CAP {
            if attempt > 0 {
                retransmits += 1;
            }
            let _ = sock.send(&pkt);
            let mut b = [0u8; 2048];
            match sock.recv(&mut b) {
                Ok(n) => {
                    if let Some(m) = decode(&b[..n]) {
                        if m.msg_type == T_ACK && m.seq == seq && m.checksum_ok {
                            got = true;
                            rtt_sum += t0.elapsed().as_secs_f64() * 1000.0;
                            rtt_n += 1;
                            break;
                        }
                        if m.msg_type == T_ERROR {
                            // surface and retry
                            continue;
                        }
                    }
                }
                Err(_) => continue, // timeout -> retransmit
            }
        }
        if got {
            acked += 1;
        } else {
            lost += 1;
        }
    }
    let secs = start.elapsed().as_secs_f64().max(1e-6);
    let rtt_avg = if rtt_n > 0 { rtt_sum / rtt_n as f64 } else { 0.0 };
    let thrpt = acked as f64 / secs;
    println!(
        "PROTO-CLIENT-RESULT sent={count} acked={acked} lost={lost} retransmits={retransmits} rtt_avg_ms={rtt_avg:.2} msgs_per_s={thrpt:.1} elapsed_s={secs:.2}"
    );
    // STATUS report back to the server (also exercises the reverse direction).
    let _ = sock.send(&encode(T_STATUS, count, b"client-done"));
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let usage = || {
        eprintln!("usage: ivcproto server <bind> [lossy=K] | client <peer> <count>");
    };
    if args.len() < 3 {
        usage();
        std::process::exit(2);
    }
    let res = match args[1].as_str() {
        "server" => {
            let lossy = args
                .get(3)
                .and_then(|s| s.strip_prefix("lossy="))
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            run_server(&args[2], lossy)
        }
        "client" => {
            let count = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(50);
            run_client(&args[2], count)
        }
        _ => {
            usage();
            std::process::exit(2);
        }
    };
    if let Err(e) = res {
        println!("PROTO-FATAL {e}");
        std::process::exit(1);
    }
}
