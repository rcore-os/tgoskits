//! Starry axtests for the asynchronous block runtime boundary.

use alloc::{
    boxed::Box,
    format,
    string::String,
    sync::Arc,
    vec,
    vec::Vec,
};
use core::{
    fmt,
    future::{Future, poll_fn},
    pin::{Pin, pin},
    task::Poll,
};

use ax_fs_ng::{BlockDeviceHandle, BlockError, BlockRegion, block_batch_stats};
use rdif_block::CompletedRequest;

use crate::block_scratch_policy::{
    MIXED_TEST_BLOCKS, MULTI_DESCRIPTOR_TEST_BLOCKS, SCRATCH_DISK_MARKER, WRITE_TEST_BLOCKS,
    declared_scratch_device_eligible, marker_scratch_device_eligible, marker_scratch_region,
    scratch_covers_case,
};

type MixedFuture = Pin<Box<dyn Future<Output = Result<CompletedRequest, BlockError>>>>;

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

/// One fallible step of a destructive case with the context needed to report
/// it. Destructive case bodies never panic: every failure is a value, so the
/// runner can restore the scratch bytes before the failure surfaces.
struct CaseFailure {
    step: &'static str,
    detail: String,
}

impl CaseFailure {
    fn new(step: &'static str, detail: String) -> Self {
        Self { step, detail }
    }
}

impl fmt::Display for CaseFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} failed: {}", self.step, self.detail)
    }
}

/// Fallible read validation for destructive case bodies: verifies the request
/// succeeded and carries exactly the expected DMA payload length.
fn checked_read_bytes(request: &CompletedRequest, byte_len: usize) -> Result<(), CaseFailure> {
    if request.result.is_err() {
        return Err(CaseFailure::new(
            "block read",
            format!("request failed: {:?}", request.result),
        ));
    }
    if request.data.as_ref().map(|data| data.len().get()) != Some(byte_len) {
        return Err(CaseFailure::new(
            "block read",
            format!("expected {byte_len} bytes of DMA data"),
        ));
    }
    Ok(())
}

/// Resolves the destructive-write scratch target. Returns `None` when the
/// machine offers neither path, in which case the write tests skip instead of
/// failing. Two discovery paths exist:
///
/// 1. An operator-declared reserved region on the root card, requested on the
///    kernel command line as `axtest.block_scratch=<start_lba>:<blocks>`. A
///    request that fails its eligibility checks is a hard error: the operator
///    explicitly asked for the destructive tests to run.
/// 2. A dedicated non-root disk whose first sector starts with the scratch
///    marker, usable without any command-line configuration.
///
/// The per-device eligibility rules (writable 512-byte geometry, minimum
/// extent, device boundary, root identity and no overlap with any protected
/// partition or partition-table metadata region) are pure functions in
/// [`crate::block_scratch_policy`] with deterministic unit tests; only device
/// I/O stays here.
fn writable_test_device() -> Option<(Arc<BlockDeviceHandle>, BlockRegion)> {
    let devices = BlockDeviceHandle::axtest_devices()
        .expect("block runtime must be installed for block axtest");

    // An explicit bootargs declaration is a hard commitment: a malformed one
    // fails the destructive tests instead of falling back to the marker path
    // or reporting a silent skip.
    match ax_fs_ng::root::axtest_scratch_region_request() {
        Some(Ok(scratch)) => {
            let candidates = devices
                .iter()
                .filter(|device| {
                    declared_scratch_device_eligible(
                        device.device_info(),
                        *scratch,
                        ax_fs_ng::root::axtest_root_region(device),
                        ax_fs_ng::root::axtest_disk_protected_regions(device),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                candidates.len(),
                1,
                "requested scratch region [{}, {}) matches no eligible device",
                scratch.start_lba,
                scratch.end_lba,
            );
            let device = Arc::clone(candidates[0]);
            log_write_device(&device, scratch);
            return Some((device, *scratch));
        }
        Some(Err(reason)) => {
            panic!("axtest.block_scratch was declared on the command line but is invalid: {reason}")
        }
        None => {}
    }

    let scratch = marker_scratch_region();
    let candidates = devices
        .iter()
        .filter(|device| {
            let info = device.device_info();
            if !marker_scratch_device_eligible(
                info,
                ax_fs_ng::root::axtest_is_root_device(device),
                ax_fs_ng::root::axtest_disk_protected_regions(device),
            ) {
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
    if candidates.is_empty() {
        return None;
    }
    // Multiple marker disks would make the destructive target ambiguous.
    assert_eq!(
        candidates.len(),
        1,
        "marker scratch disks must be unique to keep the write target deterministic"
    );
    let device = Arc::clone(candidates[0]);
    log_write_device(&device, &scratch);
    Some((device, scratch))
}

fn log_write_device(device: &BlockDeviceHandle, scratch: &BlockRegion) {
    let info = device.device_info();
    // The root region is logged for the board record; eligibility was already
    // enforced by the candidate filters above.
    if let Some(root) = ax_fs_ng::root::axtest_root_region(device) {
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
        scratch.start_lba,
        scratch.num_blocks(),
        scratch.end_lba,
        scratch.end_lba - 1,
    );
}

/// Marks a destructive case as skipped because the machine offers neither a
/// bootargs-declared scratch region nor a dedicated marker scratch disk.
fn log_write_tests_skip() {
    axtest::axtest_println!(
        "BLOCK_WRITE_TESTS_SKIPPED reason=no eligible scratch device or region on this machine"
    );
}

fn pattern_bytes(
    block_size: usize,
    block_count: u32,
    seed: u8,
) -> Result<Vec<u8>, CaseFailure> {
    let byte_len = block_size.checked_mul(block_count as usize).ok_or_else(|| {
        CaseFailure::new(
            "pattern allocation",
            format!("{block_size} bytes x {block_count} blocks overflows usize"),
        )
    })?;
    Ok((0..byte_len)
        .map(|index| {
            seed.wrapping_add(index as u8)
                .rotate_left((index % 8) as u32)
        })
        .collect())
}

fn try_read_region_sync(
    device: &Arc<BlockDeviceHandle>,
    lba: u64,
    block_count: u32,
) -> Result<Vec<u8>, CaseFailure> {
    let block_size = device.device_info().logical_block_size;
    let byte_len = block_size.checked_mul(block_count as usize).ok_or_else(|| {
        CaseFailure::new(
            "synchronous read",
            format!("{block_size} bytes x {block_count} blocks overflows usize"),
        )
    })?;
    let request = device
        .axtest_read_blocks_sync(lba, block_count)
        .map_err(|error| CaseFailure::new("synchronous read", format!("lba {lba}: {error:?}")))?;
    checked_read_bytes(&request, byte_len)?;
    let data = request.data.as_ref().expect("checked read data");
    let mut bytes = vec![0; byte_len];
    data.copy_to_slice_cpu(&mut bytes);
    Ok(bytes)
}

/// The captured pre-test bytes of one destructive case's sub-region.
/// Deliberately without `Drop`: a kernel panic shuts down without unwinding,
/// so recovery is driven explicitly by [`run_destructive_case`] before any
/// failure is reported.
struct ScratchRestore {
    device: Arc<BlockDeviceHandle>,
    lba: u64,
    block_count: u32,
    original: Vec<u8>,
}

impl ScratchRestore {
    /// Writes the captured bytes back and verifies them by reading the region
    /// again. Never panics; the caller reports failures.
    fn restore(&self) -> Result<(), CaseFailure> {
        let restored = self
            .device
            .axtest_write_blocks_sync(self.lba, self.block_count, &self.original)
            .map_err(|error| {
                CaseFailure::new("scratch restore write", format!("lba {}: {error:?}", self.lba))
            })?;
        if restored.result.is_err() {
            return Err(CaseFailure::new(
                "scratch restore write",
                format!("lba {}: {:?}", self.lba, restored.result),
            ));
        }
        let readback = try_read_region_sync(&self.device, self.lba, self.block_count)?;
        if readback != self.original {
            return Err(CaseFailure::new(
                "scratch restore verify",
                format!("lba {} did not reproduce the original bytes", self.lba),
            ));
        }
        Ok(())
    }
}

/// Everything needed to report one destructive case; at least one failure
/// slot is populated.
struct CaseReport {
    label: &'static str,
    /// Failure from before any scratch byte was written; no recovery needed.
    untouched: Option<CaseFailure>,
    /// Failure of the case body after the original bytes were captured.
    body: Option<CaseFailure>,
    /// Failure of writing the original bytes back and verifying them.
    restore: Option<CaseFailure>,
}

impl CaseReport {
    fn message(&self) -> String {
        match (
            self.untouched.as_ref(),
            self.body.as_ref(),
            self.restore.as_ref(),
        ) {
            (Some(failure), _, _) => format!(
                "destructive case {} failed before touching the scratch region: {failure}",
                self.label
            ),
            (None, Some(failure), None) => format!(
                "destructive case {} failed: {failure}; the scratch region was restored to its \
                 original bytes",
                self.label
            ),
            (None, Some(failure), Some(restore)) => format!(
                "destructive case {} failed: {failure}; restoring the scratch region also \
                 failed: {restore}",
                self.label
            ),
            (None, None, Some(restore)) => format!(
                "destructive case {} passed but restoring the scratch region failed: {restore}",
                self.label
            ),
            (None, None, None) => {
                format!("destructive case {} reported no failure", self.label)
            }
        }
    }
}

/// Drives one destructive case: capture the sub-region's bytes, verify the
/// case extent fits the scratch region, run the body, then restore the
/// captured bytes before any failure is reported. Kernel panics shut the
/// machine down without unwinding, so recovery is ordered here instead of
/// relying on destructors. The returned batch delta is diagnostic only: the
/// runtime counters are shared across devices and tasks, so cases are judged
/// by their own request results and data comparisons.
fn run_destructive_case(
    label: &'static str,
    device: Arc<BlockDeviceHandle>,
    scratch_region: BlockRegion,
    offset_blocks: u32,
    case_blocks: u32,
    body: impl FnOnce(&ScratchRestore) -> Result<(), CaseFailure>,
) -> Result<BlockBatchSnapshot, CaseReport> {
    let batch_before = block_batch_snapshot();
    let Some(lba) = scratch_region
        .start_lba
        .checked_add(u64::from(offset_blocks))
    else {
        return Err(CaseReport {
            label,
            untouched: Some(CaseFailure::new(
                "scratch coverage",
                format!(
                    "scratch start {} plus offset {} overflows the LBA space",
                    scratch_region.start_lba, offset_blocks
                ),
            )),
            body: None,
            restore: None,
        });
    };
    let Some(case_end) = lba.checked_add(u64::from(case_blocks)) else {
        return Err(CaseReport {
            label,
            untouched: Some(CaseFailure::new(
                "scratch coverage",
                format!(
                    "case start {lba} plus {case_blocks} blocks overflows the LBA space"
                ),
            )),
            body: None,
            restore: None,
        });
    };
    if !scratch_covers_case(scratch_region, offset_blocks, case_blocks) {
        return Err(CaseReport {
            label,
            untouched: Some(CaseFailure::new(
                "scratch coverage",
                format!(
                    "case writes [{}, {}) but the scratch region only holds {} blocks",
                    lba,
                    case_end,
                    scratch_region.num_blocks(),
                ),
            )),
            body: None,
            restore: None,
        });
    }
    let original = match try_read_region_sync(&device, lba, case_blocks) {
        Ok(original) => original,
        Err(failure) => {
            return Err(CaseReport {
                label,
                untouched: Some(failure),
                body: None,
                restore: None,
            });
        }
    };
    let restore_scratch = ScratchRestore {
        device: Arc::clone(&device),
        lba,
        block_count: case_blocks,
        original,
    };
    let body_failure = body(&restore_scratch).err();
    let restore_failure = restore_scratch.restore().err();
    match (body_failure, restore_failure) {
        (None, None) => Ok(block_batch_delta(batch_before, block_batch_snapshot())),
        (body, restore) => Err(CaseReport {
            label,
            untouched: None,
            body,
            restore,
        }),
    }
}

enum MixedExpectation {
    Write(u64),
    Read(u64, Vec<u8>),
}

async fn read_one(
    device: Arc<BlockDeviceHandle>,
    lba: u64,
) -> Result<CompletedRequest, BlockError> {
    device.axtest_read(lba).await
}

/// Confirms registration readiness and consistency between completion paths.
#[axtest::axtest]
fn block_runtime_registration_and_read_consistency() {
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
}

/// Submits two independent single-block reads from one task and verifies both
/// complete through the real runtime with independent DMA buffers. Devices
/// that retire the first request before the second is polled turn this into
/// two serial reads, and no supported target can observe a deterministic
/// in-flight overlap (the VisionFive 2 controller accepts one request at a
/// time), so this case does not claim concurrent in-flight or independent
/// waker coverage.
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
}

/// Validates a consecutive sequence of single-block writes and reads. The
/// original bytes are restored before any failure is reported so the scratch
/// region stays reusable for the next case.
#[axtest::axtest]
fn block_runtime_single_block_contiguous_read_write() -> axtest::AxTestResult {
    let Some((device, scratch_region)) = writable_test_device() else {
        log_write_tests_skip();
        return axtest::AxTestResult::Ok;
    };
    let block_size = device.device_info().logical_block_size;
    match run_destructive_case(
        "single-block contiguous read-write",
        Arc::clone(&device),
        scratch_region,
        0,
        WRITE_TEST_BLOCKS,
        |restore| run_single_block_body(&device, restore, block_size),
    ) {
        Ok(batch) => {
            axtest::axtest_println!(
                "BLOCK_SINGLE_RW_FUNCTIONAL lba={} blocks={} bytes={} submitted={} completed={}",
                scratch_region.start_lba,
                WRITE_TEST_BLOCKS,
                block_size * WRITE_TEST_BLOCKS as usize,
                batch.submitted_requests,
                batch.completed_requests,
            );
            axtest::AxTestResult::Ok
        }
        Err(report) => panic!("{}", report.message()),
    }
}

fn run_single_block_body(
    device: &Arc<BlockDeviceHandle>,
    restore: &ScratchRestore,
    block_size: usize,
) -> Result<(), CaseFailure> {
    let lba = restore.lba;
    let pattern = pattern_bytes(block_size, WRITE_TEST_BLOCKS, 0x31)?;
    for index in 0..WRITE_TEST_BLOCKS {
        let offset = index as usize * block_size;
        let request = device
            .axtest_write_sync(lba + u64::from(index), &pattern[offset..offset + block_size])
            .map_err(|error| {
                CaseFailure::new(
                    "single-block synchronous write",
                    format!("lba {}: {error:?}", lba + u64::from(index)),
                )
            })?;
        if request.result.is_err() {
            return Err(CaseFailure::new(
                "single-block synchronous write",
                format!("lba {}: {:?}", lba + u64::from(index), request.result),
            ));
        }
    }
    for index in 0..WRITE_TEST_BLOCKS {
        let offset = index as usize * block_size;
        let request = crate::task::future::block_on(device.axtest_read(lba + u64::from(index)))
            .map_err(|error| {
                CaseFailure::new(
                    "single-block asynchronous read",
                    format!("lba {}: {error:?}", lba + u64::from(index)),
                )
            })?;
        checked_read_bytes(&request, block_size)?;
        let data = request.data.as_ref().expect("checked read data");
        let mut bytes = vec![0; block_size];
        data.copy_to_slice_cpu(&mut bytes);
        if bytes != pattern[offset..offset + block_size] {
            return Err(CaseFailure::new(
                "single-block read-back compare",
                format!(
                    "lba {} does not match the written pattern",
                    lba + u64::from(index)
                ),
            ));
        }
    }
    Ok(())
}

/// Validates a true multi-block request (`block_count > 1`) through both the
/// asynchronous write and asynchronous read completion paths, including
/// complete buffer contents and block boundaries.
#[axtest::axtest]
fn block_runtime_multi_block_contiguous_read_write() -> axtest::AxTestResult {
    let Some((device, scratch_region)) = writable_test_device() else {
        log_write_tests_skip();
        return axtest::AxTestResult::Ok;
    };
    let block_size = device.device_info().logical_block_size;
    match run_destructive_case(
        "multi-block contiguous read-write",
        Arc::clone(&device),
        scratch_region,
        WRITE_TEST_BLOCKS,
        WRITE_TEST_BLOCKS,
        |restore| run_multi_block_body(&device, restore, block_size, 0x72),
    ) {
        Ok(batch) => {
            axtest::axtest_println!(
                "BLOCK_MULTI_RW_FUNCTIONAL lba={} blocks={} bytes={} submitted={} completed={}",
                scratch_region.start_lba + u64::from(WRITE_TEST_BLOCKS),
                WRITE_TEST_BLOCKS,
                block_size * WRITE_TEST_BLOCKS as usize,
                batch.submitted_requests,
                batch.completed_requests,
            );
            axtest::AxTestResult::Ok
        }
        Err(report) => panic!("{}", report.message()),
    }
}

/// Exercises a real chained IDMAC transfer on VisionFive 2. The kernel DMA
/// allocator returns 4 KiB-aligned buffers and each DW-MMC descriptor carries
/// at most 4 KiB, so this 8 KiB request requires two hardware descriptors.
#[axtest::axtest]
fn block_runtime_multi_descriptor_contiguous_read_write() -> axtest::AxTestResult {
    let Some((device, scratch_region)) = writable_test_device() else {
        log_write_tests_skip();
        return axtest::AxTestResult::Ok;
    };
    let block_size = device.device_info().logical_block_size;
    match run_destructive_case(
        "multi-descriptor contiguous read-write",
        Arc::clone(&device),
        scratch_region,
        WRITE_TEST_BLOCKS * 2 + MIXED_TEST_BLOCKS,
        MULTI_DESCRIPTOR_TEST_BLOCKS,
        |restore| run_multi_block_body(&device, restore, block_size, 0xC7),
    ) {
        Ok(batch) => {
            axtest::axtest_println!(
                "BLOCK_MULTI_DESCRIPTOR_RW_FUNCTIONAL lba={} blocks={} bytes={} submitted={} \
                 completed={}",
                scratch_region.start_lba + u64::from(WRITE_TEST_BLOCKS * 2 + MIXED_TEST_BLOCKS),
                MULTI_DESCRIPTOR_TEST_BLOCKS,
                block_size * MULTI_DESCRIPTOR_TEST_BLOCKS as usize,
                batch.submitted_requests,
                batch.completed_requests,
            );
            axtest::AxTestResult::Ok
        }
        Err(report) => panic!("{}", report.message()),
    }
}

fn run_multi_block_body(
    device: &Arc<BlockDeviceHandle>,
    restore: &ScratchRestore,
    block_size: usize,
    seed: u8,
) -> Result<(), CaseFailure> {
    let lba = restore.lba;
    let block_count = restore.block_count;
    let pattern = pattern_bytes(block_size, block_count, seed)?;
    let write = crate::task::future::block_on(device.axtest_write_blocks(
        lba,
        block_count,
        &pattern,
    ))
    .map_err(|error| {
        CaseFailure::new(
            "multi-block asynchronous write",
            format!("lba {lba} blocks {block_count}: {error:?}"),
        )
    })?;
    if write.result.is_err() {
        return Err(CaseFailure::new(
            "multi-block asynchronous write",
            format!("lba {lba} blocks {block_count}: {:?}", write.result),
        ));
    }
    let read = crate::task::future::block_on(device.axtest_read_blocks(lba, block_count))
        .map_err(|error| {
            CaseFailure::new(
                "multi-block asynchronous read",
                format!("lba {lba} blocks {block_count}: {error:?}"),
            )
        })?;
    checked_read_bytes(&read, pattern.len())?;
    let data = read.data.as_ref().expect("checked read data");
    let mut read_back = vec![0; pattern.len()];
    data.copy_to_slice_cpu(&mut read_back);
    if read_back != pattern {
        return Err(CaseFailure::new(
            "multi-block read-back compare",
            format!("lba {lba} blocks {block_count} does not match the written pattern"),
        ));
    }
    Ok(())
}

/// Interleaves asynchronous writes and reads in one task. Reads target the
/// untouched odd blocks while writes target even blocks, so each completion
/// has an independent, deterministic expected result.
#[axtest::axtest]
fn block_runtime_async_mixed_read_write() -> axtest::AxTestResult {
    let Some((device, scratch_region)) = writable_test_device() else {
        log_write_tests_skip();
        return axtest::AxTestResult::Ok;
    };
    let block_size = device.device_info().logical_block_size;
    let byte_len = block_size * MIXED_TEST_BLOCKS as usize;
    match run_destructive_case(
        "mixed asynchronous read-write",
        Arc::clone(&device),
        scratch_region,
        WRITE_TEST_BLOCKS * 2,
        MIXED_TEST_BLOCKS,
        |restore| run_mixed_body(&device, restore, block_size),
    ) {
        Ok(batch) => {
            axtest::axtest_println!(
                "BLOCK_MIXED_RW_FUNCTIONAL lba={} blocks={} bytes={} submitted={} completed={}",
                scratch_region.start_lba + u64::from(WRITE_TEST_BLOCKS * 2),
                MIXED_TEST_BLOCKS,
                byte_len,
                batch.submitted_requests,
                batch.completed_requests,
            );
            axtest::AxTestResult::Ok
        }
        Err(report) => panic!("{}", report.message()),
    }
}

fn run_mixed_body(
    device: &Arc<BlockDeviceHandle>,
    restore: &ScratchRestore,
    block_size: usize,
) -> Result<(), CaseFailure> {
    let lba = restore.lba;
    let block_count = restore.block_count;
    let pattern = pattern_bytes(block_size, block_count / 2, 0xA5)?;
    let mut futures: Vec<Option<(MixedExpectation, MixedFuture)>> = Vec::new();

    for index in 0..block_count {
        let target_lba = lba + u64::from(index);
        if index % 2 == 0 {
            let offset = (index as usize / 2) * block_size;
            let block = pattern[offset..offset + block_size].to_vec();
            let future_device = Arc::clone(device);
            futures.push(Some((
                MixedExpectation::Write(target_lba),
                Box::pin(async move { future_device.axtest_write(target_lba, &block).await })
                    as MixedFuture,
            )));
        } else {
            let offset = index as usize * block_size;
            let expected = restore.original[offset..offset + block_size].to_vec();
            let future_device = Arc::clone(device);
            futures.push(Some((
                MixedExpectation::Read(target_lba, expected),
                Box::pin(async move { future_device.axtest_read(target_lba).await }) as MixedFuture,
            )));
        }
    }

    let mut failure: Option<CaseFailure> = None;
    crate::task::future::block_on(poll_fn(|cx| {
        let mut pending = false;
        for slot in &mut futures {
            let poll = slot.as_mut().map(|(_, future)| future.as_mut().poll(cx));
            match poll {
                Some(Poll::Ready(Ok(request))) => {
                    let (expectation, _) = slot.take().expect("ready mixed request ownership");
                    match expectation {
                        MixedExpectation::Write(target_lba) => {
                            if request.result.is_err() {
                                failure.get_or_insert_with(|| {
                                    CaseFailure::new(
                                        "mixed asynchronous write",
                                        format!("lba {target_lba}: {:?}", request.result),
                                    )
                                });
                            }
                        }
                        MixedExpectation::Read(target_lba, expected) => {
                            if request.result.is_err() {
                                failure.get_or_insert_with(|| {
                                    CaseFailure::new(
                                        "mixed asynchronous read",
                                        format!("lba {target_lba}: {:?}", request.result),
                                    )
                                });
                            } else if request.data.as_ref().map(|data| data.len().get())
                                != Some(block_size)
                            {
                                failure.get_or_insert_with(|| {
                                    CaseFailure::new(
                                        "mixed asynchronous read",
                                        format!("lba {target_lba}: expected {block_size} bytes of DMA data"),
                                    )
                                });
                            } else {
                                let data = request.data.as_ref().expect("checked read data");
                                let mut bytes = vec![0; block_size];
                                data.copy_to_slice_cpu(&mut bytes);
                                if bytes != expected {
                                    failure.get_or_insert_with(|| {
                                        CaseFailure::new(
                                            "mixed read-back compare",
                                            format!(
                                                "lba {target_lba} does not match its expected bytes"
                                            ),
                                        )
                                    });
                                }
                            }
                        }
                    }
                }
                Some(Poll::Ready(Err(error))) => {
                    let _ = slot.take();
                    failure.get_or_insert_with(|| {
                        CaseFailure::new("mixed asynchronous request", format!("{error:?}"))
                    });
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
    if let Some(failure) = failure {
        return Err(failure);
    }

    let mixed_read_back = try_read_region_sync(device, lba, block_count)?;
    for index in 0..block_count as usize {
        let offset = index * block_size;
        let expected = if index % 2 == 0 {
            let pattern_offset = (index / 2) * block_size;
            &pattern[pattern_offset..pattern_offset + block_size]
        } else {
            &restore.original[offset..offset + block_size]
        };
        if mixed_read_back[offset..offset + block_size] != expected[..] {
            return Err(CaseFailure::new(
                "mixed read-back compare",
                format!("lba {} does not match its expected bytes", lba + index as u64),
            ));
        }
    }
    Ok(())
}
