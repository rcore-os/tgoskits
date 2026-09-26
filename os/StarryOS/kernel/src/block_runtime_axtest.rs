//! Starry axtests for the asynchronous block runtime boundary.

#[cfg(feature = "block-runtime-write-tests")]
use alloc::vec;
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    future::{Future, poll_fn},
    pin::{Pin, pin},
    task::Poll,
};

#[cfg(feature = "block-runtime-visionfive2-reserved-region")]
use ax_fs_ng::BlockRegion;
use ax_fs_ng::{BlockDeviceHandle, BlockError, block_batch_stats};
#[cfg(feature = "qperf-metrics")]
use ax_runtime::diagnostics::qperf_runtime_scheduler_metrics_snapshot;
use ax_runtime::hal::time::monotonic_time_nanos;
use rdif_block::CompletedRequest;

const BENCHMARK_REQUESTS: usize = 256;
const BENCHMARK_ROUNDS: usize = 3;
const ASYNC_CONCURRENCIES: [usize; 4] = [1, 2, 4, 8];
const ASYNC_CONCURRENCY: usize = 8;
#[cfg(feature = "block-runtime-visionfive2-reserved-region")]
const MULTI_RW_BLOCKS_PER_REQUEST: u32 = 8;
#[cfg(all(
    feature = "block-runtime-write-tests",
    not(feature = "block-runtime-visionfive2-reserved-region")
))]
const MULTI_RW_BLOCKS_PER_REQUEST: u32 = 4;
#[cfg(feature = "block-runtime-write-tests")]
const MULTI_RW_ASYNC_CONCURRENCY: usize = 4;
#[cfg(feature = "block-runtime-visionfive2-reserved-region")]
const MIXED_RW_BLOCKS_PER_REQUEST: u32 = 8;
#[cfg(all(
    feature = "block-runtime-write-tests",
    not(feature = "block-runtime-visionfive2-reserved-region")
))]
const MIXED_RW_BLOCKS_PER_REQUEST: u32 = 2;
#[cfg(feature = "block-runtime-visionfive2-reserved-region")]
const RW_BENCHMARK_REGION_BLOCKS: u32 = 64;
#[cfg(all(
    feature = "block-runtime-write-tests",
    not(feature = "block-runtime-visionfive2-reserved-region")
))]
const RW_BENCHMARK_REGION_BLOCKS: u32 = 24;
#[cfg(all(
    feature = "block-runtime-write-tests",
    not(feature = "block-runtime-visionfive2-reserved-region")
))]
const SCRATCH_DISK_MARKER: &[u8] = b"TGOS_BLOCK_SCRATCH_V1\n";
#[cfg(feature = "block-runtime-visionfive2-reserved-region")]
const SCRATCH_TEST_START_LBA: u64 = 2_099_200;
#[cfg(all(
    feature = "block-runtime-write-tests",
    not(feature = "block-runtime-visionfive2-reserved-region")
))]
const SCRATCH_TEST_START_LBA: u64 = 1;
#[cfg(feature = "block-runtime-visionfive2-reserved-region")]
const SCRATCH_REGION_BLOCKS: u32 = 256;
#[cfg(all(
    feature = "block-runtime-write-tests",
    not(feature = "block-runtime-visionfive2-reserved-region")
))]
const SCRATCH_REGION_BLOCKS: u32 = 24;
#[cfg(feature = "block-runtime-write-tests")]
const WRITE_TEST_BLOCKS: u32 = 8;
#[cfg(feature = "block-runtime-write-tests")]
const MIXED_TEST_BLOCKS: u32 = 8;
#[cfg(feature = "block-runtime-visionfive2-reserved-region")]
const MULTI_DESCRIPTOR_TEST_BLOCKS: u32 = 16;

#[derive(Clone, Copy, Debug, Default)]
struct CpuRuntimeSnapshot {
    // This runtime value belongs to the axtest's current scheduler thread.
    charged_runtime_ns: u64,
    // qperf counters are aggregate scheduler counters, not per-thread values.
    context_switches: u64,
    context_switches_blocked: u64,
    context_switches_yield: u64,
    context_switches_preempted: u64,
}

fn cpu_runtime_snapshot() -> CpuRuntimeSnapshot {
    let runtime = ax_runtime::task::thread::current::current_thread_handle()
        .expect("axtest must execute on a scheduler thread")
        .runtime()
        .expect("current scheduler thread runtime must be observable");
    #[cfg(feature = "qperf-metrics")]
    let scheduler = qperf_runtime_scheduler_metrics_snapshot().task;
    #[cfg(not(feature = "qperf-metrics"))]
    let scheduler = CpuRuntimeSnapshot::default();
    CpuRuntimeSnapshot {
        charged_runtime_ns: runtime.charged_runtime_ns(),
        context_switches: scheduler.context_switches,
        context_switches_blocked: scheduler.context_switches_blocked,
        context_switches_yield: scheduler.context_switches_yield,
        context_switches_preempted: scheduler.context_switches_preempted,
    }
}

fn cpu_runtime_delta(before: CpuRuntimeSnapshot, after: CpuRuntimeSnapshot) -> CpuRuntimeSnapshot {
    CpuRuntimeSnapshot {
        charged_runtime_ns: after
            .charged_runtime_ns
            .saturating_sub(before.charged_runtime_ns),
        context_switches: after
            .context_switches
            .saturating_sub(before.context_switches),
        context_switches_blocked: after
            .context_switches_blocked
            .saturating_sub(before.context_switches_blocked),
        context_switches_yield: after
            .context_switches_yield
            .saturating_sub(before.context_switches_yield),
        context_switches_preempted: after
            .context_switches_preempted
            .saturating_sub(before.context_switches_preempted),
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct BlockBatchSnapshot {
    submitted_requests: u64,
    completed_requests: u64,
    failed_requests: u64,
    submission_batches: u64,
    commit_calls: u64,
    commit_failures: u64,
    largest_batch: usize,
    peak_inflight: usize,
}

fn block_batch_snapshot() -> BlockBatchSnapshot {
    let stats = block_batch_stats();
    BlockBatchSnapshot {
        submitted_requests: stats.submitted_requests,
        completed_requests: stats.completed_requests,
        failed_requests: stats.failed_requests,
        submission_batches: stats.submission_batches,
        commit_calls: stats.commit_calls,
        commit_failures: stats.commit_failures,
        largest_batch: stats.largest_batch,
        peak_inflight: stats.peak_inflight,
    }
}

fn block_batch_delta(before: BlockBatchSnapshot, after: BlockBatchSnapshot) -> BlockBatchSnapshot {
    BlockBatchSnapshot {
        submitted_requests: after
            .submitted_requests
            .saturating_sub(before.submitted_requests),
        completed_requests: after
            .completed_requests
            .saturating_sub(before.completed_requests),
        failed_requests: after.failed_requests.saturating_sub(before.failed_requests),
        submission_batches: after
            .submission_batches
            .saturating_sub(before.submission_batches),
        commit_calls: after.commit_calls.saturating_sub(before.commit_calls),
        commit_failures: after.commit_failures.saturating_sub(before.commit_failures),
        largest_batch: after.largest_batch,
        peak_inflight: after.peak_inflight,
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct LatencySummary {
    samples: u64,
    average_ns: u64,
    p50_ns: u64,
    p95_ns: u64,
    p99_ns: u64,
    max_ns: u64,
}

fn summarize_latencies(latencies: &mut [u64]) -> LatencySummary {
    if latencies.is_empty() {
        return LatencySummary::default();
    }
    latencies.sort_unstable();
    let last = latencies.len() - 1;
    let percentile = |percent: usize| {
        let rank = (latencies.len() * percent).saturating_add(99) / 100;
        latencies[rank.max(1) - 1]
    };
    let sum = latencies.iter().copied().fold(0_u64, u64::saturating_add);
    LatencySummary {
        samples: latencies.len() as u64,
        average_ns: sum / latencies.len() as u64,
        p50_ns: percentile(50),
        p95_ns: percentile(95),
        p99_ns: percentile(99),
        max_ns: latencies[last],
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct BenchmarkResult {
    requests: usize,
    concurrency: usize,
    elapsed_ns: u64,
    completed_requests: u64,
    batch: BlockBatchSnapshot,
    cpu: CpuRuntimeSnapshot,
    progress_polls: u64,
    latency: LatencySummary,
}

#[cfg(feature = "block-runtime-write-tests")]
#[derive(Clone, Copy, Debug, Default)]
struct IoPhaseResult {
    requests: u64,
    bytes: u64,
    latency: LatencySummary,
}

#[cfg(feature = "block-runtime-write-tests")]
#[derive(Clone, Copy, Debug, Default)]
struct ReadWriteBenchmarkResult {
    concurrency: usize,
    elapsed_ns: u64,
    read: IoPhaseResult,
    write: IoPhaseResult,
    batch: BlockBatchSnapshot,
    cpu: CpuRuntimeSnapshot,
    progress_polls: u64,
}

fn median_elapsed_benchmark_result(mut rounds: Vec<BenchmarkResult>) -> BenchmarkResult {
    assert_eq!(rounds.len(), BENCHMARK_ROUNDS);
    rounds.sort_unstable_by_key(|result| result.elapsed_ns);
    rounds[rounds.len() / 2]
}

#[cfg(feature = "block-runtime-write-tests")]
fn median_elapsed_read_write_result(
    mut rounds: Vec<ReadWriteBenchmarkResult>,
) -> ReadWriteBenchmarkResult {
    assert_eq!(rounds.len(), BENCHMARK_ROUNDS);
    rounds.sort_unstable_by_key(|result| result.elapsed_ns);
    rounds[rounds.len() / 2]
}

fn rate_per_second(count: u64, elapsed_ns: u64) -> u64 {
    count.saturating_mul(1_000_000_000) / elapsed_ns.max(1)
}
fn cpu_runtime_permille(cpu_runtime_ns: u64, elapsed_ns: u64) -> u64 {
    cpu_runtime_ns.saturating_mul(1_000) / elapsed_ns.max(1)
}

fn print_benchmark_result(label: &str, result: BenchmarkResult, block_size: usize) {
    let block_size = u64::try_from(block_size).expect("logical block size must fit in u64");
    let bytes = result.completed_requests.saturating_mul(block_size);
    axtest::axtest_println!(
        "{label} requests={} concurrency={} benchmark_rounds={} elapsed_ns={} average_latency_ns={} p50_latency_ns={} \
         p95_latency_ns={} p99_latency_ns={} max_latency_ns={} cpu_runtime_ns={} \
         cpu_runtime_permille={} scheduler_metrics_enabled={} requests_per_sec={} \
         bytes_per_sec={} submitted={} completed={} failed={} commit_calls={} commit_failures={} \
         submission_batches={} largest_batch_global={} peak_inflight_global={} \
         scheduler_context_switches={} scheduler_blocked_switches={} \
         scheduler_yielded_switches={} scheduler_preempted_switches={} progress_polls={}",
        result.requests,
        result.concurrency,
        BENCHMARK_ROUNDS,
        result.elapsed_ns,
        result.latency.average_ns,
        result.latency.p50_ns,
        result.latency.p95_ns,
        result.latency.p99_ns,
        result.latency.max_ns,
        result.cpu.charged_runtime_ns,
        cpu_runtime_permille(result.cpu.charged_runtime_ns, result.elapsed_ns),
        cfg!(feature = "qperf-metrics"),
        rate_per_second(result.completed_requests, result.elapsed_ns),
        rate_per_second(bytes, result.elapsed_ns),
        result.batch.submitted_requests,
        result.batch.completed_requests,
        result.batch.failed_requests,
        result.batch.commit_calls,
        result.batch.commit_failures,
        result.batch.submission_batches,
        result.batch.largest_batch,
        result.batch.peak_inflight,
        result.cpu.context_switches,
        result.cpu.context_switches_blocked,
        result.cpu.context_switches_yield,
        result.cpu.context_switches_preempted,
        result.progress_polls,
    );
}

#[cfg(feature = "block-runtime-write-tests")]
fn print_read_write_benchmark_result(
    label: &str,
    result: ReadWriteBenchmarkResult,
    blocks_per_request: u32,
) {
    let requests = result
        .read
        .requests
        .saturating_add(result.write.requests);
    let bytes = result.read.bytes.saturating_add(result.write.bytes);
    axtest::axtest_println!(
        "{label} requests={} concurrency={} benchmark_rounds={} blocks_per_request={} elapsed_ns={} \
         operations_per_sec={} bytes_per_sec={} read_requests={} write_requests={} \
         read_bytes={} write_bytes={} read_average_latency_ns={} read_p50_latency_ns={} \
         read_p95_latency_ns={} read_p99_latency_ns={} read_max_latency_ns={} \
         write_average_latency_ns={} write_p50_latency_ns={} write_p95_latency_ns={} \
         write_p99_latency_ns={} write_max_latency_ns={} cpu_runtime_ns={} \
         cpu_runtime_permille={} scheduler_metrics_enabled={} submitted={} completed={} \
         failed={} commit_calls={} commit_failures={} submission_batches={} \
         largest_batch_global={} peak_inflight_global={} scheduler_context_switches={} \
         scheduler_blocked_switches={} scheduler_yielded_switches={} \
         scheduler_preempted_switches={} progress_polls={}",
        requests,
        result.concurrency,
        BENCHMARK_ROUNDS,
        blocks_per_request,
        result.elapsed_ns,
        rate_per_second(requests, result.elapsed_ns),
        rate_per_second(bytes, result.elapsed_ns),
        result.read.requests,
        result.write.requests,
        result.read.bytes,
        result.write.bytes,
        result.read.latency.average_ns,
        result.read.latency.p50_ns,
        result.read.latency.p95_ns,
        result.read.latency.p99_ns,
        result.read.latency.max_ns,
        result.write.latency.average_ns,
        result.write.latency.p50_ns,
        result.write.latency.p95_ns,
        result.write.latency.p99_ns,
        result.write.latency.max_ns,
        result.cpu.charged_runtime_ns,
        cpu_runtime_permille(result.cpu.charged_runtime_ns, result.elapsed_ns),
        cfg!(feature = "qperf-metrics"),
        result.batch.submitted_requests,
        result.batch.completed_requests,
        result.batch.failed_requests,
        result.batch.commit_calls,
        result.batch.commit_failures,
        result.batch.submission_batches,
        result.batch.largest_batch,
        result.batch.peak_inflight,
        result.cpu.context_switches,
        result.cpu.context_switches_blocked,
        result.cpu.context_switches_yield,
        result.cpu.context_switches_preempted,
        result.progress_polls,
    );
}

fn assert_successful_read_bytes(request: &CompletedRequest, byte_len: usize) {
    assert_eq!(request.result, Ok(()));
    assert_eq!(
        request.data.as_ref().map(|data| data.len().get()),
        Some(byte_len)
    );
}

fn assert_successful_read(request: &CompletedRequest, block_size: usize) {
    assert_successful_read_bytes(request, block_size);
}

#[cfg(feature = "block-runtime-write-tests")]
fn writable_test_device() -> Arc<BlockDeviceHandle> {
    let devices = BlockDeviceHandle::axtest_devices()
        .expect("block runtime must be installed for block axtest");
    #[cfg(feature = "block-runtime-visionfive2-reserved-region")]
    let candidates = devices
        .iter()
        .filter(|device| !device.device_info().read_only)
        .filter(|device| {
            let info = device.device_info();
            if info.logical_block_size != 512 {
                return false;
            }
            if SCRATCH_TEST_START_LBA
                .checked_add(u64::from(SCRATCH_REGION_BLOCKS))
                .is_none_or(|end| end > info.num_blocks)
            {
                return false;
            }

            let scratch =
                BlockRegion::new(SCRATCH_TEST_START_LBA, u64::from(SCRATCH_REGION_BLOCKS));
            // The VisionFive 2 profile is specifically for the reserved
            // extent on the mounted root SD card. Do not fall back to an
            // unrelated writable disk whose fixed LBA range has not been
            // explicitly identified as scratch space.
            let Some(root) = ax_fs_ng::root::axtest_root_region(device) else {
                return false;
            };
            scratch.end_lba <= root.start_lba || root.end_lba <= scratch.start_lba
        })
        .collect::<Vec<_>>();

    #[cfg(not(feature = "block-runtime-visionfive2-reserved-region"))]
    let candidates = devices
        .iter()
        .filter(|device| {
            !ax_fs_ng::root::axtest_is_root_device(device) && !device.device_info().read_only
        })
        .filter(|device| {
            let info = device.device_info();
            if SCRATCH_TEST_START_LBA
                .checked_add(u64::from(SCRATCH_REGION_BLOCKS))
                .is_none_or(|end| end > info.num_blocks)
                || info.logical_block_size < SCRATCH_DISK_MARKER.len()
            {
                return false;
            }
            let Ok(marker) = device.axtest_read_sync(0) else {
                return false;
            };
            if marker.result.is_err()
                || marker.data.as_ref().map(|data| data.len().get())
                    != Some(info.logical_block_size)
            {
                return false;
            }
            let Some(data) = marker.data.as_ref() else {
                return false;
            };
            let mut bytes = vec![0; info.logical_block_size];
            data.copy_to_slice_cpu(&mut bytes);
            bytes.starts_with(SCRATCH_DISK_MARKER)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        candidates.len(),
        1,
        "write axtests require exactly one eligible scratch device"
    );
    let device = Arc::clone(candidates[0]);
    let info = device.device_info();

    // The root region and scratch extent are logged for the board record;
    // eligibility was already enforced by the candidate filters above.
    #[cfg(feature = "block-runtime-visionfive2-reserved-region")]
    if let Some(root) = ax_fs_ng::root::axtest_root_region(&device) {
        axtest::axtest_println!(
            "BLOCK_ROOT_REGION start_lba={} end_lba={}",
            root.start_lba,
            root.end_lba,
        );
    }
    axtest::axtest_println!(
        "BLOCK_WRITE_DEVICE name={} blocks={} logical_block_size={} scratch_start_lba={} \
         scratch_blocks={} scratch_end_lba={} scratch_last_lba={}",
        device.name(),
        info.num_blocks,
        info.logical_block_size,
        SCRATCH_TEST_START_LBA,
        SCRATCH_REGION_BLOCKS,
        SCRATCH_TEST_START_LBA + u64::from(SCRATCH_REGION_BLOCKS),
        SCRATCH_TEST_START_LBA + u64::from(SCRATCH_REGION_BLOCKS) - 1,
    );
    device
}

#[cfg(feature = "block-runtime-write-tests")]
fn pattern_bytes(block_size: usize, block_count: u32, seed: u8) -> Vec<u8> {
    let byte_len = block_size
        .checked_mul(block_count as usize)
        .expect("block axtest pattern length must fit in usize");
    (0..byte_len)
        .map(|index| {
            seed.wrapping_add(index as u8)
                .rotate_left((index % 8) as u32)
        })
        .collect()
}

#[cfg(feature = "block-runtime-write-tests")]
fn read_region_sync(device: &Arc<BlockDeviceHandle>, lba: u64, block_count: u32) -> Vec<u8> {
    let block_size = device.device_info().logical_block_size;
    let byte_len = block_size
        .checked_mul(block_count as usize)
        .expect("block axtest read length must fit in usize");
    let request = device
        .axtest_read_blocks_sync(lba, block_count)
        .unwrap_or_else(|error| panic!("block axtest read failed: {error:?}"));
    assert_successful_read_bytes(&request, byte_len);
    let data = request
        .data
        .as_ref()
        .expect("successful block axtest read must return DMA data");
    let mut bytes = vec![0; byte_len];
    data.copy_to_slice_cpu(&mut bytes);
    bytes
}

#[cfg(feature = "block-runtime-write-tests")]
fn assert_successful_write(request: &CompletedRequest) {
    assert_eq!(request.result, Ok(()));
}

#[cfg(feature = "block-runtime-write-tests")]
struct ScratchRegionGuard {
    device: Arc<BlockDeviceHandle>,
    lba: u64,
    block_count: u32,
    original: Vec<u8>,
    armed: bool,
}

#[cfg(feature = "block-runtime-write-tests")]
impl ScratchRegionGuard {
    fn new(device: Arc<BlockDeviceHandle>, lba: u64, block_count: u32, original: Vec<u8>) -> Self {
        Self {
            device,
            lba,
            block_count,
            original,
            armed: true,
        }
    }

    fn restore(&mut self) {
        if !self.armed {
            return;
        }
        let restore = self
            .device
            .axtest_write_blocks_sync(self.lba, self.block_count, &self.original)
            .unwrap_or_else(|error| panic!("scratch-region restore failed: {error:?}"));
        assert_successful_write(&restore);
        assert_eq!(
            read_region_sync(&self.device, self.lba, self.block_count),
            self.original,
            "scratch-region restore did not reproduce the original bytes"
        );
        self.armed = false;
    }
}

#[cfg(feature = "block-runtime-write-tests")]
impl Drop for ScratchRegionGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        match self
            .device
            .axtest_write_blocks_sync(self.lba, self.block_count, &self.original)
        {
            Ok(request) if request.result.is_ok() => {
                axtest::axtest_println!(
                    "BLOCK_SCRATCH_RECOVERED_ON_DROP lba={} blocks={}",
                    self.lba,
                    self.block_count,
                );
            }
            Ok(request) => {
                axtest::axtest_println!(
                    "BLOCK_SCRATCH_RECOVERY_FAILED_ON_DROP lba={} blocks={} result={:?}",
                    self.lba,
                    self.block_count,
                    request.result,
                );
            }
            Err(error) => {
                axtest::axtest_println!(
                    "BLOCK_SCRATCH_RECOVERY_SUBMIT_FAILED_ON_DROP lba={} blocks={} error={:?}",
                    self.lba,
                    self.block_count,
                    error,
                );
            }
        }
    }
}

#[cfg(feature = "block-runtime-write-tests")]
enum MixedExpectation {
    Write,
    Read(Vec<u8>),
}

async fn read_one(
    device: Arc<BlockDeviceHandle>,
    lba: u64,
) -> Result<CompletedRequest, BlockError> {
    device.axtest_read(lba).await
}

fn run_sync_benchmark(device: &Arc<BlockDeviceHandle>, block_count: u64) -> BenchmarkResult {
    let batch_before = block_batch_snapshot();
    let cpu_before = cpu_runtime_snapshot();
    let start = monotonic_time_nanos();
    let mut completed_requests = 0_u64;
    let mut latencies = Vec::with_capacity(BENCHMARK_REQUESTS);
    for index in 0..BENCHMARK_REQUESTS {
        let lba = (index as u64) % block_count;
        let request_start = monotonic_time_nanos();
        let request = device
            .axtest_read_sync(lba)
            .unwrap_or_else(|error| panic!("synchronous benchmark request failed: {error:?}"));
        assert_successful_read(&request, device.device_info().logical_block_size);
        latencies.push(monotonic_time_nanos().saturating_sub(request_start));
        completed_requests += 1;
        drop(request);
    }
    let elapsed_ns = monotonic_time_nanos().saturating_sub(start).max(1);
    BenchmarkResult {
        requests: BENCHMARK_REQUESTS,
        concurrency: 1,
        elapsed_ns,
        completed_requests,
        batch: block_batch_delta(batch_before, block_batch_snapshot()),
        cpu: cpu_runtime_delta(cpu_before, cpu_runtime_snapshot()),
        progress_polls: 0,
        latency: summarize_latencies(&mut latencies),
    }
}

fn run_sync_benchmark_rounds(
    device: &Arc<BlockDeviceHandle>,
    block_count: u64,
) -> BenchmarkResult {
    median_elapsed_benchmark_result(
        (0..BENCHMARK_ROUNDS)
            .map(|_| run_sync_benchmark(device, block_count))
            .collect(),
    )
}

type ReadFuture = Pin<Box<dyn Future<Output = Result<CompletedRequest, BlockError>>>>;

async fn run_async_benchmark(
    device: Arc<BlockDeviceHandle>,
    block_count: u64,
    concurrency: usize,
) -> BenchmarkResult {
    assert!(
        concurrency != 0,
        "asynchronous benchmark concurrency must be non-zero"
    );
    let batch_before = block_batch_snapshot();
    let cpu_before = cpu_runtime_snapshot();
    let start = monotonic_time_nanos();
    let mut completed_requests = 0_u64;
    let mut progress_polls = 0;
    let mut latencies = Vec::with_capacity(BENCHMARK_REQUESTS);
    let mut next_request = 0;
    while next_request < BENCHMARK_REQUESTS {
        let wave_end = next_request
            .saturating_add(concurrency)
            .min(BENCHMARK_REQUESTS);
        let mut futures: Vec<Option<(u64, ReadFuture)>> =
            Vec::with_capacity(wave_end - next_request);
        for index in next_request..wave_end {
            let lba = (index as u64) % block_count;
            let device = Arc::clone(&device);
            futures.push(Some((
                monotonic_time_nanos(),
                Box::pin(async move { device.axtest_read(lba).await }) as ReadFuture,
            )));
        }
        poll_fn(|cx| {
            let mut pending = false;
            for slot in &mut futures {
                let poll = slot.as_mut().map(|(_, future)| future.as_mut().poll(cx));
                match poll {
                    Some(Poll::Ready(result)) => {
                        let (request_start, _) =
                            slot.take().expect("ready request must still be owned");
                        let request = result.unwrap_or_else(|error| {
                            panic!("asynchronous benchmark request failed: {error:?}")
                        });
                        assert_successful_read(&request, device.device_info().logical_block_size);
                        latencies.push(monotonic_time_nanos().saturating_sub(request_start));
                        drop(request);
                        completed_requests += 1;
                    }
                    Some(Poll::Pending) => pending = true,
                    None => {}
                }
            }
            if pending {
                progress_polls += 1;
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
        next_request = wave_end;
    }
    let elapsed_ns = monotonic_time_nanos().saturating_sub(start).max(1);
    BenchmarkResult {
        requests: BENCHMARK_REQUESTS,
        concurrency,
        elapsed_ns,
        completed_requests,
        batch: block_batch_delta(batch_before, block_batch_snapshot()),
        cpu: cpu_runtime_delta(cpu_before, cpu_runtime_snapshot()),
        progress_polls,
        latency: summarize_latencies(&mut latencies),
    }
}

async fn run_async_benchmark_rounds(
    device: &Arc<BlockDeviceHandle>,
    block_count: u64,
    concurrency: usize,
) -> BenchmarkResult {
    let mut rounds = Vec::with_capacity(BENCHMARK_ROUNDS);
    for _ in 0..BENCHMARK_ROUNDS {
        rounds.push(
            run_async_benchmark(Arc::clone(device), block_count, concurrency).await,
        );
    }
    median_elapsed_benchmark_result(rounds)
}

#[cfg(feature = "block-runtime-write-tests")]
type ReadWriteFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CompletedRequest, BlockError>> + 'a>>;

#[cfg(feature = "block-runtime-write-tests")]
#[derive(Clone, Copy)]
enum ReadWriteOperation {
    Read { slot: usize },
    Write,
}

#[cfg(feature = "block-runtime-write-tests")]
fn benchmark_patterns(
    block_size: usize,
    blocks_per_request: u32,
    slot_count: usize,
    seed: u8,
) -> Vec<Vec<u8>> {
    (0..slot_count)
        .map(|slot| {
            pattern_bytes(
                block_size,
                blocks_per_request,
                seed.wrapping_add((slot as u8).wrapping_mul(17)),
            )
        })
        .collect()
}

#[cfg(feature = "block-runtime-write-tests")]
fn benchmark_slot_lba(base_lba: u64, slot: usize, blocks_per_request: u32) -> u64 {
    base_lba + (slot as u64).saturating_mul(u64::from(blocks_per_request))
}

#[cfg(feature = "block-runtime-write-tests")]
fn assert_read_matches(request: &CompletedRequest, expected: &[u8]) {
    assert_successful_read_bytes(request, expected.len());
    let data = request
        .data
        .as_ref()
        .expect("successful benchmark read must return DMA data");
    let mut actual = vec![0; expected.len()];
    data.copy_to_slice_cpu(&mut actual);
    assert_eq!(actual, expected);
}

#[cfg(feature = "block-runtime-write-tests")]
fn run_sync_multi_block_read_write_benchmark(
    device: &Arc<BlockDeviceHandle>,
    base_lba: u64,
    blocks_per_request: u32,
    patterns: &[Vec<u8>],
) -> ReadWriteBenchmarkResult {
    assert!(!patterns.is_empty(), "multi-block benchmark requires slots");
    let request_bytes = patterns[0].len() as u64;
    let batch_before = block_batch_snapshot();
    let cpu_before = cpu_runtime_snapshot();
    let start = monotonic_time_nanos();
    let mut write_latencies = Vec::with_capacity(BENCHMARK_REQUESTS);
    let mut read_latencies = Vec::with_capacity(BENCHMARK_REQUESTS);

    for index in 0..BENCHMARK_REQUESTS {
        let slot = index % patterns.len();
        let request_start = monotonic_time_nanos();
        let request = device
            .axtest_write_blocks_sync(
                benchmark_slot_lba(base_lba, slot, blocks_per_request),
                blocks_per_request,
                &patterns[slot],
            )
            .unwrap_or_else(|error| panic!("synchronous multi-block write failed: {error:?}"));
        assert_successful_write(&request);
        write_latencies.push(monotonic_time_nanos().saturating_sub(request_start));
    }
    for index in 0..BENCHMARK_REQUESTS {
        let slot = index % patterns.len();
        let request_start = monotonic_time_nanos();
        let request = device
            .axtest_read_blocks_sync(
                benchmark_slot_lba(base_lba, slot, blocks_per_request),
                blocks_per_request,
            )
            .unwrap_or_else(|error| panic!("synchronous multi-block read failed: {error:?}"));
        assert_read_matches(&request, &patterns[slot]);
        read_latencies.push(monotonic_time_nanos().saturating_sub(request_start));
    }

    let elapsed_ns = monotonic_time_nanos().saturating_sub(start).max(1);
    ReadWriteBenchmarkResult {
        concurrency: 1,
        elapsed_ns,
        read: IoPhaseResult {
            requests: read_latencies.len() as u64,
            bytes: (read_latencies.len() as u64).saturating_mul(request_bytes),
            latency: summarize_latencies(&mut read_latencies),
        },
        write: IoPhaseResult {
            requests: write_latencies.len() as u64,
            bytes: (write_latencies.len() as u64).saturating_mul(request_bytes),
            latency: summarize_latencies(&mut write_latencies),
        },
        batch: block_batch_delta(batch_before, block_batch_snapshot()),
        cpu: cpu_runtime_delta(cpu_before, cpu_runtime_snapshot()),
        progress_polls: 0,
    }
}

#[cfg(feature = "block-runtime-write-tests")]
async fn run_async_multi_block_read_write_benchmark(
    device: Arc<BlockDeviceHandle>,
    base_lba: u64,
    blocks_per_request: u32,
    patterns: &[Vec<u8>],
    concurrency: usize,
) -> ReadWriteBenchmarkResult {
    assert!(!patterns.is_empty(), "multi-block benchmark requires slots");
    assert!(
        concurrency != 0 && concurrency <= patterns.len(),
        "multi-block benchmark concurrency must fit distinct slots"
    );
    let request_bytes = patterns[0].len() as u64;
    let batch_before = block_batch_snapshot();
    let cpu_before = cpu_runtime_snapshot();
    let start = monotonic_time_nanos();
    let mut write_latencies = Vec::with_capacity(BENCHMARK_REQUESTS);
    let mut read_latencies = Vec::with_capacity(BENCHMARK_REQUESTS);
    let mut progress_polls = 0_u64;

    let mut next_request = 0;
    while next_request < BENCHMARK_REQUESTS {
        let wave_end = next_request
            .saturating_add(concurrency)
            .min(BENCHMARK_REQUESTS);
        let mut futures: Vec<Option<(u64, ReadWriteFuture<'_>)>> =
            Vec::with_capacity(wave_end - next_request);
        for index in next_request..wave_end {
            let slot = index % patterns.len();
            let target_lba = benchmark_slot_lba(base_lba, slot, blocks_per_request);
            let pattern = &patterns[slot];
            let future_device = Arc::clone(&device);
            futures.push(Some((
                monotonic_time_nanos(),
                Box::pin(async move {
                    future_device
                        .axtest_write_blocks(target_lba, blocks_per_request, pattern)
                        .await
                }),
            )));
        }
        poll_fn(|cx| {
            let mut pending = false;
            for slot in &mut futures {
                match slot.as_mut().map(|(_, future)| future.as_mut().poll(cx)) {
                    Some(Poll::Ready(result)) => {
                        let (request_start, _) = slot
                            .take()
                            .expect("ready multi-block write must remain owned");
                        let request = result.unwrap_or_else(|error| {
                            panic!("asynchronous multi-block write failed: {error:?}")
                        });
                        assert_successful_write(&request);
                        write_latencies
                            .push(monotonic_time_nanos().saturating_sub(request_start));
                    }
                    Some(Poll::Pending) => pending = true,
                    None => {}
                }
            }
            if pending {
                progress_polls += 1;
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
        next_request = wave_end;
    }

    let mut next_request = 0;
    while next_request < BENCHMARK_REQUESTS {
        let wave_end = next_request
            .saturating_add(concurrency)
            .min(BENCHMARK_REQUESTS);
        let mut futures: Vec<Option<(u64, usize, ReadWriteFuture<'_>)>> =
            Vec::with_capacity(wave_end - next_request);
        for index in next_request..wave_end {
            let slot = index % patterns.len();
            let target_lba = benchmark_slot_lba(base_lba, slot, blocks_per_request);
            let future_device = Arc::clone(&device);
            futures.push(Some((
                monotonic_time_nanos(),
                slot,
                Box::pin(async move {
                    future_device
                        .axtest_read_blocks(target_lba, blocks_per_request)
                        .await
                }),
            )));
        }
        poll_fn(|cx| {
            let mut pending = false;
            for slot in &mut futures {
                match slot
                    .as_mut()
                    .map(|(_, _, future)| future.as_mut().poll(cx))
                {
                    Some(Poll::Ready(result)) => {
                        let (request_start, pattern_slot, _) = slot
                            .take()
                            .expect("ready multi-block read must remain owned");
                        let request = result.unwrap_or_else(|error| {
                            panic!("asynchronous multi-block read failed: {error:?}")
                        });
                        assert_read_matches(&request, &patterns[pattern_slot]);
                        read_latencies
                            .push(monotonic_time_nanos().saturating_sub(request_start));
                    }
                    Some(Poll::Pending) => pending = true,
                    None => {}
                }
            }
            if pending {
                progress_polls += 1;
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
        next_request = wave_end;
    }

    let elapsed_ns = monotonic_time_nanos().saturating_sub(start).max(1);
    ReadWriteBenchmarkResult {
        concurrency,
        elapsed_ns,
        read: IoPhaseResult {
            requests: read_latencies.len() as u64,
            bytes: (read_latencies.len() as u64).saturating_mul(request_bytes),
            latency: summarize_latencies(&mut read_latencies),
        },
        write: IoPhaseResult {
            requests: write_latencies.len() as u64,
            bytes: (write_latencies.len() as u64).saturating_mul(request_bytes),
            latency: summarize_latencies(&mut write_latencies),
        },
        batch: block_batch_delta(batch_before, block_batch_snapshot()),
        cpu: cpu_runtime_delta(cpu_before, cpu_runtime_snapshot()),
        progress_polls,
    }
}

#[cfg(feature = "block-runtime-write-tests")]
fn run_sync_mixed_read_write_benchmark(
    device: &Arc<BlockDeviceHandle>,
    base_lba: u64,
    blocks_per_request: u32,
    original: &[u8],
    write_patterns: &[Vec<u8>],
) -> ReadWriteBenchmarkResult {
    let request_bytes = blocks_per_request as usize * device.device_info().logical_block_size;
    assert_eq!(original.len() % request_bytes, 0);
    let slot_count = original.len() / request_bytes;
    assert_eq!(slot_count % 2, 0);
    assert_eq!(write_patterns.len(), slot_count / 2);

    let batch_before = block_batch_snapshot();
    let cpu_before = cpu_runtime_snapshot();
    let start = monotonic_time_nanos();
    let mut read_latencies = Vec::with_capacity(BENCHMARK_REQUESTS / 2);
    let mut write_latencies = Vec::with_capacity(BENCHMARK_REQUESTS / 2);

    for index in 0..BENCHMARK_REQUESTS {
        let slot = index % slot_count;
        let target_lba = benchmark_slot_lba(base_lba, slot, blocks_per_request);
        let request_start = monotonic_time_nanos();
        if slot % 2 == 0 {
            let pattern = &write_patterns[slot / 2];
            let request = device
                .axtest_write_blocks_sync(target_lba, blocks_per_request, pattern)
                .unwrap_or_else(|error| panic!("synchronous mixed write failed: {error:?}"));
            assert_successful_write(&request);
            write_latencies.push(monotonic_time_nanos().saturating_sub(request_start));
        } else {
            let offset = slot * request_bytes;
            let request = device
                .axtest_read_blocks_sync(target_lba, blocks_per_request)
                .unwrap_or_else(|error| panic!("synchronous mixed read failed: {error:?}"));
            assert_read_matches(&request, &original[offset..offset + request_bytes]);
            read_latencies.push(monotonic_time_nanos().saturating_sub(request_start));
        }
    }

    let elapsed_ns = monotonic_time_nanos().saturating_sub(start).max(1);
    ReadWriteBenchmarkResult {
        concurrency: 1,
        elapsed_ns,
        read: IoPhaseResult {
            requests: read_latencies.len() as u64,
            bytes: (read_latencies.len() as u64).saturating_mul(request_bytes as u64),
            latency: summarize_latencies(&mut read_latencies),
        },
        write: IoPhaseResult {
            requests: write_latencies.len() as u64,
            bytes: (write_latencies.len() as u64).saturating_mul(request_bytes as u64),
            latency: summarize_latencies(&mut write_latencies),
        },
        batch: block_batch_delta(batch_before, block_batch_snapshot()),
        cpu: cpu_runtime_delta(cpu_before, cpu_runtime_snapshot()),
        progress_polls: 0,
    }
}

#[cfg(feature = "block-runtime-write-tests")]
async fn run_async_mixed_read_write_benchmark(
    device: Arc<BlockDeviceHandle>,
    base_lba: u64,
    blocks_per_request: u32,
    original: &[u8],
    write_patterns: &[Vec<u8>],
    concurrency: usize,
) -> ReadWriteBenchmarkResult {
    let request_bytes = blocks_per_request as usize * device.device_info().logical_block_size;
    assert_eq!(original.len() % request_bytes, 0);
    let slot_count = original.len() / request_bytes;
    assert_eq!(slot_count % 2, 0);
    assert_eq!(write_patterns.len(), slot_count / 2);
    assert!(
        concurrency != 0 && concurrency <= slot_count,
        "mixed benchmark concurrency must fit distinct slots"
    );

    let batch_before = block_batch_snapshot();
    let cpu_before = cpu_runtime_snapshot();
    let start = monotonic_time_nanos();
    let mut read_latencies = Vec::with_capacity(BENCHMARK_REQUESTS / 2);
    let mut write_latencies = Vec::with_capacity(BENCHMARK_REQUESTS / 2);
    let mut progress_polls = 0_u64;
    let mut next_request = 0;

    while next_request < BENCHMARK_REQUESTS {
        let wave_end = next_request
            .saturating_add(concurrency)
            .min(BENCHMARK_REQUESTS);
        let mut futures: Vec<
            Option<(u64, ReadWriteOperation, ReadWriteFuture<'_>)>,
        > = Vec::with_capacity(wave_end - next_request);
        for index in next_request..wave_end {
            let slot = index % slot_count;
            let target_lba = benchmark_slot_lba(base_lba, slot, blocks_per_request);
            let future_device = Arc::clone(&device);
            if slot % 2 == 0 {
                let pattern = &write_patterns[slot / 2];
                futures.push(Some((
                    monotonic_time_nanos(),
                    ReadWriteOperation::Write,
                    Box::pin(async move {
                        future_device
                            .axtest_write_blocks(target_lba, blocks_per_request, pattern)
                            .await
                    }),
                )));
            } else {
                futures.push(Some((
                    monotonic_time_nanos(),
                    ReadWriteOperation::Read { slot },
                    Box::pin(async move {
                        future_device
                            .axtest_read_blocks(target_lba, blocks_per_request)
                            .await
                    }),
                )));
            }
        }
        poll_fn(|cx| {
            let mut pending = false;
            for slot in &mut futures {
                match slot
                    .as_mut()
                    .map(|(_, _, future)| future.as_mut().poll(cx))
                {
                    Some(Poll::Ready(result)) => {
                        let (request_start, operation, _) = slot
                            .take()
                            .expect("ready mixed benchmark request must remain owned");
                        let request = result.unwrap_or_else(|error| {
                            panic!("asynchronous mixed request failed: {error:?}")
                        });
                        match operation {
                            ReadWriteOperation::Read { slot } => {
                                let offset = slot * request_bytes;
                                assert_read_matches(
                                    &request,
                                    &original[offset..offset + request_bytes],
                                );
                                read_latencies.push(
                                    monotonic_time_nanos().saturating_sub(request_start),
                                );
                            }
                            ReadWriteOperation::Write => {
                                assert_successful_write(&request);
                                write_latencies.push(
                                    monotonic_time_nanos().saturating_sub(request_start),
                                );
                            }
                        }
                    }
                    Some(Poll::Pending) => pending = true,
                    None => {}
                }
            }
            if pending {
                progress_polls += 1;
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
        next_request = wave_end;
    }

    let elapsed_ns = monotonic_time_nanos().saturating_sub(start).max(1);
    ReadWriteBenchmarkResult {
        concurrency,
        elapsed_ns,
        read: IoPhaseResult {
            requests: read_latencies.len() as u64,
            bytes: (read_latencies.len() as u64).saturating_mul(request_bytes as u64),
            latency: summarize_latencies(&mut read_latencies),
        },
        write: IoPhaseResult {
            requests: write_latencies.len() as u64,
            bytes: (write_latencies.len() as u64).saturating_mul(request_bytes as u64),
            latency: summarize_latencies(&mut write_latencies),
        },
        batch: block_batch_delta(batch_before, block_batch_snapshot()),
        cpu: cpu_runtime_delta(cpu_before, cpu_runtime_snapshot()),
        progress_polls,
    }
}

#[cfg(feature = "block-runtime-write-tests")]
fn assert_mixed_region_matches(
    device: &Arc<BlockDeviceHandle>,
    base_lba: u64,
    region_blocks: u32,
    blocks_per_request: u32,
    original: &[u8],
    write_patterns: &[Vec<u8>],
) {
    let actual = read_region_sync(device, base_lba, region_blocks);
    let request_bytes = blocks_per_request as usize * device.device_info().logical_block_size;
    let slot_count = actual.len() / request_bytes;
    for slot in 0..slot_count {
        let offset = slot * request_bytes;
        let expected = if slot % 2 == 0 {
            write_patterns[slot / 2].as_slice()
        } else {
            &original[offset..offset + request_bytes]
        };
        assert_eq!(&actual[offset..offset + request_bytes], expected);
    }
}

#[cfg(feature = "block-runtime-write-tests")]
fn assert_read_write_benchmark_complete(
    result: ReadWriteBenchmarkResult,
    expected_read_requests: u64,
    expected_write_requests: u64,
) {
    assert_eq!(result.read.requests, expected_read_requests);
    assert_eq!(result.write.requests, expected_write_requests);
    assert_eq!(result.read.latency.samples, expected_read_requests);
    assert_eq!(result.write.latency.samples, expected_write_requests);
    assert_eq!(
        result.batch.submitted_requests,
        expected_read_requests.saturating_add(expected_write_requests)
    );
    assert_eq!(
        result.batch.completed_requests,
        result.batch.submitted_requests
    );
    assert_eq!(result.batch.failed_requests, 0);
    assert_eq!(result.batch.commit_failures, 0);
}

/// Confirms registration readiness and consistency between completion paths.
#[axtest::axtest]
fn block_runtime_registration_and_read_consistency() {
    let batch_before = block_batch_snapshot();
    let devices = BlockDeviceHandle::axtest_devices()
        .expect("block runtime must be installed for block axtest");
    assert!(!devices.is_empty(), "block runtime must register a device");
    let device = devices
        .first()
        .cloned()
        .expect("block runtime device list must contain the registered device");
    let info = device.device_info();
    assert!(
        info.num_blocks != 0,
        "registered block device must have blocks"
    );
    assert!(
        info.logical_block_size != 0,
        "registered block device must expose a logical block size"
    );
    axtest::axtest_println!(
        "BLOCK_DEVICE name={} blocks={} logical_block_size={}",
        device.name(),
        info.num_blocks,
        info.logical_block_size,
    );

    let synchronous = device
        .axtest_read_sync(0)
        .unwrap_or_else(|error| panic!("registered device synchronous read failed: {error:?}"));
    assert_successful_read(&synchronous, info.logical_block_size);
    let synchronous_data = synchronous
        .data
        .expect("successful synchronous read must return DMA data")
        .into_cpu_buffer();

    let asynchronous = crate::task::future::block_on(device.axtest_read(0))
        .unwrap_or_else(|error| panic!("registered device asynchronous read failed: {error:?}"));
    assert_successful_read(&asynchronous, info.logical_block_size);
    let asynchronous_data = asynchronous
        .data
        .expect("successful asynchronous read must return DMA data")
        .into_cpu_buffer();
    assert_eq!(
        synchronous_data.as_slice_cpu(),
        asynchronous_data.as_slice_cpu(),
        "synchronous and asynchronous reads of one LBA must agree"
    );

    let batch = block_batch_delta(batch_before, block_batch_snapshot());
    assert_eq!(batch.submitted_requests, 2);
    assert_eq!(batch.completed_requests, 2);
    assert_eq!(batch.failed_requests, 0);
    assert_eq!(batch.commit_calls, 2);
    assert_eq!(batch.commit_failures, 0);
}

/// Drives two independent block requests from one task. This exercises
/// submission, IRQ completion and per-request waker delivery through the real
/// Starry runtime without relying on a synthetic pending poll.
#[axtest::axtest]
fn block_runtime_async_double_read() {
    let batch_before = block_batch_snapshot();
    let devices = BlockDeviceHandle::axtest_devices()
        .expect("block runtime must be installed for block axtest");
    let device = devices
        .first()
        .cloned()
        .expect("block axtest requires an installed block device");
    let info = device.device_info();
    assert!(
        info.num_blocks >= 2,
        "block axtest requires at least two blocks"
    );
    let block_size = info.logical_block_size;
    let mut first = pin!(read_one(Arc::clone(&device), 0));
    let mut second = pin!(read_one(device, 1));
    let mut first_result = None;
    let mut second_result = None;
    let (first_result, second_result) = crate::task::future::block_on(poll_fn(|cx| {
        if first_result.is_none() {
            match first.as_mut().poll(cx) {
                Poll::Ready(result) => {
                    first_result = Some(result);
                }
                Poll::Pending => {}
            }
        }
        if second_result.is_none() {
            match second.as_mut().poll(cx) {
                Poll::Ready(result) => {
                    second_result = Some(result);
                }
                Poll::Pending => {}
            }
        }
        match (first_result.take(), second_result.take()) {
            (Some(first), Some(second)) => Poll::Ready((first, second)),
            (first, second) => {
                first_result = first;
                second_result = second;
                Poll::Pending
            }
        }
    }));

    let first = first_result.expect("first block request result");
    let second = second_result.expect("second block request result");
    assert_successful_read(&first, block_size);
    assert_successful_read(&second, block_size);
    let mut first_data = first.data.expect("first completed DMA").into_cpu_buffer();
    let second_data = second.data.expect("second completed DMA").into_cpu_buffer();
    let second_bytes = second_data.as_slice_cpu().to_vec();
    let mut replacement = second_bytes.clone();
    replacement[0] ^= u8::MAX;
    // Completion transfers each buffer back to the CPU. Independent requests
    // must not alias these independently retained ownership objects.
    first_data.copy_from_slice_cpu(&replacement);
    assert_eq!(first_data.as_slice_cpu(), replacement);
    assert_eq!(second_data.as_slice_cpu(), second_bytes);

    let batch = block_batch_delta(batch_before, block_batch_snapshot());
    assert_eq!(batch.submitted_requests, 2);
    assert_eq!(batch.completed_requests, 2);
    assert_eq!(batch.failed_requests, 0);
    // `commit_calls` counts runtime-to-driver batches, not requests. The two
    // submissions overlap in flight, so a queue with multi-request submit
    // batches may legitimately coalesce them into one commit; only require
    // that every commit succeeded.
    assert_eq!(batch.commit_failures, 0);
}

/// Validates a consecutive sequence of single-block writes and reads. The
/// original bytes are restored before the test completes so the scratch disk
/// remains reusable for the next case.
#[cfg(feature = "block-runtime-write-tests")]
#[axtest::axtest]
fn block_runtime_single_block_contiguous_read_write() {
    let device = writable_test_device();
    let info = device.device_info();
    let lba = SCRATCH_TEST_START_LBA;
    let original = read_region_sync(&device, lba, WRITE_TEST_BLOCKS);
    let mut scratch =
        ScratchRegionGuard::new(Arc::clone(&device), lba, WRITE_TEST_BLOCKS, original);
    let pattern = pattern_bytes(info.logical_block_size, WRITE_TEST_BLOCKS, 0x31);
    let batch_before = block_batch_snapshot();
    let start = monotonic_time_nanos();

    for index in 0..WRITE_TEST_BLOCKS {
        let offset = index as usize * info.logical_block_size;
        let request = device
            .axtest_write_sync(
                lba + u64::from(index),
                &pattern[offset..offset + info.logical_block_size],
            )
            .unwrap_or_else(|error| panic!("single-block synchronous write failed: {error:?}"));
        assert_successful_write(&request);
    }
    for index in 0..WRITE_TEST_BLOCKS {
        let offset = index as usize * info.logical_block_size;
        let request = crate::task::future::block_on(device.axtest_read(lba + u64::from(index)))
            .unwrap_or_else(|error| panic!("single-block asynchronous read failed: {error:?}"));
        assert_successful_read(&request, info.logical_block_size);
        let data = request
            .data
            .as_ref()
            .expect("single-block read must return DMA data");
        let mut bytes = vec![0; info.logical_block_size];
        data.copy_to_slice_cpu(&mut bytes);
        assert_eq!(bytes, &pattern[offset..offset + info.logical_block_size]);
    }

    let elapsed_ns = monotonic_time_nanos().saturating_sub(start).max(1);
    let batch = block_batch_delta(batch_before, block_batch_snapshot());
    assert_eq!(batch.submitted_requests, u64::from(WRITE_TEST_BLOCKS * 2));
    assert_eq!(batch.completed_requests, batch.submitted_requests);
    assert_eq!(batch.failed_requests, 0);
    assert_eq!(batch.commit_failures, 0);

    scratch.restore();

    axtest::axtest_println!(
        "BLOCK_SINGLE_RW_FUNCTIONAL lba={} blocks={} bytes={} elapsed_ns={} bytes_per_sec={} \
         submitted={} completed={}",
        lba,
        WRITE_TEST_BLOCKS,
        pattern.len(),
        elapsed_ns,
        rate_per_second((pattern.len() as u64).saturating_mul(2), elapsed_ns),
        batch.submitted_requests,
        batch.completed_requests,
    );
}

#[cfg(feature = "block-runtime-write-tests")]
fn run_multi_block_functional_test(
    device: Arc<BlockDeviceHandle>,
    lba: u64,
    block_count: u32,
    seed: u8,
    label: &str,
) {
    let info = device.device_info();
    let original = read_region_sync(&device, lba, block_count);
    let mut scratch = ScratchRegionGuard::new(Arc::clone(&device), lba, block_count, original);
    let pattern = pattern_bytes(info.logical_block_size, block_count, seed);
    let batch_before = block_batch_snapshot();
    let start = monotonic_time_nanos();

    let write =
        crate::task::future::block_on(device.axtest_write_blocks(lba, block_count, &pattern))
            .unwrap_or_else(|error| panic!("multi-block asynchronous write failed: {error:?}"));
    assert_successful_write(&write);
    let read = crate::task::future::block_on(device.axtest_read_blocks(lba, block_count))
        .unwrap_or_else(|error| panic!("multi-block asynchronous read failed: {error:?}"));
    assert_successful_read_bytes(&read, pattern.len());
    let data = read
        .data
        .as_ref()
        .expect("multi-block read must return DMA data");
    let mut read_back = vec![0; pattern.len()];
    data.copy_to_slice_cpu(&mut read_back);
    assert_eq!(read_back, pattern);

    let elapsed_ns = monotonic_time_nanos().saturating_sub(start).max(1);
    let batch = block_batch_delta(batch_before, block_batch_snapshot());
    assert_eq!(batch.submitted_requests, 2);
    assert_eq!(batch.completed_requests, 2);
    assert_eq!(batch.failed_requests, 0);
    assert_eq!(batch.commit_failures, 0);

    scratch.restore();

    axtest::axtest_println!(
        "{label} lba={} blocks={} bytes={} elapsed_ns={} bytes_per_sec={} \
         submitted={} completed={}",
        lba,
        block_count,
        pattern.len(),
        elapsed_ns,
        rate_per_second((pattern.len() as u64).saturating_mul(2), elapsed_ns),
        batch.submitted_requests,
        batch.completed_requests,
    );
}

/// Validates a true multi-block request (`block_count > 1`) through both the
/// asynchronous write and asynchronous read completion paths, including
/// complete buffer contents and block boundaries.
#[cfg(feature = "block-runtime-write-tests")]
#[axtest::axtest]
fn block_runtime_multi_block_contiguous_read_write() {
    run_multi_block_functional_test(
        writable_test_device(),
        SCRATCH_TEST_START_LBA + u64::from(WRITE_TEST_BLOCKS),
        WRITE_TEST_BLOCKS,
        0x72,
        "BLOCK_MULTI_RW_FUNCTIONAL",
    );
}

/// Exercises a real chained IDMAC transfer on VisionFive 2. The kernel DMA
/// allocator returns 4 KiB-aligned buffers and each DW-MMC descriptor carries
/// at most 4 KiB, so this 8 KiB request requires two hardware descriptors.
#[cfg(feature = "block-runtime-visionfive2-reserved-region")]
#[axtest::axtest]
fn block_runtime_multi_descriptor_contiguous_read_write() {
    assert!(
        RW_BENCHMARK_REGION_BLOCKS + MULTI_DESCRIPTOR_TEST_BLOCKS <= SCRATCH_REGION_BLOCKS,
        "scratch region is too small for the multi-descriptor functional test"
    );
    run_multi_block_functional_test(
        writable_test_device(),
        SCRATCH_TEST_START_LBA + u64::from(RW_BENCHMARK_REGION_BLOCKS),
        MULTI_DESCRIPTOR_TEST_BLOCKS,
        0xC7,
        "BLOCK_MULTI_DESCRIPTOR_RW_FUNCTIONAL",
    );
}

/// Interleaves asynchronous writes and reads in one task. Reads target the
/// untouched odd blocks while writes target even blocks, so each completion
/// has an independent, deterministic expected result.
#[cfg(feature = "block-runtime-write-tests")]
#[axtest::axtest]
fn block_runtime_async_mixed_read_write() {
    type MixedFuture = Pin<Box<dyn Future<Output = Result<CompletedRequest, BlockError>>>>;

    let device = writable_test_device();
    let info = device.device_info();
    let lba = SCRATCH_TEST_START_LBA + u64::from(WRITE_TEST_BLOCKS * 2);
    let block_count = MIXED_TEST_BLOCKS;
    let original = read_region_sync(&device, lba, block_count);
    let mut scratch = ScratchRegionGuard::new(Arc::clone(&device), lba, block_count, original);
    let pattern = pattern_bytes(info.logical_block_size, block_count / 2, 0xA5);
    let batch_before = block_batch_snapshot();
    let start = monotonic_time_nanos();
    let mut futures: Vec<Option<(MixedExpectation, MixedFuture)>> = Vec::new();

    for index in 0..block_count {
        let target_lba = lba + u64::from(index);
        if index % 2 == 0 {
            let offset = (index as usize / 2) * info.logical_block_size;
            let block = pattern[offset..offset + info.logical_block_size].to_vec();
            let future_device = Arc::clone(&device);
            futures.push(Some((
                MixedExpectation::Write,
                Box::pin(async move { future_device.axtest_write(target_lba, &block).await })
                    as MixedFuture,
            )));
        } else {
            let offset = index as usize * info.logical_block_size;
            let expected = scratch.original[offset..offset + info.logical_block_size].to_vec();
            let future_device = Arc::clone(&device);
            futures.push(Some((
                MixedExpectation::Read(expected),
                Box::pin(async move { future_device.axtest_read(target_lba).await }) as MixedFuture,
            )));
        }
    }

    let mut mixed_ok = true;
    crate::task::future::block_on(poll_fn(|cx| {
        let mut pending = false;
        for slot in &mut futures {
            let poll = slot.as_mut().map(|(_, future)| future.as_mut().poll(cx));
            match poll {
                Some(Poll::Ready(Ok(request))) => {
                    let (expectation, _) = slot.take().expect("ready mixed request ownership");
                    match expectation {
                        MixedExpectation::Write => {
                            if request.result.is_err() {
                                mixed_ok = false;
                            }
                        }
                        MixedExpectation::Read(expected) => {
                            if request.result.is_err() {
                                mixed_ok = false;
                            } else if request.data.as_ref().map(|data| data.len().get())
                                != Some(info.logical_block_size)
                            {
                                mixed_ok = false;
                            } else {
                                let data = request.data.as_ref().expect("checked read data");
                                let mut bytes = vec![0; info.logical_block_size];
                                data.copy_to_slice_cpu(&mut bytes);
                                if bytes != expected {
                                    mixed_ok = false;
                                }
                            }
                        }
                    }
                }
                Some(Poll::Ready(Err(_))) => {
                    let _ = slot.take();
                    mixed_ok = false;
                }
                Some(Poll::Pending) => pending = true,
                None => {}
            }
        }
        if pending {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }));

    let elapsed_ns = monotonic_time_nanos().saturating_sub(start).max(1);
    let batch = block_batch_delta(batch_before, block_batch_snapshot());
    assert_eq!(batch.submitted_requests, u64::from(block_count));
    assert_eq!(batch.completed_requests, batch.submitted_requests);
    assert_eq!(batch.failed_requests, 0);
    assert_eq!(batch.commit_failures, 0);

    let mixed_read_back = read_region_sync(&device, lba, block_count);
    for index in 0..block_count as usize {
        let offset = index * info.logical_block_size;
        let expected = if index % 2 == 0 {
            let pattern_offset = (index / 2) * info.logical_block_size;
            &pattern[pattern_offset..pattern_offset + info.logical_block_size]
        } else {
            &scratch.original[offset..offset + info.logical_block_size]
        };
        if mixed_read_back[offset..offset + info.logical_block_size] != expected[..] {
            mixed_ok = false;
        }
    }

    scratch.restore();
    assert!(
        mixed_ok,
        "mixed read/write completion or data validation failed"
    );

    axtest::axtest_println!(
        "BLOCK_MIXED_RW_FUNCTIONAL lba={} blocks={} bytes={} elapsed_ns={} bytes_per_sec={} \
         submitted={} completed={}",
        lba,
        block_count,
        mixed_read_back.len(),
        elapsed_ns,
        rate_per_second(mixed_read_back.len() as u64, elapsed_ns),
        batch.submitted_requests,
        batch.completed_requests,
    );
}

/// Compares synchronous and asynchronous multi-block write/read workloads on
/// the same scratch slots. Every slot uses a stable pattern, so repeated
/// requests remain verifiable while each asynchronous wave targets distinct
/// ranges. Results are measurements rather than pass/fail performance gates.
#[cfg(feature = "block-runtime-write-tests")]
#[axtest::axtest]
fn block_runtime_multi_block_read_write_benchmark() {
    assert!(
        SCRATCH_REGION_BLOCKS >= RW_BENCHMARK_REGION_BLOCKS,
        "scratch region is too small for the read/write benchmark"
    );
    assert_eq!(
        RW_BENCHMARK_REGION_BLOCKS % MULTI_RW_BLOCKS_PER_REQUEST,
        0
    );

    let device = writable_test_device();
    let info = device.device_info();
    let base_lba = SCRATCH_TEST_START_LBA;
    let slot_count =
        (RW_BENCHMARK_REGION_BLOCKS / MULTI_RW_BLOCKS_PER_REQUEST) as usize;
    let patterns = benchmark_patterns(
        info.logical_block_size,
        MULTI_RW_BLOCKS_PER_REQUEST,
        slot_count,
        0x43,
    );
    let original = read_region_sync(&device, base_lba, RW_BENCHMARK_REGION_BLOCKS);

    let mut synchronous_guard = ScratchRegionGuard::new(
        Arc::clone(&device),
        base_lba,
        RW_BENCHMARK_REGION_BLOCKS,
        original.clone(),
    );
    let synchronous = median_elapsed_read_write_result(
        (0..BENCHMARK_ROUNDS)
            .map(|_| {
                run_sync_multi_block_read_write_benchmark(
                    &device,
                    base_lba,
                    MULTI_RW_BLOCKS_PER_REQUEST,
                    &patterns,
                )
            })
            .collect(),
    );
    assert_read_write_benchmark_complete(
        synchronous,
        BENCHMARK_REQUESTS as u64,
        BENCHMARK_REQUESTS as u64,
    );
    synchronous_guard.restore();

    let mut asynchronous_guard = ScratchRegionGuard::new(
        Arc::clone(&device),
        base_lba,
        RW_BENCHMARK_REGION_BLOCKS,
        original,
    );
    let asynchronous = crate::task::future::block_on(async {
        let mut rounds = Vec::with_capacity(BENCHMARK_ROUNDS);
        for _ in 0..BENCHMARK_ROUNDS {
            rounds.push(
                run_async_multi_block_read_write_benchmark(
                    Arc::clone(&device),
                    base_lba,
                    MULTI_RW_BLOCKS_PER_REQUEST,
                    &patterns,
                    MULTI_RW_ASYNC_CONCURRENCY,
                )
                .await,
            );
        }
        median_elapsed_read_write_result(rounds)
    });
    assert_read_write_benchmark_complete(
        asynchronous,
        BENCHMARK_REQUESTS as u64,
        BENCHMARK_REQUESTS as u64,
    );
    asynchronous_guard.restore();

    print_read_write_benchmark_result(
        "BLOCK_MULTI_SYNC_RW_BENCH",
        synchronous,
        MULTI_RW_BLOCKS_PER_REQUEST,
    );
    print_read_write_benchmark_result(
        "BLOCK_MULTI_ASYNC_RW_BENCH",
        asynchronous,
        MULTI_RW_BLOCKS_PER_REQUEST,
    );
    axtest::axtest_println!(
        "BLOCK_MULTI_RW_COMPARE blocks_per_request={} requests_per_direction={} benchmark_rounds={} \
         sync_elapsed_ns={} async_elapsed_ns={} async_speedup_permille={} \
         sync_cpu_runtime_ns={} async_cpu_runtime_ns={} sync_context_switches={} \
         async_context_switches={} async_progress_polls={} async_concurrency={}",
        MULTI_RW_BLOCKS_PER_REQUEST,
        BENCHMARK_REQUESTS,
        BENCHMARK_ROUNDS,
        synchronous.elapsed_ns,
        asynchronous.elapsed_ns,
        synchronous.elapsed_ns.saturating_mul(1_000) / asynchronous.elapsed_ns,
        synchronous.cpu.charged_runtime_ns,
        asynchronous.cpu.charged_runtime_ns,
        synchronous.cpu.context_switches,
        asynchronous.cpu.context_switches,
        asynchronous.progress_polls,
        MULTI_RW_ASYNC_CONCURRENCY,
    );
}

/// Compares a serialized alternating read/write workload with asynchronous
/// waves over disjoint multi-block slots. Reads observe untouched odd slots;
/// writes update even slots, allowing completion-order-independent validation.
#[cfg(feature = "block-runtime-write-tests")]
#[axtest::axtest]
fn block_runtime_mixed_read_write_benchmark() {
    assert!(
        SCRATCH_REGION_BLOCKS >= RW_BENCHMARK_REGION_BLOCKS,
        "scratch region is too small for the mixed benchmark"
    );
    assert_eq!(
        RW_BENCHMARK_REGION_BLOCKS % MIXED_RW_BLOCKS_PER_REQUEST,
        0
    );

    let device = writable_test_device();
    let info = device.device_info();
    let base_lba = SCRATCH_TEST_START_LBA;
    let slot_count =
        (RW_BENCHMARK_REGION_BLOCKS / MIXED_RW_BLOCKS_PER_REQUEST) as usize;
    assert_eq!(slot_count % 2, 0);
    let write_patterns = benchmark_patterns(
        info.logical_block_size,
        MIXED_RW_BLOCKS_PER_REQUEST,
        slot_count / 2,
        0x9B,
    );
    let original = read_region_sync(&device, base_lba, RW_BENCHMARK_REGION_BLOCKS);
    let expected_write_requests = (0..BENCHMARK_REQUESTS)
        .filter(|index| index % slot_count % 2 == 0)
        .count() as u64;
    let expected_read_requests =
        (BENCHMARK_REQUESTS as u64).saturating_sub(expected_write_requests);

    let mut synchronous_guard = ScratchRegionGuard::new(
        Arc::clone(&device),
        base_lba,
        RW_BENCHMARK_REGION_BLOCKS,
        original.clone(),
    );
    let synchronous = median_elapsed_read_write_result(
        (0..BENCHMARK_ROUNDS)
            .map(|_| {
                run_sync_mixed_read_write_benchmark(
                    &device,
                    base_lba,
                    MIXED_RW_BLOCKS_PER_REQUEST,
                    &original,
                    &write_patterns,
                )
            })
            .collect(),
    );
    assert_read_write_benchmark_complete(
        synchronous,
        expected_read_requests,
        expected_write_requests,
    );
    assert_mixed_region_matches(
        &device,
        base_lba,
        RW_BENCHMARK_REGION_BLOCKS,
        MIXED_RW_BLOCKS_PER_REQUEST,
        &original,
        &write_patterns,
    );
    synchronous_guard.restore();

    let mut asynchronous_guard = ScratchRegionGuard::new(
        Arc::clone(&device),
        base_lba,
        RW_BENCHMARK_REGION_BLOCKS,
        original.clone(),
    );
    let asynchronous = crate::task::future::block_on(async {
        let mut rounds = Vec::with_capacity(BENCHMARK_ROUNDS);
        for _ in 0..BENCHMARK_ROUNDS {
            rounds.push(
                run_async_mixed_read_write_benchmark(
                    Arc::clone(&device),
                    base_lba,
                    MIXED_RW_BLOCKS_PER_REQUEST,
                    &original,
                    &write_patterns,
                    ASYNC_CONCURRENCY,
                )
                .await,
            );
        }
        median_elapsed_read_write_result(rounds)
    });
    assert_read_write_benchmark_complete(
        asynchronous,
        expected_read_requests,
        expected_write_requests,
    );
    assert_mixed_region_matches(
        &device,
        base_lba,
        RW_BENCHMARK_REGION_BLOCKS,
        MIXED_RW_BLOCKS_PER_REQUEST,
        &original,
        &write_patterns,
    );
    asynchronous_guard.restore();

    print_read_write_benchmark_result(
        "BLOCK_MIXED_SYNC_RW_BENCH",
        synchronous,
        MIXED_RW_BLOCKS_PER_REQUEST,
    );
    print_read_write_benchmark_result(
        "BLOCK_MIXED_ASYNC_RW_BENCH",
        asynchronous,
        MIXED_RW_BLOCKS_PER_REQUEST,
    );
    axtest::axtest_println!(
        "BLOCK_MIXED_RW_COMPARE blocks_per_request={} read_requests={} write_requests={} benchmark_rounds={} \
         sync_elapsed_ns={} async_elapsed_ns={} async_speedup_permille={} \
         sync_cpu_runtime_ns={} async_cpu_runtime_ns={} sync_context_switches={} \
         async_context_switches={} async_progress_polls={} async_concurrency={}",
        MIXED_RW_BLOCKS_PER_REQUEST,
        expected_read_requests,
        expected_write_requests,
        BENCHMARK_ROUNDS,
        synchronous.elapsed_ns,
        asynchronous.elapsed_ns,
        synchronous.elapsed_ns.saturating_mul(1_000) / asynchronous.elapsed_ns,
        synchronous.cpu.charged_runtime_ns,
        asynchronous.cpu.charged_runtime_ns,
        synchronous.cpu.context_switches,
        asynchronous.cpu.context_switches,
        asynchronous.progress_polls,
        ASYNC_CONCURRENCY,
    );
}

/// Measures equivalent synchronous and asynchronous workloads on the real
/// block device. The test reports data instead of enforcing a device-specific
/// performance threshold: queue depth, firmware and media latency determine
/// absolute values, while correctness remains covered by the assertions.
#[axtest::axtest]
fn block_runtime_sync_async_benchmark() {
    let devices = BlockDeviceHandle::axtest_devices()
        .expect("block runtime must be installed for block axtest");
    let device = devices
        .first()
        .cloned()
        .expect("block axtest requires an installed block device");
    let info = device.device_info();
    assert!(
        info.num_blocks != 0,
        "block benchmark requires a non-empty block device"
    );
    axtest::axtest_println!(
        "BLOCK_DEVICE name={} blocks={} logical_block_size={}",
        device.name(),
        info.num_blocks,
        info.logical_block_size,
    );

    // Exclude one-time channel, DMA and IRQ setup from both measured paths.
    let sync_warmup = device
        .axtest_read_sync(0)
        .unwrap_or_else(|error| panic!("synchronous benchmark warmup failed: {error:?}"));
    assert_successful_read(&sync_warmup, info.logical_block_size);
    let async_warmup = crate::task::future::block_on(device.axtest_read(0))
        .unwrap_or_else(|error| panic!("asynchronous benchmark warmup failed: {error:?}"));
    assert_successful_read(&async_warmup, info.logical_block_size);

    let synchronous = run_sync_benchmark_rounds(&device, info.num_blocks);
    let asynchronous = ASYNC_CONCURRENCIES.map(|concurrency| {
        crate::task::future::block_on(run_async_benchmark_rounds(
            &device,
            info.num_blocks,
            concurrency,
        ))
    });
    let asynchronous_serial = asynchronous[0];
    let asynchronous_concurrent = asynchronous[ASYNC_CONCURRENCIES.len() - 1];

    assert_eq!(synchronous.completed_requests, BENCHMARK_REQUESTS as u64);
    for result in asynchronous.iter().copied() {
        assert_eq!(result.completed_requests, BENCHMARK_REQUESTS as u64);
        assert_eq!(result.latency.samples, BENCHMARK_REQUESTS as u64);
        assert_eq!(result.batch.failed_requests, 0);
        assert_eq!(result.batch.commit_failures, 0);
    }
    assert_eq!(synchronous.latency.samples, BENCHMARK_REQUESTS as u64);
    assert_eq!(synchronous.batch.failed_requests, 0);
    assert_eq!(synchronous.batch.commit_failures, 0);

    print_benchmark_result(
        "BLOCK_SYNC_SERIAL_BENCH",
        synchronous,
        info.logical_block_size,
    );
    print_benchmark_result(
        "BLOCK_ASYNC_SERIAL_BENCH",
        asynchronous_serial,
        info.logical_block_size,
    );
    print_benchmark_result(
        "BLOCK_ASYNC_CONCURRENT_BENCH",
        asynchronous_concurrent,
        info.logical_block_size,
    );
    for (concurrency, result) in ASYNC_CONCURRENCIES
        .iter()
        .copied()
        .zip(asynchronous.iter().copied())
    {
        axtest::axtest_println!(
            "BLOCK_ASYNC_QD_COMPARE concurrency={} elapsed_ns={} speedup_vs_sync_permille={} \
             bytes_per_sec={} p95_latency_ns={} p99_latency_ns={} cpu_runtime_ns={} \
             context_switches={} progress_polls={}",
            concurrency,
            result.elapsed_ns,
            synchronous.elapsed_ns.saturating_mul(1_000) / result.elapsed_ns,
            rate_per_second(
                result.completed_requests.saturating_mul(info.logical_block_size as u64),
                result.elapsed_ns,
            ),
            result.latency.p95_ns,
            result.latency.p99_ns,
            result.cpu.charged_runtime_ns,
            result.cpu.context_switches,
            result.progress_polls,
        );
    }
    axtest::axtest_println!(
        "BLOCK_BENCH_COMPARE benchmark_rounds={} sync_elapsed_ns={} async_serial_elapsed_ns={} \
         async_concurrent_elapsed_ns={} sync_cpu_runtime_ns={} async_serial_cpu_runtime_ns={} \
         async_concurrent_cpu_runtime_ns={} sync_context_switches={} \
         async_serial_context_switches={} async_concurrent_context_switches={} \
         async_serial_progress_polls={} async_concurrent_progress_polls={} async_concurrency={}",
        BENCHMARK_ROUNDS,
        synchronous.elapsed_ns,
        asynchronous_serial.elapsed_ns,
        asynchronous_concurrent.elapsed_ns,
        synchronous.cpu.charged_runtime_ns,
        asynchronous_serial.cpu.charged_runtime_ns,
        asynchronous_concurrent.cpu.charged_runtime_ns,
        synchronous.cpu.context_switches,
        asynchronous_serial.cpu.context_switches,
        asynchronous_concurrent.cpu.context_switches,
        asynchronous_serial.progress_polls,
        asynchronous_concurrent.progress_polls,
        ASYNC_CONCURRENCY,
    );
}

/// Fixed CPU work per request, calibrated at runtime against the measured
/// read latency. The accumulator is folded into the benchmark checksum so the
/// compiler cannot elide the loop.
#[inline(never)]
fn burn_cpu_cycles(spins: u32, seed: u64) -> u64 {
    let mut accumulator = seed;
    for _ in 0..spins {
        accumulator = accumulator
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
    }
    accumulator
}

/// Determines how many calibration spins approximate `duration_ns` of CPU
/// work. Timing loops on real hardware jitter, so this is a coarse target:
/// the overlap comparison only needs the compute phase to last long enough
/// to cover the device transfer time.
fn calibrate_cpu_spins(duration_ns: u64) -> u32 {
    let probe_spins: u32 = 100_000;
    let start = monotonic_time_nanos();
    // Keep the probe observable. Without an opaque use of the return value,
    // an optimized target is allowed to remove this pure computation and
    // make the calibration divide by a near-zero duration.
    let probe_checksum = core::hint::black_box(burn_cpu_cycles(probe_spins, 0));
    let probe_ns = monotonic_time_nanos().saturating_sub(start).max(1);
    core::hint::black_box(probe_checksum);
    u32::try_from(
        u64::from(probe_spins)
            .saturating_mul(duration_ns)
            .saturating_div(probe_ns),
    )
    .unwrap_or(u32::MAX)
    .max(1)
}

/// Demonstrates the core asynchronous advantage on a single-inflight device:
/// while the DMA transfer is in flight, the submitting task can run CPU work
/// instead of sleeping. The synchronous reference performs the same I/O and
/// the same CPU work strictly serialized, while the asynchronous path starts
/// each read, computes while the request is in flight, and harvests the
/// completion afterwards. Wall-clock totals for both phases are reported as
/// data; correctness of the overlap itself is not asserted, only the
/// functional correctness of every request.
#[axtest::axtest]
fn block_runtime_async_cpu_overlap_benchmark() {
    let devices = BlockDeviceHandle::axtest_devices()
        .expect("block runtime must be installed for block axtest");
    let device = devices
        .first()
        .cloned()
        .expect("block axtest requires an installed block device");
    let info = device.device_info();
    assert!(
        info.num_blocks != 0,
        "block benchmark requires a non-empty block device"
    );
    let block_count = info.num_blocks;

    // Warm both paths, then calibrate one compute chunk to roughly one read
    // latency: a chunk that matches the transfer window keeps the device busy
    // back-to-back while the CPU computes inside every pending window.
    let warmup_start = monotonic_time_nanos();
    let warmup = device
        .axtest_read_sync(0)
        .unwrap_or_else(|error| panic!("cpu overlap warmup failed: {error:?}"));
    assert_successful_read(&warmup, info.logical_block_size);
    drop(warmup);
    let read_latency_ns = monotonic_time_nanos().saturating_sub(warmup_start);
    let spins = calibrate_cpu_spins(read_latency_ns);

    let requests = BENCHMARK_REQUESTS;
    let mut sync_checksum = 0_u64;

    // Reference: identical I/O and CPU work, strictly serialized. The compute
    // seed matches the overlapped side's per-request seed so the checksums
    // share the same multiset of `burn_cpu_cycles` results.
    let batch_before = block_batch_snapshot();
    let cpu_before = cpu_runtime_snapshot();
    let sync_start = monotonic_time_nanos();
    for index in 0..requests {
        let lba = (index as u64) % block_count;
        let request = device
            .axtest_read_sync(lba)
            .unwrap_or_else(|error| panic!("serialized reference read failed: {error:?}"));
        assert_successful_read(&request, info.logical_block_size);
        drop(request);
        sync_checksum ^= burn_cpu_cycles(spins, index as u64 + 1);
    }
    let sync_elapsed_ns = monotonic_time_nanos().saturating_sub(sync_start).max(1);
    let sync_batch = block_batch_delta(batch_before, block_batch_snapshot());
    let sync_cpu = cpu_runtime_delta(cpu_before, cpu_runtime_snapshot());

    // Overlapped: submit, compute while the request is in flight, then reap.
    // The state lives outside the poll closure so it survives every wake-up.
    // Each request owns exactly one compute chunk of `spins` LCG iterations,
    // tracked by `chunk_remaining`/`chunk_state`. The first pending poll burns
    // that chunk inside the transfer window and increments the overlap count.
    // If a request is immediately ready, the ready branch burns the same chunk
    // after completion without counting it as overlapped work. The per-request
    // total and final checksum therefore stay identical to the serialized path.
    let batch_before = block_batch_snapshot();
    let cpu_before = cpu_runtime_snapshot();
    let async_start = monotonic_time_nanos();
    let mut pending_read =
        None::<Pin<Box<dyn Future<Output = Result<CompletedRequest, BlockError>>>>>;
    let mut completed = 0;
    let mut chunk_remaining: u32 = 0;
    let mut chunk_state = 0_u64;
    let mut async_checksum = 0_u64;
    let mut overlapped_compute_chunks = 0_u64;
    crate::task::future::block_on(poll_fn(|cx| {
        loop {
            if pending_read.is_none() {
                if completed < requests {
                    chunk_remaining = spins;
                    chunk_state = completed as u64 + 1;
                    let lba = (completed as u64) % block_count;
                    let submit_device = Arc::clone(&device);
                    pending_read = Some(Box::pin(
                        async move { submit_device.axtest_read(lba).await },
                    ));
                } else {
                    return Poll::Ready(());
                }
            }
            let Some(read_future) = pending_read.as_mut() else {
                unreachable!("pending_read is occupied until the loop returns");
            };
            match read_future.as_mut().poll(cx) {
                Poll::Ready(result) => {
                    pending_read = None;
                    let request = result.unwrap_or_else(|error| {
                        panic!("overlapped asynchronous read failed: {error:?}")
                    });
                    assert_successful_read(&request, info.logical_block_size);
                    drop(request);
                    // An immediately ready request had no overlap window, but
                    // still runs the identical per-request compute workload.
                    if chunk_remaining > 0 {
                        chunk_state = burn_cpu_cycles(chunk_remaining, chunk_state);
                        chunk_remaining = 0;
                    }
                    async_checksum ^= chunk_state;
                    completed += 1;
                }
                Poll::Pending => {
                    // The read is in flight: this is the overlap window.
                    if chunk_remaining > 0 {
                        chunk_state = burn_cpu_cycles(chunk_remaining, chunk_state);
                        chunk_remaining = 0;
                        overlapped_compute_chunks += 1;
                    }
                    return Poll::Pending;
                }
            }
        }
    }));
    let async_elapsed_ns = monotonic_time_nanos().saturating_sub(async_start).max(1);
    let async_batch = block_batch_delta(batch_before, block_batch_snapshot());
    let async_cpu = cpu_runtime_delta(cpu_before, cpu_runtime_snapshot());

    assert_eq!(sync_batch.submitted_requests, requests as u64);
    assert_eq!(sync_batch.completed_requests, requests as u64);
    assert_eq!(sync_batch.failed_requests, 0);
    assert_eq!(sync_batch.commit_failures, 0);
    assert_eq!(async_batch.submitted_requests, requests as u64);
    assert_eq!(async_batch.completed_requests, requests as u64);
    assert_eq!(async_batch.failed_requests, 0);
    assert_eq!(async_batch.commit_failures, 0);
    assert_eq!(sync_checksum, async_checksum);

    axtest::axtest_println!(
        "BLOCK_CPU_OVERLAP_BENCH requests={} spins={} read_latency_ns={} serialized_elapsed_ns={} \
         overlapped_elapsed_ns={} serialized_bytes_per_sec={} overlapped_bytes_per_sec={} \
         overlap_speedup_permille={} overlapped_compute_chunks={} serialized_cpu_runtime_ns={} \
         overlapped_cpu_runtime_ns={} serialized_context_switches={} \
         overlapped_context_switches={} serialized_blocked_switches={} \
         overlapped_blocked_switches={}",
        requests,
        spins,
        read_latency_ns,
        sync_elapsed_ns,
        async_elapsed_ns,
        rate_per_second(
            (requests as u64).saturating_mul(info.logical_block_size as u64),
            sync_elapsed_ns
        ),
        rate_per_second(
            (requests as u64).saturating_mul(info.logical_block_size as u64),
            async_elapsed_ns
        ),
        sync_elapsed_ns.saturating_mul(1_000) / async_elapsed_ns,
        overlapped_compute_chunks,
        sync_cpu.charged_runtime_ns,
        async_cpu.charged_runtime_ns,
        sync_cpu.context_switches,
        async_cpu.context_switches,
        sync_cpu.context_switches_blocked,
        async_cpu.context_switches_blocked,
    );
}

/// Demonstrates request-size scaling: the same total payload is fetched as
/// single-block requests and as multi-block requests. Fewer, larger requests
/// amortize per-request submission and command overhead, which is the
/// dominant win on slow buses. Reported as data; every request's byte length
/// and success are asserted.
#[axtest::axtest]
fn block_runtime_request_size_benchmark() {
    let devices = BlockDeviceHandle::axtest_devices()
        .expect("block runtime must be installed for block axtest");
    let device = devices
        .first()
        .cloned()
        .expect("block axtest requires an installed block device");
    let info = device.device_info();
    assert!(
        info.num_blocks != 0,
        "block benchmark requires a non-empty block device"
    );
    let blocks_per_multi_request: u32 = 8;
    assert!(
        info.num_blocks >= u64::from(blocks_per_multi_request),
        "request-size benchmark requires at least {} blocks",
        blocks_per_multi_request
    );
    let multi_request_count = BENCHMARK_REQUESTS / blocks_per_multi_request as usize;

    // Warm the multi-block path so DMA descriptor setup is excluded from the
    // measured phase.
    let warmup = device
        .axtest_read_blocks_sync(0, blocks_per_multi_request)
        .unwrap_or_else(|error| panic!("request-size warmup failed: {error:?}"));
    assert_successful_read_bytes(
        &warmup,
        blocks_per_multi_request as usize * info.logical_block_size,
    );
    drop(warmup);

    let single_start = monotonic_time_nanos();
    let mut single_checksum = 0_u64;
    for index in 0..BENCHMARK_REQUESTS {
        let lba = (index as u64) % (info.num_blocks - u64::from(blocks_per_multi_request) + 1);
        let request = device
            .axtest_read_sync(lba)
            .unwrap_or_else(|error| panic!("single-block size benchmark read failed: {error:?}"));
        assert_successful_read(&request, info.logical_block_size);
        let data = request.data.as_ref().expect("read must return DMA data");
        let mut bytes = alloc::vec![0; info.logical_block_size];
        data.copy_to_slice_cpu(&mut bytes);
        single_checksum ^= u64::from(bytes[0]) ^ u64::from(bytes[info.logical_block_size - 1]);
    }
    let single_elapsed_ns = monotonic_time_nanos().saturating_sub(single_start).max(1);

    let multi_start = monotonic_time_nanos();
    let mut multi_checksum = 0_u64;
    for index in 0..multi_request_count {
        let lba = (index as u64 * u64::from(blocks_per_multi_request))
            % (info.num_blocks - u64::from(blocks_per_multi_request) + 1);
        let request = device
            .axtest_read_blocks_sync(lba, blocks_per_multi_request)
            .unwrap_or_else(|error| panic!("multi-block size benchmark read failed: {error:?}"));
        let byte_len = blocks_per_multi_request as usize * info.logical_block_size;
        assert_successful_read_bytes(&request, byte_len);
        let data = request.data.as_ref().expect("read must return DMA data");
        let mut bytes = alloc::vec![0; byte_len];
        data.copy_to_slice_cpu(&mut bytes);
        multi_checksum ^= u64::from(bytes[0]) ^ u64::from(bytes[byte_len - 1]);
    }
    let multi_elapsed_ns = monotonic_time_nanos().saturating_sub(multi_start).max(1);

    let total_bytes = (BENCHMARK_REQUESTS as u64) * info.logical_block_size as u64;
    axtest::axtest_println!(
        "BLOCK_REQUEST_SIZE_BENCH total_bytes={} single_blocks={} multi_requests={} \
         blocks_per_multi_request={} single_elapsed_ns={} multi_elapsed_ns={} \
         single_bytes_per_sec={} multi_bytes_per_sec={} size_speedup_permille={} \
         single_checksum={} multi_checksum={}",
        total_bytes,
        BENCHMARK_REQUESTS,
        multi_request_count,
        blocks_per_multi_request,
        single_elapsed_ns,
        multi_elapsed_ns,
        rate_per_second(total_bytes, single_elapsed_ns),
        rate_per_second(total_bytes, multi_elapsed_ns),
        single_elapsed_ns.saturating_mul(1_000) / multi_elapsed_ns,
        single_checksum,
        multi_checksum,
    );
}
