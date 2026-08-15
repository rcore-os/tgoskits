use alloc::sync::Arc;

use super::*;

/// Creates a scheduler-owned kernel thread and enqueues it on the current CPU.
pub fn spawn_raw<F>(entry: F, name: String, stack_size: usize) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    // SAFETY: `None` carries no external callback ownership.
    unsafe { spawn_raw_with_extension(entry, name, stack_size, None) }
}

/// Creates a scheduler-owned kernel thread without making it runnable.
///
/// An OS publication transaction must call [`PreparedThread::stage`] before
/// exposing external identity and [`StagedThread::activate`] after commit.
pub fn prepare_raw<F>(
    entry: F,
    name: String,
    stack_size: usize,
) -> Result<PreparedThread, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    unsafe {
        // SAFETY: `None` carries no external callback ownership.
        prepare_raw_with_options(
            entry,
            name,
            stack_size,
            None,
            None,
            SchedulePolicy::default(),
            InitialContextState::kernel(),
        )
    }
}

/// Creates a scheduler-owned kernel thread with pre-publication affinity.
pub fn spawn_raw_with_affinity<F>(
    entry: F,
    name: String,
    stack_size: usize,
    affinity: CpuSet,
) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    // SAFETY: `None` carries no external callback ownership, while the affinity
    // is installed before the new thread is published to a run queue.
    unsafe { spawn_raw_with_extension_and_affinity(entry, name, stack_size, None, Some(affinity)) }
}

/// Creates a scheduler-owned kernel service thread with policy and affinity
/// installed before run-queue publication.
pub fn spawn_raw_with_policy_and_affinity<F>(
    entry: F,
    name: String,
    stack_size: usize,
    policy: SchedulePolicy,
    affinity: CpuSet,
) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    // SAFETY: `None` carries no external callback ownership. Both scheduler
    // attributes are committed before the thread can execute.
    unsafe {
        spawn_raw_with_options(
            entry,
            name,
            stack_size,
            None,
            Some(affinity),
            policy,
            InitialContextState::kernel(),
        )
    }
}

/// Creates a kernel thread while retaining one OS-specific extension.
///
/// The runtime owns an outer extension for the closure and join metadata. It
/// forwards switch, exit, Deadline-overrun and final-drop callbacks to
/// `os_extension`, preserving the inner callback-table address as its type
/// identity for StarryOS or another consuming OS.
///
/// # Safety
///
/// When present, `os_extension` transfers its unique callback-data ownership
/// to this function. The caller must not install another copy or invoke its
/// drop callback, regardless of whether thread creation succeeds.
pub unsafe fn spawn_raw_with_extension<F>(
    entry: F,
    name: String,
    stack_size: usize,
    os_extension: Option<ThreadExtension>,
) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    // SAFETY: this function forwards the extension's unique ownership without
    // creating another copy or invoking its callback table.
    unsafe { spawn_raw_with_extension_and_affinity(entry, name, stack_size, os_extension, None) }
}

/// Creates a kernel thread with an OS extension and pre-publication affinity.
///
/// Unlike setting affinity on the returned handle, `affinity` is installed in
/// [`ThreadSpec`] before the thread becomes Ready or enters a run queue. This is
/// required by pinned vCPU and per-CPU service threads whose entry point must
/// never execute on a disallowed CPU.
///
/// # Safety
///
/// When present, `os_extension` transfers its unique callback-data ownership
/// to this function. The caller must not install another copy or invoke its
/// drop callback, regardless of whether thread creation succeeds.
pub unsafe fn spawn_raw_with_extension_and_affinity<F>(
    entry: F,
    name: String,
    stack_size: usize,
    os_extension: Option<ThreadExtension>,
    affinity: Option<CpuSet>,
) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    unsafe {
        // SAFETY: this wrapper forwards unique extension ownership once.
        spawn_raw_with_options(
            entry,
            name,
            stack_size,
            os_extension,
            affinity,
            SchedulePolicy::default(),
            InitialContextState::kernel(),
        )
    }
}

/// Creates a scheduler thread whose architecture context retains a user page table.
///
/// # Safety
///
/// `os_extension` transfers unique callback-data ownership. `address_space`
/// must describe the address space retained by the OS extension for the entire
/// thread lifetime.
pub unsafe fn spawn_raw_with_extension_in_address_space<F>(
    entry: F,
    name: String,
    stack_size: usize,
    os_extension: Option<ThreadExtension>,
    address_space: TaskAddressSpace,
) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    unsafe {
        // SAFETY: this wrapper forwards both capabilities without copying the
        // extension or exposing its architecture context.
        spawn_raw_with_options(
            entry,
            name,
            stack_size,
            os_extension,
            None,
            SchedulePolicy::default(),
            InitialContextState::user(address_space),
        )
    }
}

/// Creates a user thread with its policy installed before run-queue publication.
///
/// # Safety
///
/// The extension and address-space ownership rules are identical to
/// [`spawn_raw_with_extension_in_address_space`].
pub unsafe fn spawn_raw_with_extension_in_address_space_and_policy<F>(
    entry: F,
    name: String,
    stack_size: usize,
    os_extension: Option<ThreadExtension>,
    address_space: TaskAddressSpace,
    policy: SchedulePolicy,
) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    unsafe {
        // SAFETY: ownership is forwarded once and the validated policy is
        // embedded in ThreadSpec before scheduler publication.
        spawn_raw_with_options(
            entry,
            name,
            stack_size,
            os_extension,
            None,
            policy,
            InitialContextState::user(address_space),
        )
    }
}

/// Prepares a user thread without making it runnable.
///
/// This is the transactional form of
/// [`spawn_raw_with_extension_in_address_space_and_policy`]. The caller may
/// inspect private identity through [`PreparedThread::thread_handle`], then
/// call [`PreparedThread::stage`] before publishing OS registries and
/// [`StagedThread::activate`] after that publication commits. Dropping either
/// transaction token rolls back or aborts the unstarted entry.
///
/// # Safety
///
/// The extension and address-space ownership rules are identical to
/// [`spawn_raw_with_extension_in_address_space_and_policy`].
pub unsafe fn prepare_raw_with_extension_in_address_space_and_policy<F>(
    entry: F,
    name: String,
    stack_size: usize,
    os_extension: Option<ThreadExtension>,
    address_space: TaskAddressSpace,
    policy: SchedulePolicy,
) -> Result<PreparedThread, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    unsafe {
        prepare_raw_with_options(
            entry,
            name,
            stack_size,
            os_extension,
            None,
            policy,
            InitialContextState::user(address_space),
        )
    }
}

/// Creates a RISC-V user thread while preserving the inherited FP context.
///
/// # Safety
///
/// The extension and address-space contracts are identical to
/// [`spawn_raw_with_extension_in_address_space`].
#[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
pub unsafe fn spawn_raw_with_extension_in_address_space_and_fp_state<F>(
    entry: F,
    name: String,
    stack_size: usize,
    os_extension: Option<ThreadExtension>,
    address_space: TaskAddressSpace,
    fp_state: ax_hal::cpu::FpState,
) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    unsafe {
        // SAFETY: the newly owned FP snapshot is installed before publication;
        // extension ownership is forwarded exactly once.
        spawn_raw_with_options(
            entry,
            name,
            stack_size,
            os_extension,
            None,
            SchedulePolicy::default(),
            InitialContextState::user_with_fp_state(address_space, fp_state),
        )
    }
}

/// Creates a RISC-V user thread with inherited FP state and scheduling policy.
///
/// # Safety
///
/// The ownership rules are identical to
/// [`spawn_raw_with_extension_in_address_space_and_fp_state`].
#[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
pub unsafe fn spawn_raw_with_extension_in_address_space_and_fp_state_and_policy<F>(
    entry: F,
    name: String,
    stack_size: usize,
    os_extension: Option<ThreadExtension>,
    address_space: TaskAddressSpace,
    fp_state: ax_hal::cpu::FpState,
    policy: SchedulePolicy,
) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    unsafe {
        // SAFETY: all owned capabilities are installed before publication and
        // each is transferred exactly once.
        spawn_raw_with_options(
            entry,
            name,
            stack_size,
            os_extension,
            None,
            policy,
            InitialContextState::user_with_fp_state(address_space, fp_state),
        )
    }
}

/// Prepares a RISC-V user thread with FP state without making it runnable.
///
/// # Safety
///
/// The ownership rules are identical to
/// [`spawn_raw_with_extension_in_address_space_and_fp_state_and_policy`].
#[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
pub unsafe fn prepare_raw_with_extension_in_address_space_and_fp_state_and_policy<F>(
    entry: F,
    name: String,
    stack_size: usize,
    os_extension: Option<ThreadExtension>,
    address_space: TaskAddressSpace,
    fp_state: ax_hal::cpu::FpState,
    policy: SchedulePolicy,
) -> Result<PreparedThread, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    unsafe {
        prepare_raw_with_options(
            entry,
            name,
            stack_size,
            os_extension,
            None,
            policy,
            InitialContextState::user_with_fp_state(address_space, fp_state),
        )
    }
}

unsafe fn spawn_raw_with_options<F>(
    entry: F,
    name: String,
    stack_size: usize,
    os_extension: Option<ThreadExtension>,
    affinity: Option<CpuSet>,
    policy: SchedulePolicy,
    context_state: InitialContextState,
) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    unsafe {
        prepare_raw_with_options(
            entry,
            name,
            stack_size,
            os_extension,
            affinity,
            policy,
            context_state,
        )
    }?
    .publish()
}

unsafe fn prepare_raw_with_options<F>(
    entry: F,
    name: String,
    stack_size: usize,
    os_extension: Option<ThreadExtension>,
    affinity: Option<CpuSet>,
    policy: SchedulePolicy,
    context_state: InitialContextState,
) -> Result<PreparedThread, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    if stack_size == 0 {
        // SAFETY: this function accepted the extension's unique ownership on entry.
        unsafe { release_transferred_extension(os_extension) };
        return Err(TaskError::InvalidConfiguration);
    }
    let Some(system) = task_system() else {
        // SAFETY: no runtime object observed or retained the extension.
        unsafe { release_transferred_extension(os_extension) };
        return Err(TaskError::NotInitialized);
    };
    let resources = match create_thread_resources(stack_size, runtime_thread_entry, context_state) {
        Ok(resources) => resources,
        Err(error) => {
            // SAFETY: resource construction failed before publishing extension data.
            unsafe { release_transferred_extension(os_extension) };
            return Err(error);
        }
    };
    let start = Arc::new(RuntimeThreadStart::new());
    let data = Box::into_raw(Box::new(RuntimeThreadData::new(
        Box::new(entry),
        name,
        os_extension,
        Arc::clone(&start),
    )))
    .expose_provenance();
    // SAFETY: the boxed data remains live until the scheduler reaper invokes
    // `runtime_thread_drop_hook` through this exact ops table.
    let extension = unsafe {
        // SAFETY: `data` is the unique live runtime allocation created above.
        runtime_thread_extension(data)
    };
    let mut spec = unsafe {
        // SAFETY: create_thread_resources returned one live bundle created by
        // this runtime, and this specification is its unique installation.
        ThreadSpec::new(policy)
            .with_extension(extension)
            .with_resources(resources)
    };
    if let Some(affinity) = affinity {
        spec = spec.with_affinity(affinity);
    }
    let handle = system.create_thread(spec)?;
    Ok(PreparedThread::new(system, handle, start))
}
