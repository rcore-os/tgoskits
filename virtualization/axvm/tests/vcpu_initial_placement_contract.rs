const VCPU_RUNTIME: &str = include_str!("../src/runtime/vcpus.rs");
const CPU_UP_FLOW: &str = include_str!("../src/architecture/cpu_up.rs");
const ARCH_OPS: &str = include_str!("../src/architecture/ops.rs");
const VCPU_CORE: &str = include_str!("../src/vcpu.rs");
const VM_CORE: &str = include_str!("../src/vm/mod.rs");
const TASK_API: &str = include_str!("../../../os/arceos/modules/axtask/src/api.rs");

#[test]
fn vcpu_spawn_uses_an_affinity_valid_initial_cpu() {
    assert!(
        VCPU_RUNTIME.contains("prepare_task_with_initial_cpu"),
        "vCPU tasks must prepare their planned initial host CPU instead of inheriting the \
         spawning CPU"
    );

    let explicit_spawn = TASK_API
        .split_once("pub fn prepare_task_with_initial_cpu")
        .map(|(_, body)| body)
        .expect("ax-task must expose explicit initial run-queue placement");
    assert!(
        explicit_spawn.contains("ax_hal::cpu_num()"),
        "explicit initial placement must reject CPUs outside the configured host CPU range"
    );
    assert!(
        explicit_spawn.contains("!cpumask.get(initial_cpu)"),
        "explicit initial placement must reject CPUs outside the task affinity mask"
    );
    assert!(
        TASK_API.contains("run_queue_for_cpu::<NoPreemptIrqSave>(initial_cpu)"),
        "activation must enqueue the task on its validated initial run queue"
    );
}

#[test]
fn vcpu_runtime_publishes_tasks_before_activation() {
    let secondary_start = CPU_UP_FLOW
        .split_once("fn vcpu_on")
        .map(|(_, body)| body)
        .expect("secondary vCPU startup must exist");
    let publish = secondary_start
        .find("runtime.publish_reserved_vcpu_task(vcpu_id, task_ref.clone())")
        .expect("secondary vCPU task must be published in its runtime");
    let activate = secondary_start
        .find("vcpu_task.activate()")
        .expect("secondary vCPU task must be activated");
    assert!(
        publish < activate,
        "secondary vCPU task bookkeeping must be visible before a remote CPU can run it"
    );
}

#[test]
fn secondary_vcpu_startup_is_reserved_until_task_activation() {
    let secondary_start = CPU_UP_FLOW
        .split_once("fn vcpu_on")
        .map(|(_, body)| body)
        .expect("secondary vCPU startup must exist");
    let registry_reserve = secondary_start
        .find("runtime.reserve_vcpu_task")
        .expect("runtime bookkeeping must reserve the vCPU ID before backend mutation");
    let reserve = secondary_start
        .find("vcpu.reserve_startup()")
        .expect("secondary startup must atomically reserve a free vCPU");
    let configure = secondary_start
        .find("vcpu.configure_startup")
        .expect("secondary startup must configure only the reserved vCPU");
    let publish = secondary_start
        .find("runtime.publish_reserved_vcpu_task")
        .expect("secondary startup must publish only its reserved task slot");
    let activate = secondary_start
        .find("vcpu_task.activate()")
        .expect("secondary vCPU task must be activated");

    assert!(registry_reserve < reserve);
    assert!(reserve < configure);
    assert!(configure < publish);
    assert!(publish < activate);
    assert!(
        secondary_start.contains("vcpu.cancel_startup()"),
        "every pre-activation failure must release the startup reservation"
    );
    assert!(
        ARCH_OPS.contains("VmVcpuState::Starting => vcpu.bind_startup()?"),
        "the activated task must consume the startup reservation while binding"
    );
}

#[test]
fn vcpu_state_reservation_and_task_publication_are_atomic() {
    let transition = VCPU_CORE
        .split_once("pub fn transition_state")
        .map(|(_, body)| {
            body.split_once("/// Returns the architecture-specific vCPU.")
                .unwrap()
                .0
        })
        .expect("vCPU state transition API must exist");
    assert!(
        transition.contains("let mut inner_mut = self.inner_mut.lock()"),
        "state compare-and-update must occur under one lock acquisition"
    );
    assert!(
        !transition.contains("self.with_state_transition"),
        "a check-then-unlock transition permits duplicate startup reservations"
    );

    let publication = VM_CORE
        .split_once("fn reserve_vcpu_task")
        .map(|(_, body)| {
            body.split_once("pub(crate) fn rollback_vcpu_task_slot")
                .unwrap()
                .0
        })
        .expect("fallible vCPU task reservation must exist");
    assert!(publication.contains("tasks.entry(vcpu_id)"));
    assert!(publication.contains("Occupied"));
    assert!(publication.contains("already published"));
    assert!(publication.contains("VcpuTaskSlot::Starting"));
    assert!(publication.contains("publish_reserved_vcpu_task"));
}
