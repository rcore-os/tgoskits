//! Source-level contract for scheduler-sensitive per-CPU consumers.

const AXIPI: &str = include_str!("../../axipi/src/lib.rs");
const AXIPI_EVENT: &str = include_str!("../../axipi/src/event.rs");
const AXIPI_QUEUE: &str = include_str!("../../axipi/src/queue.rs");
const AXHAL_PERCPU: &str = include_str!("../../axhal/src/percpu.rs");
const ALLOC_TRACKING: &str = include_str!("../../axalloc/src/tracking.rs");
const BUDDY_SLAB: &str = include_str!("../../axalloc/src/buddy_slab.rs");
const TASK_RUNTIME: &str = include_str!("../src/task.rs");
const RUNTIME_LIB: &str = include_str!("../src/lib.rs");
const UNIX_NAMESPACE: &str = include_str!("../src/unix_ns.rs");

fn source_section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start = source.find(start).expect("source section start must exist");
    let end = source[start..]
        .find(end)
        .map(|offset| start + offset)
        .expect("source section end must exist");
    &source[start..end]
}

#[test]
fn ipi_queue_access_is_pinned_for_routing_and_hard_irq_consumption() {
    assert!(AXIPI.contains("this_cpu_id_pinned(route_guard.cpu_pin())"));
    assert!(AXIPI.contains("let cpu_pin = bound_cpu_pin(route_guard.cpu_pin());"));
    assert!(AXIPI.contains("IPI_EVENT_QUEUE.with_current_ref(&cpu_pin"));
    assert!(AXIPI.contains("let irq_guard = ax_kspin::IrqGuard::new();"));
    assert!(AXIPI.contains("let cpu_pin = bound_cpu_pin(irq_guard.cpu_pin());"));
    assert!(AXIPI.contains(".with_current_ref(&cpu_pin"));
    assert!(!AXIPI.contains("IPI_EVENT_QUEUE.with_current(|"));
}

#[test]
fn local_ipi_callbacks_execute_before_the_irq_cpu_pin_is_released() {
    let run_on_cpu = source_section(
        AXIPI,
        "/// Executes a callback on the specified destination CPU via IPI.",
        "/// Executes a raw thunk synchronously on the specified CPU via IPI.",
    );
    assert!(run_on_cpu.contains("let irq_guard = IrqGuard::new();"));
    assert!(run_on_cpu.contains("this_cpu_id_pinned(irq_guard.cpu_pin())"));
    assert!(
        run_on_cpu.find("callback.call();").expect("local callback")
            < run_on_cpu.find("drop(irq_guard);").expect("IRQ release")
    );

    let run_on_cpu_sync_raw = source_section(
        AXIPI,
        "/// Executes a raw thunk synchronously on the specified CPU via IPI.",
        "/// Executes a callback on all other CPUs via IPI.",
    );
    assert!(run_on_cpu_sync_raw.contains("let irq_guard = IrqGuard::new();"));
    assert!(run_on_cpu_sync_raw.contains("this_cpu_id_pinned(irq_guard.cpu_pin())"));
    assert!(
        run_on_cpu_sync_raw
            .find("unsafe { f(arg) };")
            .expect("local synchronous callback")
            < run_on_cpu_sync_raw
                .find("drop(irq_guard);")
                .expect("IRQ release")
    );

    let run_on_each_cpu = source_section(
        AXIPI,
        "/// Executes a callback on all other CPUs via IPI.",
        "/// Publishes pending IPI work from the hard-IRQ handler.",
    );
    assert!(run_on_each_cpu.contains("let irq_guard = IrqGuard::new();"));
    assert!(run_on_each_cpu.contains("this_cpu_id_pinned(irq_guard.cpu_pin())"));
    let routing_irq_release = run_on_each_cpu
        .find("drop(irq_guard);")
        .expect("routing IRQ release");
    let remote_publication = run_on_each_cpu
        .find("for (cpu_id, (node, destination))")
        .expect("remote publication loop");
    assert!(
        routing_irq_release < remote_publication,
        "multicast must not keep IRQs disabled across the complete CPU set",
    );
    let callback_irq_acquire = run_on_each_cpu
        .find("let callback_irq_guard = IrqGuard::new();")
        .expect("local callback IRQ acquire");
    assert!(
        callback_irq_acquire
            < run_on_each_cpu
                .find("callback.call();")
                .expect("local multicast callback")
    );
    assert!(
        run_on_each_cpu
            .find("callback.call();")
            .expect("local multicast callback")
            < run_on_each_cpu
                .find("drop(callback_irq_guard);")
                .expect("IRQ release")
    );
}

#[test]
fn ipi_publication_allocates_before_pinning_and_never_inside_the_queue_lock() {
    let run_on_cpu = source_section(
        AXIPI,
        "/// Executes a callback on the specified destination CPU via IPI.",
        "/// Executes a raw thunk synchronously on the specified CPU via IPI.",
    );
    let unicast_allocation = run_on_cpu
        .find("IpiEventNode::prepare(callback.into())")
        .expect("unicast callback node allocation");
    let unicast_pin = run_on_cpu
        .find("PreemptGuard::new()")
        .expect("unicast routing pin");
    assert!(unicast_allocation < unicast_pin);

    let run_on_each_cpu = source_section(
        AXIPI,
        "/// Executes a callback on all other CPUs via IPI.",
        "/// Publishes pending IPI work from the hard-IRQ handler.",
    );
    let multicast_erasure = run_on_each_cpu
        .find("let callback = callback.into();")
        .expect("multicast callback erasure");
    let node_storage = run_on_each_cpu
        .find("let mut nodes = Vec::with_capacity(cpu_num);")
        .expect("preallocated callback-node table");
    let node_allocation = run_on_each_cpu
        .find("IpiEventNode::prepare(callback.clone().into_unicast())")
        .expect("multicast callback-node allocation");
    let destination_storage = run_on_each_cpu
        .find("Vec::with_capacity(cpu_num)")
        .and_then(|first| {
            run_on_each_cpu[first + 1..]
                .find("Vec::with_capacity(cpu_num)")
                .map(|second| first + 1 + second)
        })
        .expect("preallocated destination table");
    let multicast_pin = run_on_each_cpu
        .find("PreemptGuard::new()")
        .expect("multicast routing pin");
    assert!(multicast_erasure < node_storage);
    assert!(node_storage < node_allocation);
    assert!(node_allocation < destination_storage);
    assert!(destination_storage < multicast_pin);

    assert!(!AXIPI_QUEUE.contains("VecDeque"));
    let queue_push = source_section(
        AXIPI_QUEUE,
        "    pub fn push(&mut self, src_cpu_id: usize, mut node: Box<IpiEventNode>)",
        "    /// Detaches the FIFO head without allocating.",
    );
    assert!(queue_push.contains("Box::into_raw(node)"));
    assert!(!queue_push.contains("Box::new"));
    assert!(!queue_push.contains("Vec::"));
}

#[test]
fn callback_routing_uses_typed_fallible_destination_validation() {
    let run_on_cpu = source_section(
        AXIPI,
        "/// Executes a callback on the specified destination CPU via IPI.",
        "/// Executes a raw thunk synchronously on the specified CPU via IPI.",
    );
    assert!(run_on_cpu.contains("dest_cpu: CpuId"));
    assert!(run_on_cpu.contains("Result<(), ax_hal::irq::IrqError>"));
    assert!(run_on_cpu.contains("validate_callback_destination(dest_cpu)?"));
    assert!(!run_on_cpu.contains("IPI destination must have"));

    let run_on_each_cpu = source_section(
        AXIPI,
        "/// Executes a callback on all other CPUs via IPI.",
        "/// Publishes pending IPI work from the hard-IRQ handler.",
    );
    assert!(run_on_each_cpu.contains("Result<(), ax_hal::irq::IrqError>"));
    assert!(run_on_each_cpu.contains("validate_callback_destination(CpuId(cpu_id))?"));
}

#[test]
fn ipi_callback_contract_forbids_sleeping_or_irq_unsafe_work() {
    const IRQ_SAFE_CONTRACT: &str =
        "must not block, allocate, fault, or acquire non-IRQ-safe locks";

    let run_on_cpu = source_section(
        AXIPI,
        "/// Executes a callback on the specified destination CPU via IPI.",
        "/// Executes a raw thunk synchronously on the specified CPU via IPI.",
    );
    let run_on_cpu_sync_raw = source_section(
        AXIPI,
        "/// Executes a raw thunk synchronously on the specified CPU via IPI.",
        "/// Executes a callback on all other CPUs via IPI.",
    );
    let run_on_each_cpu = source_section(
        AXIPI,
        "/// Executes a callback on all other CPUs via IPI.",
        "/// Publishes pending IPI work from the hard-IRQ handler.",
    );

    assert!(run_on_cpu.contains(IRQ_SAFE_CONTRACT));
    assert!(run_on_cpu_sync_raw.contains(IRQ_SAFE_CONTRACT));
    assert!(run_on_each_cpu.contains(IRQ_SAFE_CONTRACT));
}

#[test]
fn current_thread_identity_requires_a_pin_and_two_phase_scheduler_publication() {
    let cpu_base = source_section(
        AXHAL_PERCPU,
        "pub fn cpu_base(",
        "/// Returns the pinned current execution-context header.",
    );
    let current_thread = source_section(
        AXHAL_PERCPU,
        "pub fn current_thread(",
        "/// Validates current-thread publication before the irreversible switch tail.",
    );
    assert!(cpu_base.contains("&CpuPin"));
    assert!(current_thread.contains("&CpuPin"));

    let scheduler_switch = source_section(
        TASK_RUNTIME,
        "unsafe fn switch_context(",
        "fn install_address_space(",
    );
    let prepare = scheduler_switch
        .find("prepare_current_runtime_context_publish(&cpu_pin, next_context)")
        .expect("scheduler must validate publication under its CPU pin");
    let transfer = scheduler_switch
        .find("transfer_scheduler_switch_baton()")
        .expect("scheduler must transfer its baton before publication");
    let commit = scheduler_switch
        .find("commit_current_thread_publish(prepared_current)")
        .expect("scheduler must perform the infallible Release commit");
    let raw_switch = scheduler_switch
        .find("switch_to_raw(next_arch_context)")
        .expect("scheduler must immediately enter the naked switch tail");
    assert!(prepare < transfer && transfer < commit && commit < raw_switch);

    assert!(!AXHAL_PERCPU.contains("CURRENT_TASK_PTR"));
    assert!(!TASK_RUNTIME.contains("CURRENT_TASK_PTR"));
    assert!(!AXHAL_PERCPU.contains("pub unsafe fn publish_current_thread("));
    assert!(!TASK_RUNTIME.contains("ax_hal::percpu::publish_current_thread("));
}

#[test]
fn allocator_recursion_state_remains_on_one_cpu_for_the_whole_operation() {
    assert!(ALLOC_TRACKING.contains("let preempt_guard = ax_kspin::PreemptGuard::new();"));
    assert!(ALLOC_TRACKING.contains("ax_percpu::bound_current(preempt_guard.cpu_pin())"));
    assert!(ALLOC_TRACKING.contains("IN_GLOBAL_ALLOCATOR.read_current(&cpu_pin)"));
    assert!(ALLOC_TRACKING.contains("IN_GLOBAL_ALLOCATOR.write_current(&cpu_pin, true)"));
    assert!(!ALLOC_TRACKING.contains("IN_GLOBAL_ALLOCATOR.with_current"));
}

#[test]
fn remote_cpu_access_uses_validated_typed_indices() {
    assert!(BUDDY_SLAB.contains("ax_percpu::CpuIndex::try_from(cpu_idx)"));
    assert!(BUDDY_SLAB.contains("remote_ref_raw(cpu_index)"));
    assert!(TASK_RUNTIME.contains("task_system()?.cpu_remote(CpuId::new(cpu.as_u32()))"));
    assert!(!TASK_RUNTIME.contains("CPU_LOCAL.remote_ptr(cpu_index)"));
}

#[test]
fn ipi_callbacks_are_transferable_to_the_destination_cpu() {
    assert!(AXIPI_EVENT.contains("Box<dyn FnOnce() + Send>"));
    assert!(AXIPI_EVENT.contains("Arc<dyn Fn() + Send + Sync>"));
    assert!(AXIPI_EVENT.contains("F: FnOnce() + Send + 'static"));
    assert!(AXIPI_EVENT.contains("F: Fn() + Send + Sync + 'static"));
}

#[test]
fn unix_namespace_holds_an_owned_task_scope_context_while_locking() {
    assert!(!UNIX_NAMESPACE.contains("FS_CONTEXT.lock()"));
    assert!(UNIX_NAMESPACE.contains("let fs_context = current_fs_context();"));
}

#[test]
fn runtime_installs_the_cpu_owned_thunk_before_irq_consumers_are_online() {
    let ipi_init = RUNTIME_LIB.find("ax_ipi::init();").expect("IPI queue init");
    let unsafe_install = RUNTIME_LIB
        .find("ax_hal::irq::set_run_on_cpu_sync(ax_ipi_run_on_cpu_sync)")
        .expect("synchronous CPU-hook installation");
    let unsafe_block = RUNTIME_LIB[..unsafe_install]
        .rfind("unsafe {")
        .expect("explicit unsafe synchronous CPU-hook installation");
    let interrupt_init = RUNTIME_LIB
        .find("init_interrupt();")
        .expect("runtime interrupt initialization");
    let scheduler_online = RUNTIME_LIB
        .find("task::publish_current_cpu_online()")
        .expect("scheduler CPU publication");

    assert!(ipi_init < unsafe_block && unsafe_block < unsafe_install);
    assert!(unsafe_install < interrupt_init);
    assert!(unsafe_install < scheduler_online);
}

#[test]
fn cancelled_synchronous_ipi_does_not_retain_the_raw_payload() {
    let sync_call = source_section(AXIPI, "struct SyncCall {", "impl SyncCall {");

    assert!(sync_call.contains("payload: UnsafeCell<Option<SyncPayload>>"));
    assert!(!sync_call.contains("function: unsafe fn(*mut ())"));
    assert!(!sync_call.contains("argument: usize"));

    let execute = source_section(AXIPI, "    fn execute(&self)", "    fn wait(&self)");
    let running_clear = execute
        .find("payload.invoke_and_clear()")
        .expect("Running owner must consume and clear the raw payload");
    let publish_done = execute
        .find("self.lifecycle.finish()")
        .expect("Running owner must publish Done");
    assert!(running_clear < publish_done);

    let wait = source_section(
        AXIPI,
        "    fn wait_with(",
        "    fn take_running_payload(&self)",
    );
    let cancelled_clear = wait
        .find("self.clear_cancelled_payload()")
        .expect("Cancelled owner must clear the raw payload");
    let timeout_return = wait[cancelled_clear..]
        .find("Err(ax_hal::irq::IrqError::Timeout)")
        .map(|offset| cancelled_clear + offset)
        .expect("Cancelled owner must return Timeout");
    assert!(cancelled_clear < timeout_return);
}

#[test]
fn callback_follow_up_selects_a_current_cpu_doorbell_for_itself() {
    assert!(AXIPI.contains("fn callback_ipi_target(current: CpuId, destination: CpuId)"));
    let sender = source_section(
        AXIPI,
        "fn send_callback_ipi_claim(claim: CallbackIpiClaim)",
        "fn kick_callback_ipi(cpu: usize)",
    );

    assert!(sender.contains("callback_ipi_target(current_cpu, CpuId(claim.cpu))"));
    assert!(
        !sender.contains("CpuIpiTarget::Other {\n            cpu: CpuId(claim.cpu),\n        }")
    );

    let follow_up = source_section(
        AXIPI,
        "fn request_follow_up_ipi()",
        "fn validate_callback_routing_context()",
    );
    assert!(follow_up.contains("this_cpu_id_pinned(irq_guard.cpu_pin())"));
    assert!(follow_up.contains("kick_callback_ipi(cpu_id)"));
}
