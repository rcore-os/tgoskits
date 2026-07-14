//! Source-level ownership checks shared by host CI for non-host context code.

use std::{fs, path::PathBuf};

#[test]
fn x86_task_context_switches_tls_only_in_the_naked_window() {
    let source = read_arch_source("x86_64/context.rs");
    let rust_switch = function_body(&source, "pub fn switch_to");
    let naked_switch = function_body(&source, r#"unsafe extern "C" fn context_switch"#);

    assert!(!rust_switch.contains("write_thread_pointer"));
    assert!(!rust_switch.contains("write_user_page_table"));
    assert!(naked_switch.contains("rdmsr"));
    assert!(naked_switch.contains("wrmsr"));
    assert!(naked_switch.contains("kernel_tls_offset"));
    assert!(!task_context_definition(&source).contains("cr3"));
}

#[test]
fn aarch64_task_context_switches_tls_only_in_the_naked_window() {
    let source = read_arch_source("aarch64/context.rs");
    let rust_switch = function_body(&source, "pub fn switch_to");
    let naked_switch = function_body(&source, r#"unsafe extern "C" fn context_switch"#);

    assert!(!rust_switch.contains("write_thread_pointer"));
    assert!(!rust_switch.contains("write_user_page_table"));
    assert!(naked_switch.contains("tpidr_el0"));
    assert!(!task_context_definition(&source).contains("ttbr0_el1"));
}

#[test]
fn architecture_tls_accessors_expose_task_owned_kernel_tls() {
    for relative in ["x86_64/asm.rs", "aarch64/asm.rs"] {
        let source = read_arch_source(relative);
        assert!(
            source.contains("pub fn read_thread_pointer() -> KernelTlsBase"),
            "{relative} must return the task-owned TLS newtype",
        );
        assert!(
            source.contains("pub unsafe fn write_thread_pointer(kernel_tls: KernelTlsBase)"),
            "{relative} must require the task-owned TLS newtype",
        );
    }
}

#[test]
fn assembly_trap_handlers_use_raw_pointer_c_abi() {
    for (relative, handler) in [
        ("x86_64/trap.rs", "x86_trap_handler"),
        ("aarch64/trap.rs", "aarch64_trap_handler"),
        ("riscv/trap.rs", "riscv_trap_handler"),
        ("loongarch64/trap.rs", "loongarch64_trap_handler"),
    ] {
        let source = read_arch_source(relative);
        let signature = format!("unsafe extern \"C\" fn {handler}");
        let handler_signature = function_signature(&source, &signature);
        assert!(
            handler_signature.contains("*mut RawTrapFrame"),
            "{relative} must expose the assembly boundary as a raw-pointer C ABI",
        );
    }
}

#[test]
fn x86_cpu_initialization_does_not_rebind_the_platform_cpu_area() {
    let source = read_arch_source("x86_64/init.rs");
    assert!(!source.contains("pub fn init_percpu"));
    assert!(!source.contains("ax_percpu::init("));
    assert!(!source.contains("ax_percpu::init_percpu_reg"));
}

fn read_arch_source(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(relative);
    fs::read_to_string(path).expect("architecture context source must be readable")
}

fn function_signature<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .expect("expected function signature must exist");
    let tail = &source[start..];
    let end = tail.find('{').expect("function signature must end");
    &tail[..end]
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .expect("expected function signature must exist");
    let tail = &source[start..];
    let next = tail[signature.len()..]
        .find("\n}")
        .map_or(tail.len(), |offset| signature.len() + offset + 2);
    &tail[..next]
}

fn task_context_definition(source: &str) -> &str {
    let start = source
        .find("pub struct TaskContext")
        .expect("TaskContext definition must exist");
    let tail = &source[start..];
    let end = tail.find("\n}").expect("TaskContext definition must end");
    &tail[..end]
}
