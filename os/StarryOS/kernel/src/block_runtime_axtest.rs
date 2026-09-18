//! Starry axtests for the asynchronous block runtime boundary.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    future::{Future, poll_fn},
    pin::{Pin, pin},
    task::Poll,
};

#[cfg(feature = "qperf-metrics")]
use ax_runtime::diagnostics::qperf_runtime_scheduler_metrics_snapshot;
use ax_runtime::hal::time::monotonic_time_nanos;
use ax_fs_ng::{BlockDeviceHandle, BlockError, block_batch_stats};
use rdif_block::CompletedRequest;

const BENCHMARK_REQUESTS: usize = 32;

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

fn cpu_runtime_delta(
    before: CpuRuntimeSnapshot,
    after: CpuRuntimeSnapshot,
) -> CpuRuntimeSnapshot {
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
    peak_inflight: usize,
}

fn block_batch_snapshot() -> BlockBatchSnapshot {
    let stats = block_batch_stats();
    BlockBatchSnapshot {
        submitted_requests: stats.submitted_requests,
        completed_requests: stats.completed_requests,
        failed_requests: stats.failed_requests,
        submission_batches: stats.submission_batches,
        peak_inflight: stats.peak_inflight,
    }
}

fn block_batch_delta(
    before: BlockBatchSnapshot,
    after: BlockBatchSnapshot,
) -> BlockBatchSnapshot {
    BlockBatchSnapshot {
        submitted_requests: after
            .submitted_requests
            .saturating_sub(before.submitted_requests),
        completed_requests: after
            .completed_requests
            .saturating_sub(before.completed_requests),
        failed_requests: after
            .failed_requests
            .saturating_sub(before.failed_requests),
        submission_batches: after
            .submission_batches
            .saturating_sub(before.submission_batches),
        peak_inflight: after.peak_inflight,
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct BenchmarkResult {
    elapsed_ns: u64,
    completed_requests: u64,
    batch: BlockBatchSnapshot,
    cpu: CpuRuntimeSnapshot,
    progress_polls: u64,
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
        "{label} requests={} elapsed_ns={} cpu_runtime_ns={} cpu_runtime_permille={} scheduler_metrics_enabled={} requests_per_sec={} bytes_per_sec={} submitted={} completed={} failed={} submission_batches={} peak_inflight_global={} scheduler_context_switches={} scheduler_blocked_switches={} scheduler_yielded_switches={} scheduler_preempted_switches={} progress_polls={}",
        BENCHMARK_REQUESTS,
        result.elapsed_ns,
        result.cpu.charged_runtime_ns,
        cpu_runtime_permille(result.cpu.charged_runtime_ns, result.elapsed_ns),
        cfg!(feature = "qperf-metrics"),
        rate_per_second(result.completed_requests, result.elapsed_ns),
        rate_per_second(bytes, result.elapsed_ns),
        result.batch.submitted_requests,
        result.batch.completed_requests,
        result.batch.failed_requests,
        result.batch.submission_batches,
        result.batch.peak_inflight,
        result.cpu.context_switches,
        result.cpu.context_switches_blocked,
        result.cpu.context_switches_yield,
        result.cpu.context_switches_preempted,
        result.progress_polls,
    );
}

async fn read_one(
    device: Arc<BlockDeviceHandle>,
    lba: u64,
) -> Result<CompletedRequest, BlockError> {
    // Make the first scheduling hand-off explicit so the composition check is
    // deterministic even when a device completes a request very quickly.
    let mut yielded = false;
    poll_fn(|cx| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await;
    device.axtest_read(lba).await
}

fn run_sync_benchmark(device: &Arc<BlockDeviceHandle>, block_count: u64) -> BenchmarkResult {
    let batch_before = block_batch_snapshot();
    let cpu_before = cpu_runtime_snapshot();
    let start = monotonic_time_nanos();
    let mut completed_requests = 0;
    for index in 0..BENCHMARK_REQUESTS {
        let lba = (index as u64) % block_count;
        let request = device
            .axtest_read_sync(lba)
            .unwrap_or_else(|error| panic!("synchronous benchmark request failed: {error:?}"));
        assert_eq!(request.result, Ok(()));
        completed_requests += 1;
        drop(request);
    }
    let elapsed_ns = monotonic_time_nanos().saturating_sub(start).max(1);
    BenchmarkResult {
        elapsed_ns,
        completed_requests,
        batch: block_batch_delta(batch_before, block_batch_snapshot()),
        cpu: cpu_runtime_delta(cpu_before, cpu_runtime_snapshot()),
        progress_polls: 0,
    }
}

type ReadFuture = Pin<Box<dyn Future<Output = Result<CompletedRequest, BlockError>>>>;

async fn run_async_benchmark(device: Arc<BlockDeviceHandle>, block_count: u64) -> BenchmarkResult {
    let mut futures: Vec<Option<ReadFuture>> = (0..BENCHMARK_REQUESTS)
        .map(|index| {
            let lba = (index as u64) % block_count;
            let device = Arc::clone(&device);
            Some(Box::pin(async move { device.axtest_read(lba).await }) as ReadFuture)
        })
        .collect();
    let batch_before = block_batch_snapshot();
    let cpu_before = cpu_runtime_snapshot();
    let start = monotonic_time_nanos();
    let mut completed_requests = 0;
    let mut progress_polls = 0;
    poll_fn(|cx| {
        let mut pending = false;
        for slot in &mut futures {
            let poll = slot
                .as_mut()
                .map(|future| future.as_mut().poll(cx));
            match poll {
                Some(Poll::Ready(result)) => {
                    let request = result.unwrap_or_else(|error| {
                        panic!("asynchronous benchmark request failed: {error:?}")
                    });
                    assert_eq!(request.result, Ok(()));
                    drop(request);
                    slot.take();
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
    let elapsed_ns = monotonic_time_nanos().saturating_sub(start).max(1);
    BenchmarkResult {
        elapsed_ns,
        completed_requests,
        batch: block_batch_delta(batch_before, block_batch_snapshot()),
        cpu: cpu_runtime_delta(cpu_before, cpu_runtime_snapshot()),
        progress_polls,
    }
}

/// Drives two independent block requests from one task while the first
/// operation is pending. This exercises submission, IRQ completion and
/// per-request waker delivery through the real Starry runtime.
#[axtest::axtest]
fn block_runtime_async_double_read() {
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
    let mut first_pending_before_second = false;
    let (first_result, second_result) = crate::task::future::block_on(poll_fn(|cx| {
        let first_pending = if first_result.is_none() {
            match first.as_mut().poll(cx) {
                Poll::Ready(result) => {
                    first_result = Some(result);
                    false
                }
                Poll::Pending => true,
            }
        } else {
            false
        };
        let second_polled = if second_result.is_none() {
            match second.as_mut().poll(cx) {
                Poll::Ready(result) => {
                    second_result = Some(result);
                    true
                }
                Poll::Pending => true,
            }
        } else {
            false
        };
        if first_pending && second_polled {
            first_pending_before_second = true;
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

    assert!(
        first_pending_before_second,
        "second request was not polled while the first completion was pending"
    );
    let first = first_result.expect("first block request result");
    let mut second = second_result.expect("second block request result");
    // Terminal IDs are recyclable queue-local keys. Equal IDs are a legal
    // receipt pair; normalize only the already-delivered envelopes so this
    // contract check does not depend on device completion timing.
    second.id = first.id;
    assert_eq!(first.result, Ok(()));
    assert_eq!(second.result, Ok(()));
    assert_eq!(
        first.data.as_ref().map(|dma| dma.len().get()),
        Some(block_size)
    );
    assert_eq!(
        second.data.as_ref().map(|dma| dma.len().get()),
        Some(block_size)
    );
    let mut first_data = first.data.expect("first completed DMA").into_cpu_buffer();
    let second_data = second.data.expect("second completed DMA").into_cpu_buffer();
    let second_bytes = second_data.as_slice_cpu().to_vec();
    let mut replacement = second_bytes.clone();
    replacement[0] ^= u8::MAX;
    // Completion transfers each buffer back to the CPU. Reusing a transport
    // ID must not alias these independently retained ownership objects.
    first_data.copy_from_slice_cpu(&replacement);
    assert_eq!(first_data.as_slice_cpu(), replacement);
    assert_eq!(second_data.as_slice_cpu(), second_bytes);
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

    // Exclude one-time channel, DMA and IRQ setup from both measured paths.
    device
        .axtest_read_sync(0)
        .unwrap_or_else(|error| panic!("synchronous benchmark warmup failed: {error:?}"));
    crate::task::future::block_on(device.axtest_read(0))
        .unwrap_or_else(|error| panic!("asynchronous benchmark warmup failed: {error:?}"));

    let synchronous = run_sync_benchmark(&device, info.num_blocks);
    let asynchronous = crate::task::future::block_on(run_async_benchmark(
        Arc::clone(&device),
        info.num_blocks,
    ));

    assert_eq!(synchronous.completed_requests, BENCHMARK_REQUESTS as u64);
    assert_eq!(asynchronous.completed_requests, BENCHMARK_REQUESTS as u64);
    assert_eq!(synchronous.batch.failed_requests, 0);
    assert_eq!(asynchronous.batch.failed_requests, 0);

    print_benchmark_result(
        "BLOCK_SYNC_BENCH",
        synchronous,
        info.logical_block_size,
    );
    print_benchmark_result(
        "BLOCK_ASYNC_BENCH",
        asynchronous,
        info.logical_block_size,
    );
    axtest::axtest_println!(
        "BLOCK_BENCH_COMPARE sync_elapsed_ns={} async_elapsed_ns={} sync_cpu_runtime_ns={} async_cpu_runtime_ns={} sync_context_switches={} async_context_switches={} async_progress_polls={}",
        synchronous.elapsed_ns,
        asynchronous.elapsed_ns,
        synchronous.cpu.charged_runtime_ns,
        asynchronous.cpu.charged_runtime_ns,
        synchronous.cpu.context_switches,
        asynchronous.cpu.context_switches,
        asynchronous.progress_polls,
    );
}
