use axtest::prelude::*;

#[axtest]
fn axtask_api_constants_hold() {
    assert!(crate::api::axtask_api_constants_hold_for_test());
}

#[axtest]
fn axtask_api_function_existence_hold() {
    assert!(crate::api::axtask_api_function_existence_hold_for_test());
}

#[axtest]
fn axtask_api_atomic_context_structs_hold() {
    assert!(crate::api::axtask_api_atomic_context_structs_hold_for_test());
}

#[axtest]
fn axtask_irq_notify_constructor_and_pending_hold() {
    assert!(crate::irq_notify::irq_notify_constructor_and_pending_hold_for_test());
}

#[axtest]
fn axtask_irq_notify_drain_logic_hold() {
    assert!(crate::irq_notify::irq_notify_drain_logic_hold_for_test());
}

#[axtest]
fn axtask_wait_queue_new_and_default_hold() {
    assert!(crate::wait_queue::wait_queue_new_and_default_hold_for_test());
}
#[axtest]
fn axtask_task_id_and_state_hold() {
    ax_assert!(crate::task::task_id_and_state_hold_for_test());
}

#[axtest]
fn axtask_task_constants_hold() {
    ax_assert!(crate::task::task_constants_hold_for_test());
}

#[axtest]
fn axtask_task_id_operations_hold() {
    ax_assert!(crate::task::task_id_operations_hold_for_test());
}

#[axtest]
fn axtask_task_state_all_variants_hold() {
    ax_assert!(crate::task::task_state_all_variants_hold_for_test());
}

#[axtest]
fn axtask_api_current_and_exit_hold() {
    ax_assert!(crate::api::axtask_api_current_and_exit_hold_for_test());
}

#[axtest]
fn axtask_api_priority_constants_hold() {
    ax_assert!(crate::api::axtask_api_priority_constants_hold_for_test());
}

#[axtest]
fn axtask_run_queue_constants_hold() {
    ax_assert!(crate::run_queue::run_queue_constants_hold_for_test());
}

#[axtest]
fn axtask_run_queue_task_state_variants_hold() {
    ax_assert!(crate::run_queue::run_queue_task_state_variants_hold_for_test());
}

#[axtest]
fn axtask_api_type_aliases_hold() {
    assert!(crate::api::axtask_api_type_aliases_hold_for_test());
}

#[axtest]
fn axtask_api_scheduler_name_hold() {
    assert!(crate::api::axtask_api_scheduler_name_hold_for_test());
}

#[axtest]
fn axtask_api_task_registry_functions_exist_hold() {
    assert!(crate::api::axtask_api_task_registry_functions_exist_hold_for_test());
}

#[axtest]
fn axtask_run_queue_percpu_statics_exist_hold() {
    assert!(crate::run_queue::run_queue_percpu_statics_exist_hold_for_test());
}

#[axtest]
fn axtask_run_queue_axrunqueue_struct_fields_hold() {
    assert!(crate::run_queue::run_queue_axrunqueue_struct_fields_hold_for_test());
}

#[axtest]
fn axtask_run_queue_current_run_queue_ref_exists_hold() {
    assert!(crate::run_queue::run_queue_current_run_queue_ref_exists_hold_for_test());
}

#[axtest]
fn axtask_run_queue_select_functions_exist_hold() {
    assert!(crate::run_queue::run_queue_select_functions_exist_hold_for_test());
}

#[axtest]
fn axtask_run_queue_init_secondary_exists_hold() {
    assert!(crate::run_queue::run_queue_init_secondary_exists_hold_for_test());
}
