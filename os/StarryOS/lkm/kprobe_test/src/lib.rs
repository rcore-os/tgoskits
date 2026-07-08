#![no_std]

#[macro_use]
extern crate ax_log;
extern crate alloc;

use alloc::string::ToString;

use kmod_tools::{exit_fn, init_fn, module};
use kprobe::{KretprobeBuilder, ProbeBuilder, ProbeData, PtRegs};
use starry_kernel::kprobe::{
    register_kprobe, register_kretprobe, unregister_kprobe, unregister_kretprobe,
};

#[inline(never)]
#[unsafe(no_mangle)]
fn detect_func(x: usize, y: usize, z: Option<usize>) -> Option<usize> {
    let hart = 0;
    ax_println!("detect_func: hart_id: {}, x: {}, y:{}", hart, x, y);
    z.map(|z| x + y + z)
}

fn pre_handler(_data: &dyn ProbeData, pt_regs: &mut PtRegs) {
    ax_println!(
        "[kprobe] pre_handler: arg0: {}, arg1: {}, arg2: {}",
        pt_regs.args()[0],
        pt_regs.args()[1],
        pt_regs.args()[2]
    );
}

fn post_handler(_data: &dyn ProbeData, pt_regs: &mut PtRegs) {
    ax_println!(
        "[kprobe] post_handler: arg0: {}, arg1: {}, arg2: {}",
        pt_regs.args()[0],
        pt_regs.args()[1],
        pt_regs.args()[2]
    );
}

fn kret_post_handler(_data: &dyn ProbeData, pt_regs: &mut PtRegs) {
    ax_println!(
        "[kretprobe] post_handler: ret_value(a0): {}, ret_value(a1): {}",
        pt_regs.first_ret_value(),
        pt_regs.second_ret_value()
    );
}

pub fn kprobe_test() {
    ax_println!(
        "[kprobe] kprobe test for [detect_func]: {:#x}",
        detect_func as *const () as usize
    );
    let kprobe_builder = ProbeBuilder::new()
        .with_symbol_addr(detect_func as *const () as usize)
        .with_offset(0)
        .with_enable(true)
        .with_pre_handler(pre_handler)
        .with_post_handler(post_handler);

    let kprobe = register_kprobe(kprobe_builder);
    let new_pre_handler = |_data: &dyn ProbeData, pt_regs: &mut PtRegs| {
        ax_println!(
            "[kprobe] new_pre_handler: arg0: {}, arg1: {}, arg2: {}",
            pt_regs.args()[0],
            pt_regs.args()[1],
            pt_regs.args()[2]
        );
    };

    let builder2 = ProbeBuilder::new()
        .with_symbol("kprobe::detect_func".to_string())
        .with_symbol_addr(detect_func as *const () as usize)
        .with_offset(0)
        .with_enable(true)
        .with_pre_handler(new_pre_handler)
        .with_post_handler(post_handler);

    let kprobe2 = register_kprobe(builder2);
    ax_println!(
        "[kprobe] install 2 kprobes at [detect_func]: {:#x}",
        detect_func as *const () as usize
    );

    detect_func(1, 2, Some(3));

    unregister_kprobe(kprobe);
    unregister_kprobe(kprobe2);
    ax_println!(
        "[kprobe] uninstall 2 kprobes at [detect_func]: {:#x}",
        detect_func as *const () as usize
    );

    let kretprobe_builder = KretprobeBuilder::new(10)
        .with_symbol_addr(detect_func as *const () as usize)
        .with_enable(true)
        .with_ret_handler(kret_post_handler);

    let kretprobe = register_kretprobe(kretprobe_builder);
    ax_println!(
        "[kretprobe] install kretprobe at [detect_func]: {:#x}",
        detect_func as *const () as usize
    );
    detect_func(0xff, 0, Some(1));

    unregister_kretprobe(kretprobe);

    detect_func(3, 4, None);
    ax_println!("[kprobe] [kretprobe] test passed");
}

#[init_fn]
pub fn kprobe_test_init() -> i32 {
    ax_println!("[kprobe] kprobe test module init");
    kprobe_test();
    0
}

#[exit_fn]
fn kprobe_test_exit() {
    ax_println!("[kprobe] kprobe test module exit");
}

module!(
    name: "kprobe_test",
    license: "GPL",
    description: "A simple kprobe test kernel module",
    version: "0.1.0",
);
