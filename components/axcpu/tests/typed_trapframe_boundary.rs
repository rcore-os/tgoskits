//! Source-level checks for the typed assembly-to-Rust trap boundary.

use std::{fs, path::PathBuf};

const ARCH_TRAPS: [(&str, &str); 4] = [
    ("x86_64/trap.rs", "x86_trap_handler"),
    ("aarch64/trap.rs", "aarch64_trap_handler"),
    ("riscv/trap.rs", "riscv_trap_handler"),
    ("loongarch64/trap.rs", "loongarch64_trap_handler"),
];

#[ax_cpu::trap::breakpoint_handler]
fn typed_breakpoint_hook(_frame: &mut ax_cpu::KernelTrapFrame<'_>) -> bool {
    false
}

#[test]
fn assembly_frames_are_private_types_not_public_aliases() {
    for (relative, handler) in ARCH_TRAPS {
        let source = read_source(relative);
        let raw_definition = if relative == "x86_64/trap.rs" {
            "struct RawTrapFrame {"
        } else {
            "struct RawTrapFrame(TrapFrame);"
        };
        assert!(
            source.contains(raw_definition),
            "{relative} must keep the assembly image behind a private type",
        );
        assert!(
            !source.contains("type RawTrapFrame = TrapFrame;"),
            "{relative} must not alias the untrusted assembly image to the public frame",
        );
        assert!(
            source.contains("pub struct KernelTrapFrame<'a>"),
            "{relative} must expose a lifetime-bound kernel trap view",
        );
        assert!(
            source.contains("PhantomData<*mut ()>"),
            "{relative} kernel trap views must remain on their owning CPU",
        );
        assert!(
            !source.contains("Deref for KernelTrapFrame")
                && !source.contains("DerefMut for KernelTrapFrame"),
            "{relative} must not expose raw-frame references through Deref",
        );
        assert!(
            !source.contains("pub const fn registers(&self) -> &TrapFrame"),
            "{relative} must expose initialized snapshots, not raw-frame references",
        );
        assert!(
            source.contains(&format!("unsafe extern \"C\" fn {handler}("))
                && source.contains("*mut RawTrapFrame"),
            "{relative} must retain an explicit raw-pointer C ABI",
        );
    }
}

#[test]
fn x86_kernel_raw_frame_stops_before_absent_rsp_and_ss_slots() {
    let source = read_source("x86_64/trap.rs");
    assert!(source.contains("size_of::<RawTrapFrame>() == core::mem::offset_of!(TrapFrame, rsp)"));
    assert!(source.contains("rsp: self.raw as *const RawTrapFrame as u64"));
    assert!(source.contains("ss: gdt::KDATA.0 as u64"));
}

#[test]
fn aarch64_raw_entry_validates_integer_trap_metadata() {
    let source = read_source("aarch64/trap.rs");
    let signature = function_signature(&source, "unsafe extern \"C\" fn aarch64_trap_handler");
    assert!(signature.contains("raw_kind: u8"));
    assert!(signature.contains("raw_source: u8"));
    assert!(!signature.contains("kind: TrapKind"));
    assert!(!signature.contains("source: TrapSource"));
}

#[test]
fn aarch64_kernel_writeback_preserves_the_complete_return_mode() {
    let source = read_source("aarch64/trap.rs");
    let apply = function_body(&source, "pub fn apply_registers");
    assert!(
        apply.contains("const MODE_MASK: u64 = 0b1_1111;"),
        "SPSR.M[4:0], including the AArch32 execution-state bit, is origin-owned",
    );

    let context = read_source("aarch64/context.rs");
    let origin = function_body(&context, "pub const fn origin");
    assert!(origin.contains("self.spsr & 0b1_1111 == 0"));
}

#[test]
fn public_breakpoint_hooks_cannot_receive_the_raw_mutable_frame() {
    let source = read_source("trap.rs");
    let breakpoint = function_signature(&source, "pub fn breakpoint_handler");
    assert!(breakpoint.contains("&mut KernelTrapFrame<'_>"));
    assert!(!breakpoint.contains("&mut TrapFrame"));

    let debug = function_signature(&source, "pub fn debug_handler");
    assert!(debug.contains("&mut KernelTrapFrame<'_>"));
    assert!(!debug.contains("&mut TrapFrame"));
}

#[test]
fn public_api_exposes_user_registers_but_not_the_internal_trap_layout() {
    let crate_root = read_source("lib.rs");
    let trap = read_source("trap.rs");
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let axhal = fs::read_to_string(workspace.join("os/arceos/modules/axhal/src/lib.rs"))
        .expect("axhal source must be readable");

    assert!(!crate_root.contains("pub use crate::TrapFrame"));
    assert!(!trap.contains("pub use crate::{KernelTrapFrame, TrapFrame, UserRegisters}"));
    assert!(!axhal.contains("TaskContext, TrapFrame"));

    for (relative, _) in ARCH_TRAPS {
        let module = read_source(&relative.replace("trap.rs", "mod.rs"));
        assert!(module.contains("UserRegisters"));
        assert!(
            module.contains("TrapFrame as UserRegisters"),
            "{relative} must expose the owned register image only under its user-facing name",
        );
        assert!(
            module.contains("pub(crate) use self::context::TrapFrame"),
            "{relative} may keep the assembly layout name only inside ax-cpu",
        );
        assert!(
            !module.contains("pub type UserRegisters = TrapFrame"),
            "{relative} must expose UserRegisters as the owned public image, not a type alias",
        );
    }
}

#[test]
fn x86_user_tls_changes_are_confined_to_the_assembly_entry_window() {
    let user = read_source("x86_64/uspace.rs");
    let run = function_body(&user, "pub fn run");
    assert!(!run.contains("write_user_thread_pointer"));
    assert!(!run.contains("write_thread_pointer"));
    assert!(!run.contains("KernelGsBase::write"));
    assert!(!run.contains("KernelGsBase::read"));

    let entry = read_source("x86_64/trap.S");
    assert!(entry.contains("IA32_FS_BASE"));
    assert!(entry.contains("IA32_KERNEL_GS_BASE"));
    assert!(entry.contains("user_fs_base_offset"));
    assert!(entry.contains("kernel_fs_base_offset"));
}

#[test]
fn loongarch_kernel_probe_writeback_preserves_cpu_anchor() {
    let source = read_source("loongarch64/trap.rs");
    let apply = function_body(&source, "pub fn apply_registers");
    assert!(
        apply.contains("kernel_u0"),
        "kernel-frame writeback must save the live per-CPU r21 snapshot",
    );
    assert!(
        apply.contains("regs.u0 = kernel_u0"),
        "kernel-frame writeback must restore the live per-CPU r21 snapshot",
    );
    assert!(
        apply.contains("kernel_tp") && apply.contains("regs.tp = kernel_tp"),
        "kernel-frame writeback must preserve the current task TLS register",
    );
}

#[test]
fn riscv_kernel_probe_writeback_preserves_canonical_gp_and_task_tls() {
    let source = read_source("riscv/trap.rs");
    let apply = function_body(&source, "pub fn apply_registers");
    assert!(apply.contains("kernel_gp") && apply.contains("regs.gp = kernel_gp"));
    assert!(apply.contains("kernel_tp") && apply.contains("regs.tp = kernel_tp"));
}

#[test]
fn starry_kernel_probes_use_the_typed_kernel_view() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let trap = fs::read_to_string(workspace.join("os/StarryOS/kernel/src/trap.rs"))
        .expect("Starry trap glue must be readable");
    let kprobe = fs::read_to_string(workspace.join("os/StarryOS/kernel/src/kprobe.rs"))
        .expect("Starry kprobe glue must be readable");
    let uprobe = fs::read_to_string(workspace.join("os/StarryOS/kernel/src/uprobe/mod.rs"))
        .expect("Starry uprobe glue must be readable");

    assert!(trap.contains("cpu::KernelTrapFrame<'_>"));
    assert!(kprobe.contains("pub fn handle_breakpoint(tf: &mut KernelTrapFrame<'_>)"));
    assert!(kprobe.contains("tf.apply_registers(&updated)"));
    assert!(uprobe.contains("break_uprobe_handler(tf: &mut UserRegisters)"));
}

fn read_source(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(relative);
    fs::read_to_string(path).expect("ax-cpu source must be readable")
}

fn function_signature<'a>(source: &'a str, name: &str) -> &'a str {
    let start = source.find(name).expect("function must exist");
    let tail = &source[start..];
    let end = tail.find('{').expect("function signature must end");
    &tail[..end]
}

fn function_body<'a>(source: &'a str, name: &str) -> &'a str {
    let start = source.find(name).expect("function must exist");
    let tail = &source[start..];
    let end = tail
        .find("\n    }")
        .expect("function body must have a closing brace");
    &tail[..end]
}
