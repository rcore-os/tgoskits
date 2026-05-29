use alloc::{format, string::String};
use core::{
    fmt::Write,
    sync::atomic::{AtomicU64, Ordering},
};

const DEPTH_BINS: usize = 9;

static VIRTQUEUE_ADD: AtomicU64 = AtomicU64::new(0);
static VIRTQUEUE_NOTIFY_KICK: AtomicU64 = AtomicU64::new(0);
static VIRTQUEUE_POP_COMPLETE: AtomicU64 = AtomicU64::new(0);
static VIRTQUEUE_ADD_NOTIFY_WAIT_POP: AtomicU64 = AtomicU64::new(0);
static VIRTQUEUE_DEPTH_MAX: AtomicU64 = AtomicU64::new(0);
static VIRTQUEUE_DEPTH_HIST: [AtomicU64; DEPTH_BINS] = [const { AtomicU64::new(0) }; DEPTH_BINS];

static BLK_READ_REQUESTS: AtomicU64 = AtomicU64::new(0);
static BLK_WRITE_REQUESTS: AtomicU64 = AtomicU64::new(0);
static BLK_READ_BYTES: AtomicU64 = AtomicU64::new(0);
static BLK_WRITE_BYTES: AtomicU64 = AtomicU64::new(0);

static NET_RX_PACKETS: AtomicU64 = AtomicU64::new(0);
static NET_TX_PACKETS: AtomicU64 = AtomicU64::new(0);
static NET_RX_BYTES: AtomicU64 = AtomicU64::new(0);
static NET_TX_BYTES: AtomicU64 = AtomicU64::new(0);
static NET_RX_COPY_WITHIN_COUNT: AtomicU64 = AtomicU64::new(0);
static NET_RX_COPY_WITHIN_BYTES: AtomicU64 = AtomicU64::new(0);
static NET_TX_STAGING_COPY_COUNT: AtomicU64 = AtomicU64::new(0);
static NET_TX_STAGING_COPY_BYTES: AtomicU64 = AtomicU64::new(0);
static NET_INFLIGHT_INSERT: AtomicU64 = AtomicU64::new(0);
static NET_INFLIGHT_REMOVE: AtomicU64 = AtomicU64::new(0);
static NET_INFLIGHT_GET: AtomicU64 = AtomicU64::new(0);

pub fn record_blk_read(bytes: usize) {
    BLK_READ_REQUESTS.fetch_add(1, Ordering::Relaxed);
    BLK_READ_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
    record_add_notify_wait_pop(1);
}

pub fn record_blk_write(bytes: usize) {
    BLK_WRITE_REQUESTS.fetch_add(1, Ordering::Relaxed);
    BLK_WRITE_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
    record_add_notify_wait_pop(1);
}

pub fn record_net_tx(bytes: usize, depth: usize) {
    NET_TX_PACKETS.fetch_add(1, Ordering::Relaxed);
    NET_TX_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
    NET_TX_STAGING_COPY_COUNT.fetch_add(1, Ordering::Relaxed);
    NET_TX_STAGING_COPY_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
    NET_INFLIGHT_INSERT.fetch_add(1, Ordering::Relaxed);
    record_virtqueue_add(depth);
    record_notify_kick();
}

pub fn record_net_tx_complete(depth: usize) {
    NET_INFLIGHT_GET.fetch_add(1, Ordering::Relaxed);
    NET_INFLIGHT_REMOVE.fetch_add(1, Ordering::Relaxed);
    record_pop_complete(depth);
}

pub fn record_net_rx_submit(depth: usize) {
    NET_INFLIGHT_INSERT.fetch_add(1, Ordering::Relaxed);
    record_virtqueue_add(depth);
    record_notify_kick();
}

pub fn record_net_rx_complete(bytes: usize, copy_bytes: usize, depth: usize) {
    NET_RX_PACKETS.fetch_add(1, Ordering::Relaxed);
    NET_RX_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
    NET_RX_COPY_WITHIN_COUNT.fetch_add(1, Ordering::Relaxed);
    NET_RX_COPY_WITHIN_BYTES.fetch_add(copy_bytes as u64, Ordering::Relaxed);
    NET_INFLIGHT_GET.fetch_add(1, Ordering::Relaxed);
    NET_INFLIGHT_REMOVE.fetch_add(1, Ordering::Relaxed);
    record_pop_complete(depth);
}

fn record_add_notify_wait_pop(depth: usize) {
    VIRTQUEUE_ADD_NOTIFY_WAIT_POP.fetch_add(1, Ordering::Relaxed);
    record_virtqueue_add(depth);
    record_notify_kick();
    record_pop_complete(depth.saturating_sub(1));
}

fn record_virtqueue_add(depth: usize) {
    VIRTQUEUE_ADD.fetch_add(1, Ordering::Relaxed);
    record_depth(depth);
}

fn record_notify_kick() {
    VIRTQUEUE_NOTIFY_KICK.fetch_add(1, Ordering::Relaxed);
}

fn record_pop_complete(depth: usize) {
    VIRTQUEUE_POP_COMPLETE.fetch_add(1, Ordering::Relaxed);
    record_depth(depth);
}

fn record_depth(depth: usize) {
    update_max(&VIRTQUEUE_DEPTH_MAX, depth as u64);
    VIRTQUEUE_DEPTH_HIST[depth_bin(depth)].fetch_add(1, Ordering::Relaxed);
}

fn depth_bin(depth: usize) -> usize {
    match depth {
        0 => 0,
        1 => 1,
        2 => 2,
        3 | 4 => 3,
        5..=8 => 4,
        9..=16 => 5,
        17..=32 => 6,
        33..=64 => 7,
        _ => 8,
    }
}

fn update_max(max: &AtomicU64, value: u64) {
    let mut current = max.load(Ordering::Relaxed);
    while value > current {
        match max.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(next) => current = next,
        }
    }
}

pub fn render() -> String {
    let mut output = format!(
        "QPERF_METRIC virtqueue_add_count={} virtio_notify_kick_count={} \
         virtqueue_pop_complete_count={} virtqueue_add_notify_wait_pop_count={} \
         virtqueue_depth_max={} virtqueue_depth_hist_0={} virtqueue_depth_hist_1={} \
         virtqueue_depth_hist_2={} virtqueue_depth_hist_3_4={} virtqueue_depth_hist_5_8={} \
         virtqueue_depth_hist_9_16={} virtqueue_depth_hist_17_32={} virtqueue_depth_hist_33_64={} \
         virtqueue_depth_hist_gt64={} virtio_blk_read_requests={} virtio_blk_write_requests={} \
         virtio_blk_read_bytes={} virtio_blk_write_bytes={} virtio_net_rx_packets={} \
         virtio_net_tx_packets={} virtio_net_rx_bytes={} virtio_net_tx_bytes={} \
         virtio_net_rx_copy_within_count={} virtio_net_rx_copy_within_bytes={} \
         virtio_net_tx_staging_copy_count={} virtio_net_tx_staging_copy_bytes={} \
         virtio_net_inflight_insert_count={} virtio_net_inflight_remove_count={} \
         virtio_net_inflight_get_count={}\n",
        VIRTQUEUE_ADD.load(Ordering::Relaxed),
        VIRTQUEUE_NOTIFY_KICK.load(Ordering::Relaxed),
        VIRTQUEUE_POP_COMPLETE.load(Ordering::Relaxed),
        VIRTQUEUE_ADD_NOTIFY_WAIT_POP.load(Ordering::Relaxed),
        VIRTQUEUE_DEPTH_MAX.load(Ordering::Relaxed),
        VIRTQUEUE_DEPTH_HIST[0].load(Ordering::Relaxed),
        VIRTQUEUE_DEPTH_HIST[1].load(Ordering::Relaxed),
        VIRTQUEUE_DEPTH_HIST[2].load(Ordering::Relaxed),
        VIRTQUEUE_DEPTH_HIST[3].load(Ordering::Relaxed),
        VIRTQUEUE_DEPTH_HIST[4].load(Ordering::Relaxed),
        VIRTQUEUE_DEPTH_HIST[5].load(Ordering::Relaxed),
        VIRTQUEUE_DEPTH_HIST[6].load(Ordering::Relaxed),
        VIRTQUEUE_DEPTH_HIST[7].load(Ordering::Relaxed),
        VIRTQUEUE_DEPTH_HIST[8].load(Ordering::Relaxed),
        BLK_READ_REQUESTS.load(Ordering::Relaxed),
        BLK_WRITE_REQUESTS.load(Ordering::Relaxed),
        BLK_READ_BYTES.load(Ordering::Relaxed),
        BLK_WRITE_BYTES.load(Ordering::Relaxed),
        NET_RX_PACKETS.load(Ordering::Relaxed),
        NET_TX_PACKETS.load(Ordering::Relaxed),
        NET_RX_BYTES.load(Ordering::Relaxed),
        NET_TX_BYTES.load(Ordering::Relaxed),
        NET_RX_COPY_WITHIN_COUNT.load(Ordering::Relaxed),
        NET_RX_COPY_WITHIN_BYTES.load(Ordering::Relaxed),
        NET_TX_STAGING_COPY_COUNT.load(Ordering::Relaxed),
        NET_TX_STAGING_COPY_BYTES.load(Ordering::Relaxed),
        NET_INFLIGHT_INSERT.load(Ordering::Relaxed),
        NET_INFLIGHT_REMOVE.load(Ordering::Relaxed),
        NET_INFLIGHT_GET.load(Ordering::Relaxed),
    );
    let _ = writeln!(
        output,
        "QPERF_METRIC qperf_metrics_export=1 qperf_metrics_scope=ax_driver_virtio"
    );
    output
}

pub fn reset() {
    for item in [
        &VIRTQUEUE_ADD,
        &VIRTQUEUE_NOTIFY_KICK,
        &VIRTQUEUE_POP_COMPLETE,
        &VIRTQUEUE_ADD_NOTIFY_WAIT_POP,
        &VIRTQUEUE_DEPTH_MAX,
        &BLK_READ_REQUESTS,
        &BLK_WRITE_REQUESTS,
        &BLK_READ_BYTES,
        &BLK_WRITE_BYTES,
        &NET_RX_PACKETS,
        &NET_TX_PACKETS,
        &NET_RX_BYTES,
        &NET_TX_BYTES,
        &NET_RX_COPY_WITHIN_COUNT,
        &NET_RX_COPY_WITHIN_BYTES,
        &NET_TX_STAGING_COPY_COUNT,
        &NET_TX_STAGING_COPY_BYTES,
        &NET_INFLIGHT_INSERT,
        &NET_INFLIGHT_REMOVE,
        &NET_INFLIGHT_GET,
    ] {
        item.store(0, Ordering::Relaxed);
    }
    for item in &VIRTQUEUE_DEPTH_HIST {
        item.store(0, Ordering::Relaxed);
    }
}
