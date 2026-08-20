#![no_std]
#![no_main]

use ax_std as _;
use axbacktrace as _;
use axbacktrace::Frame;
use axtest::prelude::*;

#[axtest]
fn frame_adjust_uses_the_target_architecture_instruction_width() {
    let frame = Frame {
        fp: 0x1000,
        ip: 0x2000,
    };

    #[cfg(target_arch = "x86_64")]
    ax_assert_eq!(frame.adjust_ip(), 0x1fff);
    #[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
    ax_assert_eq!(frame.adjust_ip(), 0x1ffc);
    #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
    ax_assert_eq!(frame.adjust_ip(), 0x1ffe);
}

#[axtest::tests]
mod tests {}
