//! Source-level contract for runtime-owned current-thread identity publication.

const TASK_RUNTIME: &str = include_str!("../src/task.rs");

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing `{start}`"))
        .1
        .split_once(end)
        .unwrap_or_else(|| panic!("missing `{end}` after `{start}`"))
        .0
}

fn assert_in_order(source: &str, markers: &[&str]) {
    let mut previous = 0;
    for (index, marker) in markers.iter().enumerate() {
        let offset = source
            .find(marker)
            .unwrap_or_else(|| panic!("missing `{marker}`"));
        if index != 0 {
            assert!(
                previous < offset,
                "`{}` must precede `{marker}`",
                markers[index - 1]
            );
        }
        previous = offset;
    }
}

#[test]
fn runtime_context_owns_one_pinned_leaf_header_without_percpu_duplicates() {
    let context = section(
        TASK_RUNTIME,
        "struct RuntimeContext {",
        "const _: () = assert!(offset_of!(RuntimeContext, header) == 0);",
    );
    assert!(context.contains("header: CurrentThreadHeader"));
    assert!(context.contains("inner: Box<UnsafeCell<ax_hal::context::TaskContext>>"));
    assert!(context.contains("switch_tail: UnsafeCell<Option<RuntimeSwitchTail>>"));

    assert!(!TASK_RUNTIME.contains("struct CurrentThreadHeader"));
    assert!(!TASK_RUNTIME.contains("static CURRENT_RUNTIME_CONTEXT"));
    assert!(!TASK_RUNTIME.contains("static CURRENT_RUNTIME_STACK"));
    assert!(!TASK_RUNTIME.contains("#[ax_percpu::def_percpu]\nstatic CURRENT_THREAD"));
    for facade in [
        "ax_hal::percpu::cpu_base(cpu_pin)",
        "ax_hal::percpu::current_thread(cpu_pin)",
        "ax_hal::percpu::prepare_current_thread_publish(cpu_pin, header)",
        "ax_hal::percpu::commit_current_thread_publish(prepared_current)",
    ] {
        assert!(
            TASK_RUNTIME.contains(facade),
            "runtime must use the typed HAL CPU-local facade: {facade}"
        );
    }
    assert!(!TASK_RUNTIME.contains("ax_cpu_local::current_thread("));
    assert!(!TASK_RUNTIME.contains("ax_cpu_local::publish_current_thread("));
    assert!(!TASK_RUNTIME.contains("ax_hal::percpu::publish_current_thread("));
}

#[test]
fn scheduler_identity_is_bound_once_into_header_and_task_context() {
    let bind = section(
        TASK_RUNTIME,
        "fn bind_context_thread(binding: ContextThreadBinding)",
        "fn destroy_context(",
    );
    assert_in_order(
        bind,
        &[
            "ThreadIdentity::from_parts",
            "header.bind_thread(thread_identity)",
            "set_current_header(header.as_non_null())",
        ],
    );
}

#[test]
fn bootstrap_replaces_the_permanent_boot_header_before_cpu_online() {
    let bootstrap = section(
        TASK_RUNTIME,
        "fn bind_bootstrap_runtime_context(",
        "fn finish_runtime_context_switch_tail(",
    );
    assert_in_order(
        bootstrap,
        &[
            "binding.boot_thread",
            "context.header().bind_cpu(binding)",
            "install_bootstrap_kernel_tls",
            "prepare_current_runtime_context_publish(cpu_pin, context)",
            "commit_current_thread_publish(prepared)",
            "install_bootstrap_current_thread(cpu_pin, context.header())",
        ],
    );

    let initialize = section(
        TASK_RUNTIME,
        "fn initialize_current_cpu(",
        "fn create_idle_resources(",
    );
    assert_in_order(
        initialize,
        &[
            "install_bootstrap_thread",
            "bind_bootstrap_runtime_context",
            "register_idle_thread",
        ],
    );
}

#[test]
fn switch_publication_and_exact_epoch_tail_bracket_the_naked_switch() {
    let switch = section(
        TASK_RUNTIME,
        "unsafe fn switch_context(",
        "fn install_address_space(",
    );
    assert_in_order(
        switch,
        &[
            "current_runtime_context(&cpu_pin)",
            "prepare_switch_to",
            "next_context.header().bind_cpu(prefix.header().binding())",
            "prepare_current_runtime_context_publish(&cpu_pin, next_context)",
            "next_context.stage_switch_tail(tail)",
            "transfer_scheduler_switch_baton",
            "commit_current_thread_publish(prepared_current)",
            ".switch_to_raw(",
        ],
    );

    let commit_window = switch
        .split_once("commit_current_thread_publish(prepared_current)")
        .expect("switch must publish the incoming CPU slot")
        .1
        .split_once(".switch_to_raw(")
        .expect("slot publication must be followed by the raw switch tail")
        .0;
    assert!(
        !commit_window.contains('('),
        "no Rust helper may run after CPU-slot publication: {commit_window}"
    );

    let tail = section(
        TASK_RUNTIME,
        "unsafe fn finish_switch_tail(&self)",
        "fn current_cpu_prefix(",
    );
    assert_in_order(
        tail,
        &[
            "let Some(tail) = *slot",
            "previous.unbind_cpu(tail.binding_epoch)",
            "*slot = None",
        ],
    );
}

#[test]
fn disabled_tls_has_an_explicit_zero_identity() {
    let disabled = section(
        TASK_RUNTIME,
        "#[cfg(not(feature = \"tls\"))]\nfn runtime_tls_pointer",
        "fn map_alloc_status(",
    );
    assert!(disabled.contains('0'));
}
