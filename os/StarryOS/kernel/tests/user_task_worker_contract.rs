//! Kernel workers must not enter helpers that require a Starry user identity.

const AIO: &str = include_str!("../src/syscall/fs/aio.rs");
const EVENTFD: &str = include_str!("../src/file/event.rs");
const EBPF: &str = include_str!("../src/ebpf/mod.rs");
const FUTURE: &str = include_str!("../src/task/future.rs");
const KPROBE: &str = include_str!("../src/kprobe.rs");
const MM_ACCESS: &str = include_str!("../src/mm/access.rs");
const MPP_SERVICE: &str = include_str!("../src/pseudofs/dev/mpp_service.rs");
const RKNPU: &str = include_str!("../src/pseudofs/dev/card1.rs");
const KPU: &str = include_str!("../src/pseudofs/dev/kpu.rs");
const DMAHEAP: &str = include_str!("../src/pseudofs/dev/dmaheap.rs");
const TPU: &str = include_str!("../src/pseudofs/dev/tpu/device.rs");
const NET_IO: &str = include_str!("../src/syscall/net/io.rs");
const NET_OPT: &str = include_str!("../src/syscall/net/opt.rs");
const NET_CMSG: &str = include_str!("../src/syscall/net/cmsg.rs");
const STARRY_VM_LIB: &str = include_str!("../../../../components/starry-vm/src/lib.rs");
const STARRY_VM_THIN: &str = include_str!("../../../../components/starry-vm/src/thin.rs");
const PIDFD: &str = include_str!("../src/syscall/fs/pidfd.rs");
const PERF_SAMPLING: &str = include_str!("../src/perf/sampling.rs");
const PROCFS: &str = include_str!("../src/pseudofs/proc.rs");
const SERIAL_TTY: &str = include_str!("../src/pseudofs/dev/tty/serial.rs");
const SCHEDULER_TASK: &str = include_str!("../src/task/scheduler_task.rs");
const SCHEDULER_IDENTITY: &str = include_str!("../src/task/scheduler_identity.rs");
const TASK: &str = include_str!("../src/task/mod.rs");
const TASK_OPS: &str = include_str!("../src/task/ops.rs");
const TASK_TIMER: &str = include_str!("../src/task/timer.rs");
const TRACEPOINT: &str = include_str!("../src/tracepoint/mod.rs");
const TRACE_SCHED: &str = include_str!("../src/tracepoint/sched.rs");
const UPROBE: &str = include_str!("../src/uprobe/mod.rs");
const USER_LOOP: &str = include_str!("../src/task/user.rs");
const ENTRY: &str = include_str!("../src/entry.rs");

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
fn kernel_and_user_thread_creation_cross_one_explicit_adapter_boundary() {
    let kernel_spawn = function_body(SCHEDULER_TASK, "pub fn try_spawn_kernel_thread_with_stack<");
    let user_spawn = function_body(SCHEDULER_TASK, "fn spawn_user_thread_inner<");

    assert!(kernel_spawn.contains("scheduler::spawn_raw("));
    assert!(!kernel_spawn.contains("ThreadExtension"));
    assert!(!kernel_spawn.contains("address_space"));
    assert!(user_spawn.contains("ThreadExtension::new"));
    assert!(user_spawn.contains("Some(extension)"));

    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = std::vec![source_root];
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(&path).expect("Starry source directory must be readable") {
            let entry = entry.expect("Starry source entry must be readable");
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().and_then(std::ffi::OsStr::to_str) != Some("rs")
                || path.file_name().and_then(std::ffi::OsStr::to_str) == Some("scheduler_task.rs")
            {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("Rust source must be readable");
            assert!(
                !source.contains("scheduler::spawn_raw")
                    && !source.contains("ThreadExtension::new"),
                "{} bypasses the Starry user/kernel thread adapter",
                path.display()
            );
        }
    }
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
    assert!(switch_in.contains("scope.activate_pinned"));
    assert!(switch_out.contains("scope.deactivate_pinned"));
    assert!(!switch_in.contains("ActiveScope::set(&"));
    assert!(!switch_out.contains("ActiveScope::set_global()"));
    assert!(!switch_in.contains("mem::forget"));
    assert!(!switch_out.contains("force_read_decrement"));
}

#[test]
fn active_scope_mutation_uses_a_bounded_writer_gate() {
    let mutation = function_body(TASK, "pub(crate) fn with_current_scope_mut<");

    assert!(mutation.contains("scope.with_active_mut_pinned"));
    assert!(!mutation.contains("force_read_decrement"));
    assert!(!mutation.contains("mem::forget"));
}

#[test]
fn console_output_ownership_uses_a_success_typed_two_phase_handover() {
    let prepare = function_body(SERIAL_TTY, "pub fn prepare_console_handover(");
    let commit = function_body(SERIAL_TTY, "pub fn commit(mut self)");
    let rollback = function_body(SERIAL_TTY, "fn abort_failed_start(");
    let init = function_body(ENTRY, "pub fn init(");

    assert!(SERIAL_TTY.contains("-> AxResult<PreparedConsoleHandover>"));
    assert!(prepare.contains("backend.prepare_console_handover"));
    assert!(!prepare.contains("start_port"));
    assert!(!prepare.contains("ensure_started"));
    assert!(!prepare.contains("claim_runtime_output"));
    assert!(commit.contains("commit_console_handover"));
    assert!(commit.contains("prepare_runtime_output_handover"));
    assert!(commit.contains("prepare_runtime_output_sink"));
    assert!(commit.contains("runtime_output.commit"));
    assert!(commit.contains("platform_handover.commit"));
    assert!(commit.contains("PortStartError::RecoveryFailed"));
    assert!(commit.contains("runtime_output.fail_closed"));
    assert!(rollback.contains("quiesce_to_polling"));
    assert!(rollback.contains(".is_ok()"));
    assert!(!rollback.contains("let _ = ax_serial::run_on_owner"));
    assert!(init.contains("prepare_console_handover"));
    assert!(init.contains("console_handover"));
    assert!(init.contains(".commit()"));
    assert!(!init.contains("map(Some)"));

    let prepare = init
        .find("prepare_console_handover")
        .expect("the handover must be prepared before opening stdio");
    let scope_prepare = init
        .find("PreparedProcessScope::prepare_init")
        .expect("the complete init scope must be prepared as one typed value");
    let process_create = init
        .find("ProcessData::new")
        .expect("process data must consume the prepared scope");
    let claim = init
        .find("console_handover\n        .commit()")
        .expect("the prepared console must be committed");
    let publish = init
        .find("add_task_to_table")
        .expect("the user task must be published");
    assert!(
        prepare < scope_prepare
            && scope_prepare < process_create
            && process_create < claim
            && claim < publish
    );
    assert!(TASK.contains("pub(crate) struct ProcessDataInit"));
    assert!(TASK.contains("prepared_scope: PreparedProcessScope"));
    assert!(TASK.contains("pub(crate) fn new(init: ProcessDataInit)"));
    assert!(TASK.contains("scope: prepared_scope.into_scope()"));
    assert!(!init.contains("scope_cell_mut_unpublished"));
    assert!(!init.contains("core::mem::replace"));
    assert!(!init.contains("PreemptIrqGuard"));

    let runtime_output = function_body(SERIAL_TTY, "fn try_submit_runtime_output(");
    assert!(SERIAL_TTY.contains("RuntimeOutputSinkV1::new"));
    assert!(SERIAL_TTY.contains("unsafe extern \"C\" fn runtime_normal_output"));
    assert!(SERIAL_TTY.contains("unsafe extern \"C\" fn runtime_emergency_output"));
    assert!(runtime_output.contains("try_lock"));
    assert!(runtime_output.contains("publish_irq"));
    assert!(!runtime_output.contains("output_lock"));
    assert!(!runtime_output.contains(".lock()"));
}

#[test]
fn runtime_emergency_console_uses_the_bounded_raw_uart_capability() {
    let emergency = function_body(
        SERIAL_TTY,
        "unsafe extern \"C\" fn runtime_emergency_output(",
    );

    assert!(emergency.contains("try_write_emergency"));
    assert!(emergency.contains("irqs_enabled"));
    assert!(emergency.contains("disable_irqs"));
    assert!(emergency.contains("enable_irqs"));
    assert!(!emergency.contains("try_submit_runtime_output"));
    assert!(!emergency.contains("try_lock"));
    assert!(!emergency.contains("publish_irq"));

    let flush = function_body(
        SERIAL_TTY,
        "unsafe extern \"C\" fn runtime_emergency_flush(",
    );
    assert!(flush.contains("try_flush_emergency"));
    assert!(flush.contains("disable_irqs"));
    assert!(flush.contains("enable_irqs"));
    assert!(!flush.contains("try_lock"));
    assert!(!flush.contains("publish_irq"));
}

#[test]
fn runtime_console_handover_has_one_fail_closed_irq_off_commit_boundary() {
    let commit = function_body(SERIAL_TTY, "pub fn commit(mut self) -> AxResult<()> {");
    let irq_boundary = function_body(SERIAL_TTY, "fn with_local_irqs_disabled<");

    assert!(commit.contains("commit_console_handover"));
    assert!(commit.contains("runtime_output.fail_closed()"));
    let retire_early = commit
        .find("let platform_result = platform_handover.commit()")
        .expect("the final transition must retire the paused early owner");
    let publish_runtime = commit
        .find("let runtime_result = runtime_output.commit()")
        .expect("the final transition must publish the runtime owner");
    assert!(retire_early < publish_runtime);

    assert!(irq_boundary.contains("irqs_enabled"));
    assert!(irq_boundary.contains("disable_irqs"));
    assert!(irq_boundary.contains("enable_irqs"));
    assert!(!irq_boundary.contains("IrqGuard"));
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
fn perf_irq_samples_use_linux_ids_only_for_a_published_user_task() {
    let service = function_body(PERF_SAMPLING, "fn service_overflowed_slots(");

    assert!(service.contains("try_current_user_irq_view()"));
    assert!(service.contains("task.tgid()"));
    assert!(service.contains("task.tid()"));
    assert!(!service.contains("current_thread_id()"));
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
    let enabled = hook
        .find("__sched_switch.key_is_enabled()")
        .expect("the IRQ-off hook must reject disabled tracepoints before capture");
    let worker_load = hook
        .find("SCHED_TRACE_WORKER_ID.load")
        .expect("enabled events must filter trace workers");
    let capture = hook
        .find("DeferredSchedSwitch::capture")
        .expect("enabled scheduler events must be captured");
    assert!(enabled < worker_load && worker_load < capture);
    assert!(hook.contains("should_defer_sched_switch"));
    assert!(hook.contains("SCHED_TRACE_WORKER_ID"));
    assert!(hook.contains("publish_deferred"));
    assert!(hook.contains("notify_irq"));
    assert!(!hook.contains("trace_sched_switch("));
    assert!(!hook.contains("lock()"));
    assert!(!hook.contains("Vec"));

    let worker = function_body(TRACE_SCHED, "fn start_worker(");
    assert!(worker.contains("drain_deferred"));
    assert!(worker.contains("replay_sched_switch"));
    assert!(!worker.contains("sched_notify.notify_irq"));

    let pipe_worker = function_body(TRACEPOINT, "fn start_trace_pipe_notify_worker(");
    assert!(pipe_worker.contains("pipe_notify.wait"));

    let init = function_body(TRACEPOINT, "pub fn tracepoint_init(");
    let publish_identity = init
        .find("publish_trace_worker_id")
        .expect("worker identities must be published from returned handles");
    let install = init
        .find("sched::install()")
        .expect("the scheduler hook must eventually be installed");
    assert!(publish_identity < install);
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
fn user_memory_access_is_scoped_and_never_returns_borrowed_user_memory() {
    assert!(
        MM_ACCESS.contains("struct UserAccessScope"),
        "user access must be represented by an RAII scope"
    );
    assert!(
        MM_ACCESS.contains("PhantomData<Rc<()>>"),
        "the user-access scope must not cross scheduler threads"
    );
    assert!(
        MM_ACCESS.contains("impl Drop for UserAccessScope"),
        "nested user-access state must be released on every return path"
    );
    assert!(
        TASK.contains("user_memory_access_depth: AtomicU32"),
        "nested user access must use a depth counter rather than a boolean"
    );
    assert!(
        !MM_ACCESS.contains("AxResult<&'static"),
        "safe user-pointer APIs must not return references into user mappings"
    );

    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = std::vec![source_root];
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(&path).expect("Starry source directory must be readable") {
            let entry = entry.expect("Starry source entry must be readable");
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().and_then(std::ffi::OsStr::to_str) != Some("rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("Rust source must be readable");
            assert!(
                !source.contains("get_as_ref")
                    && !source.contains("get_as_mut")
                    && !source.contains("get_as_slice")
                    && !source.contains("get_as_mut_slice"),
                "{} still uses an escaping user-memory reference API",
                path.display()
            );
        }
    }
}

#[test]
fn all_starry_user_copy_uses_the_faultable_vm_boundary() {
    for (path, source) in [
        ("pseudofs/dev/mpp_service.rs", MPP_SERVICE),
        ("pseudofs/dev/card1.rs", RKNPU),
        ("pseudofs/dev/kpu.rs", KPU),
        ("pseudofs/dev/dmaheap.rs", DMAHEAP),
    ] {
        assert!(
            !source.contains("user_copy("),
            "{path} bypasses the scoped Starry VM copy boundary"
        );
    }
}

#[test]
fn starry_device_ioctls_never_borrow_user_memory_directly() {
    for pattern in ["unsafe { &*(arg as *const", "unsafe { &mut *(arg as *mut"] {
        assert!(
            !TPU.contains(pattern),
            "TPU ioctl still constructs a direct user reference: {pattern}"
        );
    }
}

#[test]
fn safe_vm_copyout_requires_initialized_object_bytes() {
    assert!(
        STARRY_VM_LIB.contains("pub fn vm_write_slice<T: NoUninit>"),
        "vm_write_slice must reject values with potentially uninitialized padding"
    );
    assert!(
        STARRY_VM_THIN.contains("Self::Target: NoUninit"),
        "VmMutPtr::vm_write must require an initialized object representation"
    );
    assert!(
        MM_ACCESS.contains(
            "pub fn write(self, value: T) -> AxResult<()>\n    where\n        T: NoUninit"
        )
    );
    assert!(MM_ACCESS.contains(
        "pub fn write_slice(self, values: &[T]) -> AxResult<()>\n    where\n        T: NoUninit"
    ));
    assert!(MM_ACCESS.contains(
        "pub fn write_field<U>(self, offset: usize, value: U) -> AxResult<()>\n    where\n        \
         U: NoUninit"
    ));
    let user_field = function_body(MM_ACCESS, "pub fn write_field<U>(");
    assert!(user_field.contains("checked_add"));
}

#[test]
fn user_pointer_capability_is_copy_independent_of_the_pointee() {
    assert!(MM_ACCESS.contains("impl<T> Copy for UserPtr<T>"));
    assert!(MM_ACCESS.contains("impl<T> Copy for UserConstPtr<T>"));
    assert!(!MM_ACCESS.contains("#[derive(PartialEq, Clone, Copy)]\npub struct UserPtr"));
}

#[test]
fn socket_copyout_updates_only_linux_output_fields() {
    assert!(
        !NET_IO.contains("msg.write(msg_value)"),
        "recvmsg must not overwrite the input pointer fields in msghdr"
    );
    assert!(
        !NET_IO.contains("msgvec_ptr.write_slice(&msgvec)"),
        "mmsg must copy out only per-message output fields"
    );
}

#[test]
fn variable_length_abi_copies_are_bounded_before_allocation() {
    let bind_to_device = function_body(NET_OPT, "fn read_bind_to_device(");
    assert!(bind_to_device.contains("IFNAMSIZ"));
    let parse_cmsg = function_body(NET_CMSG, "pub fn parse(");
    assert!(parse_cmsg.contains("SCM_MAX_FD"));
}

#[test]
fn kernel_user_page_fault_requires_an_active_user_access_scope() {
    let handler = function_body(MM_ACCESS, "fn handle_page_fault(");
    let identity_lookup = handler
        .find("resolve_page_fault_user_task(try_current_user_task())")
        .expect("kernel page faults must resolve optional Starry identity");
    let active_scope = handler
        .find("if !thr.has_active_user_memory_access()")
        .expect("kernel user-address faults must require an active user-access scope");
    let hard_irq = handler
        .find("in_irq_context()")
        .expect("kernel user faults must fail closed in hard IRQ context");
    let sleep = handler
        .find("might_sleep()")
        .expect("validated scoped faults may enter the sleeping MM path");

    assert!(hard_irq < identity_lookup && identity_lookup < active_scope && active_scope < sleep);
    assert!(!handler.contains("debug!("));
    assert!(!handler.contains("warn!("));
}

#[test]
fn proactive_user_copy_fails_closed_before_sleepable_mm_work_in_hard_irq() {
    let prepare = function_body(MM_ACCESS, "fn prepare_user_memory(");
    let hard_irq = prepare
        .find("in_irq_context()")
        .expect("user-copy preparation must reject hard IRQ context");
    let identity = prepare
        .find("user_task_for_memory_access")
        .expect("task identity must be resolved only after the IRQ check");
    let aspace_lock = prepare
        .find("aspace_arc.lock()")
        .expect("faultable copy preparation must populate through the address space");
    let populate = prepare
        .find("populate_area")
        .expect("faultable copy preparation must populate user pages");

    assert!(hard_irq < identity && hard_irq < aspace_lock && hard_irq < populate);
}

#[test]
fn user_access_scope_underflow_cannot_publish_wrapped_depth() {
    let leave = function_body(TASK, "pub(crate) fn leave_user_memory_access(");
    assert!(leave.contains("fetch_update"));
    assert!(leave.contains("checked_sub"));
    assert!(!leave.contains("fetch_sub"));
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

#[test]
fn remote_scope_reads_release_the_scope_gate_before_fd_table_locks() {
    let thread_fd_dir = &PROCFS[PROCFS
        .find("impl SimpleDirOps for ThreadFdDir")
        .expect("missing ThreadFdDir implementation")..];
    for body in [
        function_body(thread_fd_dir, "fn child_names"),
        function_body(thread_fd_dir, "fn lookup_child"),
        function_body(PIDFD, "pub fn sys_pidfd_getfd("),
    ] {
        let clone = body
            .find("scope_cell(&scope).clone()")
            .expect("remote lookup must clone the scoped fd-table owner");
        let release = body
            .find("drop(scope)")
            .expect("remote lookup must release the scope gate explicitly");
        let table_lock = body[release..]
            .find(".read()")
            .map(|offset| release + offset)
            .expect("remote lookup must eventually read the fd table");
        assert!(clone < release && release < table_lock);
    }
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
