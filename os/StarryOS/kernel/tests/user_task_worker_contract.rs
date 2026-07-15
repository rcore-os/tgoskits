//! Kernel workers must not enter helpers that require a Starry user identity.

const AIO: &str = include_str!("../src/syscall/fs/aio.rs");
const EVENTFD: &str = include_str!("../src/file/event.rs");
const EBPF: &str = include_str!("../src/ebpf/mod.rs");
const FUTURE: &str = include_str!("../src/task/future.rs");
const KPROBE: &str = include_str!("../src/kprobe.rs");
const MM_ACCESS: &str = include_str!("../src/mm/access.rs");
const PROCFS: &str = include_str!("../src/pseudofs/proc.rs");
const SCHEDULER_TASK: &str = include_str!("../src/task/scheduler_task.rs");
const SCHEDULER_IDENTITY: &str = include_str!("../src/task/scheduler_identity.rs");
const TASK: &str = include_str!("../src/task/mod.rs");
const TASK_OPS: &str = include_str!("../src/task/ops.rs");
const TASK_TIMER: &str = include_str!("../src/task/timer.rs");
const TRACEPOINT: &str = include_str!("../src/tracepoint/mod.rs");
const TRACE_SCHED: &str = include_str!("../src/tracepoint/sched.rs");
const UPROBE: &str = include_str!("../src/uprobe/mod.rs");
const USER_LOOP: &str = include_str!("../src/task/user.rs");

#[test]
fn generic_block_on_has_no_implicit_starry_user_identity() {
    let block_on = function_body(FUTURE, "pub fn block_on<");

    assert!(!block_on.contains("try_current_user_task"));
    assert!(!block_on.contains("current_user_task"));
    assert!(FUTURE.contains("pub fn block_on_user<"));
}

#[test]
fn signal_aware_futures_require_an_explicit_user_task_borrow() {
    assert!(FUTURE.contains("pub async fn interruptible_for<"));
    assert!(FUTURE.contains("pub async fn poll_io_for<"));
    assert!(!FUTURE.contains("pub async fn interruptible<F"));
    assert!(!FUTURE.contains("pub async fn poll_io<P"));
}

#[test]
fn aio_poll_worker_waits_on_context_state_without_current_user_identity() {
    let poll_result = function_body(AIO, "fn poll_result(");

    assert!(
        !poll_result.contains("interruptible("),
        "AIO POLL runs on aio-worker and must not query current_user_task"
    );
    assert!(poll_result.contains("context.destroying.load"));
    assert!(poll_result.contains("completion_wakers"));
}

#[test]
fn aio_worker_is_a_kernel_thread_without_a_user_extension() {
    let enqueue = function_body(AIO, "fn enqueue_request(");

    assert!(enqueue.contains("spawn_kernel_thread("));
}

#[test]
fn aio_resfd_completion_uses_the_nonblocking_kernel_signal_path() {
    let notify = function_body(AIO, "fn notify_resfd(");

    assert!(EVENTFD.contains("pub(crate) fn signal_kernel("));
    assert!(notify.contains("resfd.signal_kernel(1)"));
    assert!(!notify.contains("resfd.write("));
}

#[test]
fn eventfd_user_write_still_accepts_zero() {
    let signal = function_body(EVENTFD, "pub(crate) fn signal_kernel(");
    assert!(!signal.contains("value == 0"));
    assert!(signal.contains("value == u64::MAX"));
}

#[test]
fn user_trap_passes_its_proven_task_to_uprobe_handlers() {
    assert!(UPROBE.contains("pub fn break_uprobe_handler(task: &UserTaskRef,"));
    assert!(!UPROBE.contains("try_current_user_task"));
    assert!(USER_LOOP.contains("break_uprobe_handler(&curr, &mut uctx)"));
    #[cfg(target_arch = "x86_64")]
    {
        assert!(UPROBE.contains("pub fn debug_uprobe_handler(task: &UserTaskRef,"));
        assert!(USER_LOOP.contains("debug_uprobe_handler(&curr, &mut uctx)"));
    }
}

#[test]
fn validated_user_handles_cache_their_extension_identity() {
    let user_task = SCHEDULER_TASK
        .split_once("pub struct UserTaskRef")
        .expect("UserTaskRef must exist")
        .1
        .split_once("impl UserTaskRef")
        .expect("UserTaskRef impl must follow its fields")
        .0;
    let as_thread = function_body(SCHEDULER_TASK, "pub fn as_thread(");

    assert!(user_task.contains("extension_data: usize"));
    assert!(!as_thread.contains("thread_os_extension"));
    assert!(!as_thread.contains("extension_data(&self.scheduler)"));
}

#[test]
fn weak_user_reference_treats_a_live_foreign_extension_as_absent() {
    let upgrade = function_body(SCHEDULER_TASK, "pub fn upgrade(self)");
    assert!(upgrade.contains("UserTaskRef::try_from_scheduler(handle)"));
    assert!(!upgrade.contains("InvalidRuntimeHandle"));
    assert!(!upgrade.contains("map_or_else"));
}

#[test]
fn published_user_thread_cannot_be_reported_as_a_recoverable_spawn_failure() {
    let spawn = function_body(SCHEDULER_TASK, "fn spawn_user_thread_inner<");
    assert!(spawn.contains("finish_published_user_thread(handle)"));
    assert!(!spawn.contains("try_from_scheduler(handle)?"));
}

#[test]
fn scheduler_identity_is_a_lock_free_one_time_publication() {
    assert!(SCHEDULER_IDENTITY.contains("AtomicU64"));
    assert!(SCHEDULER_IDENTITY.contains("compare_exchange"));
    assert!(!SCHEDULER_IDENTITY.contains("SpinNoIrq"));
}

#[test]
fn scheduler_switch_hooks_reuse_the_existing_cpu_pin() {
    let switch_in = function_body(TASK, "fn scheduler_switch_in(");
    let switch_out = function_body(TASK, "fn scheduler_switch_out(");
    assert!(switch_in.contains("ActiveScope::set_pinned"));
    assert!(switch_out.contains("ActiveScope::set_global_pinned"));
    assert!(!switch_in.contains("ActiveScope::set(&"));
    assert!(!switch_out.contains("ActiveScope::set_global()"));
}

#[test]
fn observer_context_uses_the_starry_per_cpu_user_view() {
    assert!(SCHEDULER_TASK.contains("static CURRENT_USER_EXTENSION: usize"));
    assert!(SCHEDULER_TASK.contains("pub(crate) fn try_current_user_irq_view("));
    for observer in [TRACEPOINT, EBPF, KPROBE] {
        assert!(!observer.contains("try_current_user_task().ok().flatten()"));
    }
    assert!(TRACEPOINT.contains("try_current_user_irq_view()"));
    assert!(EBPF.contains("try_current_user_irq_view()"));
    assert!(KPROBE.contains("try_current_user_irq_view()"));
}

#[test]
fn irq_user_view_exposes_only_bounded_probe_operations() {
    let view = SCHEDULER_TASK
        .split_once("impl UserTaskIrqView")
        .expect("IRQ user view implementation must exist")
        .1
        .split_once("pub(crate) fn try_current_user_irq_view")
        .expect("IRQ user view implementation must remain focused")
        .0;
    assert!(!view.contains("fn as_thread"));
    assert!(view.contains("push_kretprobe"));
    assert!(view.contains("pop_kretprobe"));
    assert!(view.contains("try_lock()"));
    assert!(TASK.contains("Vec::with_capacity("));
    assert!(TASK.contains("KRETPROBE_STACK_CAPACITY"));
    assert!(!KPROBE.contains("task.as_thread().kretprobe_stack"));
}

#[test]
fn bpf_comm_snapshot_releases_the_irq_view_before_copyout() {
    let helper = function_body(EBPF, "fn bpf_get_current_comm(");
    let snapshot = helper
        .find("try_current_user_irq_view()")
        .expect("comm helper must read the bounded IRQ snapshot");
    let copyout = helper
        .find("core::ptr::copy_nonoverlapping")
        .expect("comm helper must copy the snapshot to the verified buffer");
    let drop_view = helper[snapshot..copyout]
        .find("drop(task)")
        .expect("IRQ view must be released before copyout");
    assert!(snapshot + drop_view < copyout);
}

#[test]
fn ftrace_common_pid_uses_the_current_linux_tid() {
    let current_pid = function_body(TRACEPOINT, "fn current_pid()");
    assert!(current_pid.contains("task.tid()"));
    assert!(!current_pid.contains("task.tgid()"));
}

#[test]
fn sched_switch_hook_only_publishes_to_a_preallocated_deferred_ring() {
    let hook = function_body(TRACE_SCHED, "fn on_sched_switch(");
    assert!(hook.contains("publish_deferred"));
    assert!(hook.contains("notify_irq"));
    assert!(!hook.contains("trace_sched_switch("));
    assert!(!hook.contains("lock()"));
    assert!(!hook.contains("Vec"));

    let worker = function_body(TRACE_SCHED, "fn start_worker(");
    assert!(worker.contains("drain_deferred"));
    assert!(worker.contains("replay_sched_switch"));
}

#[test]
fn irq_identity_tracks_live_tid_and_uses_a_bounded_comm_snapshot() {
    let tid = function_body(SCHEDULER_TASK, "pub(crate) fn tid(");
    let identity = SCHEDULER_TASK
        .split_once("struct IrqTaskIdentity")
        .expect("IRQ identity cache must exist")
        .1
        .split_once("static STARRY_USER_TASK_EXTENSION_OPS")
        .expect("IRQ identity must precede extension callbacks")
        .0;
    assert!(tid.contains("thread.tid()"));
    assert!(identity.contains("comm_sequence: AtomicU32"));
    assert!(identity.contains("TASK_COMM_LEN - 1"));
    assert!(identity.contains("-> Option<usize>"));
    assert!(!identity.contains("compare_exchange_weak"));
}

#[test]
fn page_fault_identity_failure_is_nonblocking_and_nonreentrant() {
    let handler = function_body(MM_ACCESS, "fn handle_page_fault(");
    let resolve = function_body(MM_ACCESS, "fn resolve_page_fault_user_task(");
    assert!(handler.contains("PAGE_FAULT_IDENTITY_FAILURES.fetch_add"));
    assert!(!handler.contains("invalid Starry user-task extension during page fault"));
    assert!(resolve.contains("TaskError::CpuOwnerBorrowed"));
}

#[test]
fn kernel_page_fault_rejects_non_user_addresses_before_identity_or_sleep() {
    let handler = function_body(MM_ACCESS, "fn handle_page_fault(");
    let address_check = handler
        .find("if !user_range.contains(&vaddr.as_usize())")
        .expect("kernel page-fault callback must reject non-user addresses");
    let identity_lookup = handler
        .find("resolve_page_fault_user_task(try_current_user_task())")
        .expect("user-range faults must use the optional task adapter");
    let sleep = handler
        .find("might_sleep()")
        .expect("validated user faults may enter the sleeping MM path");

    assert!(address_check < identity_lookup);
    assert!(address_check < sleep);
}

#[test]
fn user_memory_access_does_not_treat_identity_corruption_as_no_user_task() {
    let lookup = function_body(MM_ACCESS, "fn user_task_for_memory_access(");
    assert!(lookup.contains("USER_MEMORY_IDENTITY_FAILURES.fetch_add"));
    assert!(!lookup.contains("Ok(None) | Err(_)"));
}

#[test]
fn weak_user_task_errors_are_not_treated_as_normal_exit() {
    assert!(!TASK_OPS.contains("upgrade().ok().flatten()"));
    assert!(TASK_OPS.contains("pub fn tasks() -> AxResult<Vec<UserTaskRef>>"));
    let get_task = function_body(TASK_OPS, "pub fn get_task(");
    assert!(get_task.contains("AxError::BadState"));
    assert!(!TASK_TIMER.contains("weak_task.upgrade().ok().flatten()"));
}

#[test]
fn procfs_preserves_weak_user_task_invariant_errors() {
    assert!(PROCFS.contains("fn upgrade_proc_task("));
    assert!(PROCFS.contains("VfsError::BadState"));
    assert!(!PROCFS.contains("self.task.upgrade().ok().flatten()"));
    assert!(!PROCFS.contains("Ok(None) | Err(_)"));
}

fn function_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function `{signature}`"));
    let source = &source[start..];
    let open = source
        .find('{')
        .unwrap_or_else(|| panic!("missing body for `{signature}`"));
    let mut depth = 0_usize;
    for (offset, character) in source[open..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[open..=open + offset];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function `{signature}`")
}
