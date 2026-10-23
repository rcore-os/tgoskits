// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Host-side fixtures for DRM object lifecycle and helper behavior.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    boxed::Box,
    sync::{
        Condvar, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use drm_kapi::{list::init_list_head, locks::spin_lock_init};

use crate::{
    drm_device_types::DrmDevice,
    drm_drv_types::DrmDriver,
    drm_mode_config_upstream::{drm_mode_config_cleanup, drmm_mode_config_init},
};

static HOST_CLOCK_ORIGIN: OnceLock<Instant> = OnceLock::new();
static NEXT_HOST_TASK_ID: AtomicU64 = AtomicU64::new(1);

std::thread_local! {
    static HOST_TASK_ID: u64 = NEXT_HOST_TASK_ID.fetch_add(1, Ordering::Relaxed);
    static HOST_ADMIN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static HOST_SIGNAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static REJECT_ALLOCATION: std::cell::Cell<Option<Layout>> = const { std::cell::Cell::new(None) };
    static ALLOCATION_REJECTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static ALLOCATION_BALANCE: std::cell::Cell<Option<isize>> = const { std::cell::Cell::new(None) };
}

struct HostTestAllocator;

#[global_allocator]
static HOST_TEST_ALLOCATOR: HostTestAllocator = HostTestAllocator;

fn record_allocation(change: isize) {
    let _ = ALLOCATION_BALANCE.try_with(|balance| {
        if let Some(current) = balance.get() {
            balance.set(Some(current + change));
        }
    });
}

// SAFETY: All successful allocations and deallocations use System with the
// original layout. A one-shot, thread-local rejection returns null as allowed
// by GlobalAlloc. The const-initialized TLS cells never allocate recursively.
unsafe impl GlobalAlloc for HostTestAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let reject = REJECT_ALLOCATION
            .try_with(|next| {
                if next.get() == Some(layout) {
                    next.set(None);
                    ALLOCATION_REJECTED.set(true);
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
        if reject {
            return core::ptr::null_mut();
        }
        // SAFETY: Forward the caller's allocation layout unchanged.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            record_allocation(1);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        record_allocation(-1);
        // SAFETY: The caller returns the original System pointer and layout.
        unsafe { System.dealloc(pointer, layout) };
    }
}

/// Reject one real allocation of `layout` in this thread and report whether
/// it was reached and how many successful allocations remain after `run`.
pub fn with_allocation_failure<T>(layout: Layout, run: impl FnOnce() -> T) -> (T, bool, isize) {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            REJECT_ALLOCATION.set(None);
            ALLOCATION_BALANCE.set(None);
        }
    }
    assert!(ALLOCATION_BALANCE.get().is_none());
    ALLOCATION_REJECTED.set(false);
    ALLOCATION_BALANCE.set(Some(0));
    REJECT_ALLOCATION.set(Some(layout));
    let restore = Restore;
    let result = run();
    let rejected = ALLOCATION_REJECTED.get();
    let balance = ALLOCATION_BALANCE.get().unwrap();
    drop(restore);
    (result, rejected, balance)
}

pub fn with_task_policy<T>(admin: bool, signal: bool, run: impl FnOnce() -> T) -> T {
    struct Restore(bool, bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            HOST_ADMIN.set(self.0);
            HOST_SIGNAL.set(self.1);
        }
    }
    let _restore = Restore(HOST_ADMIN.replace(admin), HOST_SIGNAL.replace(signal));
    run()
}

static HOST_WAIT_STATE: (Mutex<u64>, Condvar) = (Mutex::new(0), Condvar::new());

fn host_now_ns() -> u64 {
    HOST_CLOCK_ORIGIN
        .get_or_init(Instant::now)
        .elapsed()
        .as_nanos()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn host_delay_us(micros: u32) {
    let start = Instant::now();
    let duration = Duration::from_micros(u64::from(micros));
    while start.elapsed() < duration {
        std::hint::spin_loop();
    }
}

fn host_sleep_us(micros: u64) {
    std::thread::sleep(Duration::from_micros(micros));
}

fn host_relax() {
    std::thread::yield_now();
}

static HOST_CLOCK_OPS: drm_kapi::clock::ClockOps = drm_kapi::clock::ClockOps {
    now_ns: host_now_ns,
    delay_us: host_delay_us,
    sleep_us: host_sleep_us,
    relax: host_relax,
};

// A host-test token mapping for page-cache and allocation identity algorithms.
// The returned addresses are not hardware PFNs and must never be used as
// evidence for a device mapping or physical-address capability.
fn host_virt_to_phys(address: *const u8) -> Option<u64> {
    (!address.is_null()).then_some(address as usize as u64)
}

fn host_phys_to_virt(physical: u64) -> Option<*mut u8> {
    usize::try_from(physical)
        .ok()
        .filter(|address| *address != 0)
        .map(|address| address as *mut u8)
}

fn host_totalram_pages() -> usize {
    // This synthetic capacity is only used by host-side allocator logic; it
    // does not describe the machine's installed RAM.
    1 << 20
}

static HOST_MEMORY_ADDRESS_OPS: drm_kapi::shmem::MemoryAddressOps =
    drm_kapi::shmem::MemoryAddressOps {
        virt_to_phys: host_virt_to_phys,
        phys_to_virt: host_phys_to_virt,
        totalram_pages: host_totalram_pages,
    };

fn host_current_task_id() -> Option<u64> {
    HOST_TASK_ID.with(|id| Some(*id))
}

fn host_need_resched() -> bool {
    false
}

fn host_can_block() -> bool {
    true
}

fn host_yield_now() {
    std::thread::yield_now();
}

fn host_sleep_ns(nanos: u64) -> Result<(), i32> {
    std::thread::sleep(Duration::from_nanos(nanos));
    Ok(())
}

fn host_spawn_with_policy(
    name: std::string::String,
    policy: drm_kapi::task_runtime::TaskSchedulingPolicy,
    entry: drm_kapi::task_runtime::TaskEntry,
) -> Result<(), i32> {
    if policy != drm_kapi::task_runtime::TaskSchedulingPolicy::Normal {
        return Err(-drm_kapi::config::EOPNOTSUPP);
    }
    std::thread::Builder::new()
        .name(name)
        .spawn(move || entry())
        .map(|_| ())
        .map_err(|_| -drm_kapi::config::ENOMEM)
}

fn host_cpu_count() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

fn host_preempt_disable() {}

fn host_preempt_enable() {}

fn host_irq_save_disable() -> core::ffi::c_ulong {
    0
}

/// # Safety
///
/// In this host mock, IRQ manipulation is a no-op; caller must provide valid flags.
unsafe fn host_irq_restore(_flags: core::ffi::c_ulong) {}

fn host_irq_disable() {}

fn host_irq_enable() {}

fn host_irqs_disabled() -> bool {
    false
}

static HOST_LOCK_CONTEXT_OPS: drm_kapi::locks::LockContextOps = drm_kapi::locks::LockContextOps {
    preempt_disable: host_preempt_disable,
    preempt_enable: host_preempt_enable,
    irq_save_disable: host_irq_save_disable,
    irq_restore: host_irq_restore,
    irq_disable: host_irq_disable,
    irq_enable: host_irq_enable,
    irqs_disabled: host_irqs_disabled,
};

/// # Safety
///
/// `buf` must point to a writable, non-null buffer with capacity of at least 16 bytes (`TASK_COMM_LEN`).
unsafe extern "C" fn host_copy_current_comm(buf: *mut core::ffi::c_char) {
    assert!(!buf.is_null());
    let current = std::thread::current();
    let name = current.name().unwrap_or("kdrm-core-test").as_bytes();
    let len = name.len().min(15);
    // SAFETY: the TaskOps contract requires a writable 16-byte TASK_COMM_LEN
    // buffer; the final byte remains NUL due to the zero fill.
    unsafe {
        core::ptr::write_bytes(buf, 0, 16);
        core::ptr::copy_nonoverlapping(name.as_ptr(), buf.cast::<u8>(), len);
    }
}

/// # Safety
///
/// No caller-side invariants required; safe to call in any context.
unsafe extern "C" fn host_current_pid_vnr() -> core::ffi::c_int {
    std::process::id().try_into().unwrap_or(i32::MAX)
}

/// # Safety
///
/// No caller-side invariants required; safe to call in any context.
unsafe extern "C" fn host_capable(capability: core::ffi::c_int) -> bool {
    capability == drm_kapi::config::CAP_SYS_ADMIN && HOST_ADMIN.get()
}

/// # Safety
///
/// No caller-side invariants required; safe to call in any context.
unsafe extern "C" fn host_perfmon_capable() -> bool {
    false
}

static HOST_TASK_OPS: drm_kapi::kernel_services::TaskOps = drm_kapi::kernel_services::TaskOps {
    copy_current_comm: host_copy_current_comm,
    current_pid_vnr: host_current_pid_vnr,
    capable: host_capable,
    perfmon_capable: host_perfmon_capable,
};

static HOST_TASK_RUNTIME_OPS: drm_kapi::task_runtime::TaskRuntimeOps =
    drm_kapi::task_runtime::TaskRuntimeOps {
        current_task_id: host_current_task_id,
        current_need_resched: host_need_resched,
        can_block_current: host_can_block,
        yield_now: host_yield_now,
        sleep_ns: host_sleep_ns,
        spawn_with_policy: host_spawn_with_policy,
        cpu_count: host_cpu_count,
    };

fn lock_host_wait_state() -> std::sync::MutexGuard<'static, u64> {
    HOST_WAIT_STATE
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn host_wait_timeout_until(timeout_ns: Option<u64>, ready: &dyn Fn() -> bool) -> bool {
    let deadline =
        timeout_ns.and_then(|timeout| Instant::now().checked_add(Duration::from_nanos(timeout)));
    loop {
        // The predicate can itself wake this provider (e.g. a DMA-fence
        // predicate drops an RCU read-side section). Never invoke it while
        // holding HOST_WAIT_STATE's mutex.
        let observed = *lock_host_wait_state();
        if ready() {
            return false;
        }
        if deadline.is_some_and(|end| Instant::now() >= end) {
            return true;
        }

        let generation = lock_host_wait_state();
        // A wake after the generation sample but before this lock means the
        // condition may have changed. Recheck the predicate before sleeping.
        if *generation != observed {
            drop(generation);
            continue;
        }

        if let Some(deadline) = deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                drop(generation);
                continue;
            }
            let (next, _) = HOST_WAIT_STATE
                .1
                .wait_timeout(generation, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            drop(next);
        } else {
            let next = HOST_WAIT_STATE
                .1
                .wait(generation)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            drop(next);
        }
    }
}

fn host_wake_all() {
    let mut generation = lock_host_wait_state();
    *generation = generation.wrapping_add(1);
    HOST_WAIT_STATE.1.notify_all();
}

static HOST_WAIT_OPS: drm_kapi::wait::WaitOps = drm_kapi::wait::WaitOps {
    wait_timeout_until: host_wait_timeout_until,
    wake_all: host_wake_all,
};

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::{host_wait_timeout_until, host_wake_all};

    #[test]
    fn wait_predicate_may_reenter_wake_without_losing_it() {
        let calls = Cell::new(0);
        let timed_out = host_wait_timeout_until(Some(100_000_000), &|| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                // Exercise a wake in the sample/check/relock interval. It
                // must neither reenter a held provider mutex nor be lost.
                host_wake_all();
                false
            } else {
                true
            }
        });

        assert!(!timed_out);
        assert_eq!(calls.get(), 2);
    }
}

/// # Safety
///
/// In this host mock, signals are never pending; the task pointer is unused and requires no preconditions.
unsafe extern "C" fn no_pending_host_signal(_state: i32, _task: *mut core::ffi::c_void) -> bool {
    HOST_SIGNAL.get()
}

/// Install the real host monotonic clock used by host-only DRM tests.
pub fn ensure_test_clock_provider() {
    drm_kapi::clock::install_clock_ops(&HOST_CLOCK_OPS)
        .expect("DRM host clock provider was already replaced");
}

/// Install real host-thread scheduler and condition-variable wait adapters for
/// DRM host tests. These provide execution context rather than fake DRM state.
pub fn ensure_test_runtime_providers() {
    drm_kapi::task_runtime::install_task_runtime_ops(&HOST_TASK_RUNTIME_OPS)
        .expect("DRM host task-runtime provider was already replaced");
    drm_kapi::wait::install_wait_ops(&HOST_WAIT_OPS)
        .expect("DRM host wait provider was already replaced");
    drm_kapi::locks::install_lock_context_ops(&HOST_LOCK_CONTEXT_OPS)
        .expect("DRM host lock-context provider was already replaced");
    drm_kapi::kernel_services::install_task_ops(&HOST_TASK_OPS)
        .expect("DRM host task-info provider was already replaced");
}

/// Install the real monotonic clock, task identity, scheduler and wait
/// providers required by host DRM tests.
pub fn init_runtime() {
    drm_kapi::shmem::install_memory_address_ops(&HOST_MEMORY_ADDRESS_OPS)
        .expect("DRM host memory-address provider was already replaced");
    ensure_test_clock_provider();
    ensure_test_runtime_providers();
    ensure_test_signal_provider();
}

/// Install a no-pending-signal policy for host tests without a process signal
/// subsystem. The test provider never dereferences the opaque task token.
pub fn ensure_test_signal_provider() {
    drm_kapi::signal::install_signal_pending_state(no_pending_host_signal)
        .expect("DRM host signal provider was already replaced");
}

/// A host `drm_device` plus the `drm_driver` it points at. Both are boxed so
/// their addresses stay fixed for the lifetime of the test.
pub struct TestDrm {
    pub dev: Box<DrmDevice>,
    _driver: Box<DrmDriver>,
}

impl TestDrm {
    /// A device whose driver advertises `driver_features` (`DRIVER_*` bits).
    pub fn new(driver_features: u32) -> Self {
        init_runtime();
        // SAFETY: `DrmDriver` consists solely of primitive integers, flags, and nullable function/data pointers.
        // All-zero bytes represent a valid uninitialized driver struct as allocated by kernel `kzalloc()`.
        let mut driver: Box<DrmDriver> = Box::new(unsafe { core::mem::zeroed() });
        driver.driver_features = driver_features;

        // SAFETY: `DrmDevice` consists of primitives, lists, locks, and nullable pointers.
        // An all-zero byte pattern represents valid initial state for these C-compatible fields before explicit initialization.
        let mut dev: Box<DrmDevice> = Box::new(unsafe { core::mem::zeroed() });
        dev.driver = &mut *driver;
        dev.driver_features = driver_features;
        // drm_dev_init(): the managed-resource list starts empty and dev starts with refcount 1.
        // SAFETY: `dev` is a valid, uniquely owned heap allocation. `addr_of_mut!` generates valid
        // pointers to internal fields that match the expectations of list, lock, and kref initializers.
        unsafe {
            init_list_head(core::ptr::addr_of_mut!(dev.managed.resources));
            spin_lock_init(&mut dev.managed.lock);
            drm_kapi::memory::kref_init(core::ptr::addr_of_mut!(dev.r#ref));
        }
        Self {
            dev,
            _driver: driver,
        }
    }

    /// The raw device pointer handed to the C-ABI ports.
    pub fn ptr(&mut self) -> *mut DrmDevice {
        &mut *self.dev
    }

    /// The object-registry part of `drmm_mode_config_init()` only: the object
    /// id registry, the property and blob lists, and their mutexes. Used by
    /// tests that need ids and properties without the standard property set.
    pub fn init_registry(&mut self) {
        let ptr = self.ptr();
        // SAFETY: `ptr` is the live device owned by `self`; each call below
        // initialises one field of the device's mode_config in place.
        unsafe {
            let mc = core::ptr::addr_of_mut!((*ptr).mode_config);
            drm_kapi::mutex::mutex_init(core::ptr::addr_of_mut!((*mc).idr_mutex));
            drm_kapi::mutex::mutex_init(core::ptr::addr_of_mut!((*mc).blob_lock));
            crate::drm_mode_object_upstream::idr_init(core::ptr::addr_of_mut!((*mc).object_idr));
            init_list_head(core::ptr::addr_of_mut!((*mc).property_list));
            init_list_head(core::ptr::addr_of_mut!((*mc).property_blob_list));
        }
    }

    /// `drmm_mode_config_init()`: standard properties, id registries and lists.
    pub fn init_mode_config(&mut self) -> i32 {
        let ptr = self.ptr();
        // SAFETY: `ptr` is the live device owned by `self` and initialized in `TestDrm::new`.
        unsafe { drmm_mode_config_init(ptr) }
    }

    /// `drm_mode_config_cleanup()`: the explicit teardown the tests call.
    pub fn cleanup_mode_config(&mut self) {
        let ptr = self.ptr();
        // SAFETY: `ptr` is the live device owned by `self` whose mode_config was initialized earlier.
        unsafe { drm_mode_config_cleanup(ptr) }
    }
}
