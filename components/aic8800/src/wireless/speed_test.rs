//! 双向 TCP 吞吐量测试
//!
//! 照搬 wireless/aic8800_fdrv/src/net_dev.rs 中 run_speed_test 的逻辑，
//! 增加下载测试阶段。

use alloc::vec;
use core::net::{IpAddr, SocketAddr};

use ax_net::{
    RecvOptions, SendOptions, Shutdown, SocketAddrEx, SocketOps,
    options::{Configurable, SetSocketOption},
    tcp::TcpSocket,
};

/// 默认测试数据量 (4 MB)
pub const DEFAULT_TEST_SIZE: usize = 4 * 1024 * 1024;

/// 每次 send/recv 的块大小（与原始代码一致）
const CHUNK_SIZE: usize = 1024;

/// 读取 RISC-V time CSR
fn rdtime() -> u64 {
    let time: u64;
    unsafe { core::arch::asm!("rdtime {}", out(reg) time) };
    time
}

/// SG2002 定时器频率 25MHz，每个 tick = 40ns
const NANOS_PER_TICK: u64 = 40;

fn ticks_to_nanos(ticks: u64) -> u64 {
    ticks * NANOS_PER_TICK
}

/// 执行双向 TCP 吞吐量测试
///
/// 完全参照原始代码 wireless/aic8800_fdrv/src/net_dev.rs::run_speed_test
pub fn tcp_speed_test(
    host_ip: [u8; 4],
    port: u16,
    total_bytes: usize,
) -> Result<(f64, f64), &'static str> {
    let server_ip = alloc::format!(
        "{}.{}.{}.{}",
        host_ip[0],
        host_ip[1],
        host_ip[2],
        host_ip[3]
    );

    log::debug!("[speed-test] ===== Network Speed Test =====");
    log::debug!("[speed-test] Target: {}:{}", server_ip, port);
    log::debug!(
        "[speed-test] Total: {} bytes, Chunk: {} bytes",
        total_bytes,
        CHUNK_SIZE
    );

    // ---- Phase 1: ARP warmup ----
    log::debug!("[speed-test] Phase 1: ARP warmup...");
    for _ in 0..500 {
        ax_net::poll_interfaces();
        ax_task::sleep(core::time::Duration::from_millis(1));
    }

    // ---- Phase 2: TCP connect ----
    log::debug!(
        "[speed-test] Phase 2: Connecting to {}:{}...",
        server_ip,
        port
    );

    let socket = TcpSocket::new();
    let ip: IpAddr = server_ip.parse().expect("Invalid server IP");
    let remote = SocketAddr::new(ip, port);

    if let Err(e) = socket.connect(SocketAddrEx::Ip(remote)) {
        log::error!("[speed-test] TCP connect failed: {:?}", e);
        return Err("TCP connect failed");
    }
    log::debug!("[speed-test] Connected!");

    // 设置 send/recv 超时（1 秒），防止 block_on 永久阻塞
    // ax_net 默认 send_timeout_nanos = 0（无超时），导致缓冲区满时永远卡住
    let timeout = core::time::Duration::from_secs(1);
    let _ = socket.set_option(SetSocketOption::SendTimeout(&timeout));
    let _ = socket.set_option(SetSocketOption::ReceiveTimeout(&timeout));

    // ---- Phase 3: Upload (TX) ----
    log::debug!("[speed-test] Phase 3: Sending {} bytes...", total_bytes);

    let buf = vec![0xABu8; CHUNK_SIZE];
    let mut total_sent: usize = 0;
    let mut send_errors: usize = 0;

    let start_ticks = rdtime();

    while total_sent < total_bytes {
        let remaining = total_bytes - total_sent;
        let to_send = if remaining < CHUNK_SIZE {
            remaining
        } else {
            CHUNK_SIZE
        };
        let data: &[u8] = &buf[..to_send];

        match socket.send(data, SendOptions::default()) {
            Ok(n) => {
                total_sent += n;
                if total_sent % (100 * 1024) < n {
                    let pct = total_sent * 100 / total_bytes;
                    log::debug!(
                        "[speed-test] Upload: {}/{} bytes ({}%)",
                        total_sent,
                        total_bytes,
                        pct
                    );
                }
            }
            Err(e) => {
                send_errors += 1;
                if send_errors > 100 {
                    log::error!("[speed-test] Too many send errors, aborting. Last: {:?}", e);
                    break;
                }
                for _ in 0..100 {
                    ax_net::poll_interfaces();
                    ax_task::yield_now();
                }
            }
        }
    }

    let end_ticks = rdtime();
    let upload_ns = ticks_to_nanos(end_ticks - start_ticks);
    let upload_ms = upload_ns / 1_000_000;

    let upload_bits = (total_sent as u64) * 8;
    let upload_kbps = if upload_ns > 0 {
        upload_bits * 1_000_000_000 / upload_ns / 1000
    } else {
        0
    };
    let upload_mbps_f = upload_kbps as f64 / 1000.0;

    log::debug!("[speed-test] ===== Upload Results =====");
    log::debug!("[speed-test] Sent: {} bytes", total_sent);
    log::debug!("[speed-test] Time: {} ms", upload_ms);
    log::debug!(
        "[speed-test] Throughput: {} Kbps ({:.2} Mbps)",
        upload_kbps,
        upload_mbps_f
    );
    log::debug!("[speed-test] Send errors: {}", send_errors);

    // ---- Phase 4: Download (RX) ----
    log::debug!("[speed-test] Phase 4: Receiving {} bytes...", total_bytes);

    let mut recv_buf = vec![0u8; CHUNK_SIZE];
    let mut total_recv: usize = 0;
    let mut recv_errors: usize = 0;

    let start_ticks = rdtime();

    while total_recv < total_bytes {
        match socket.recv(&mut recv_buf[..], RecvOptions::default()) {
            Ok(n) => {
                if n == 0 {
                    log::error!("[speed-test] Connection closed during download");
                    break;
                }
                total_recv += n;
                if total_recv % (100 * 1024) < n {
                    let pct = total_recv * 100 / total_bytes;
                    log::debug!(
                        "[speed-test] Download: {}/{} bytes ({}%)",
                        total_recv,
                        total_bytes,
                        pct
                    );
                }
            }
            Err(e) => {
                recv_errors += 1;
                if recv_errors > 100 {
                    log::error!("[speed-test] Too many recv errors, aborting. Last: {:?}", e);
                    break;
                }
                for _ in 0..100 {
                    ax_net::poll_interfaces();
                    ax_task::yield_now();
                }
            }
        }
    }

    let end_ticks = rdtime();
    let download_ns = ticks_to_nanos(end_ticks - start_ticks);
    let download_ms = download_ns / 1_000_000;

    let download_bits = (total_recv as u64) * 8;
    let download_kbps = if download_ns > 0 {
        download_bits * 1_000_000_000 / download_ns / 1000
    } else {
        0
    };
    let download_mbps_f = download_kbps as f64 / 1000.0;

    log::debug!("[speed-test] ===== Download Results =====");
    log::debug!("[speed-test] Received: {} bytes", total_recv);
    log::debug!("[speed-test] Time: {} ms", download_ms);
    log::debug!(
        "[speed-test] Throughput: {} Kbps ({:.2} Mbps)",
        download_kbps,
        download_mbps_f
    );
    log::debug!("[speed-test] Recv errors: {}", recv_errors);

    let _ = socket.shutdown(Shutdown::Both);

    log::debug!(
        "[speed-test] === Final: Upload {:.2} Mbps | Download {:.2} Mbps ===",
        upload_mbps_f,
        download_mbps_f
    );

    Ok((upload_mbps_f, download_mbps_f))
}
