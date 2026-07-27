use alloc::{format, vec};

use axtest::prelude::*;

use crate::{Backtrace, CAPTURE_CAPACITY, CaptureBuf, Frame, Inner, max_depth, set_max_depth};

#[axtest]
fn axbacktrace_frame_format_and_adjust_rules_hold() {
    let frame = Frame {
        fp: 0x1000,
        ip: 0x2000,
    };

    ax_assert_eq!(format!("{frame}"), "fp=0x1000, ip=0x2000");
    ax_assert_eq!(
        core::mem::size_of::<Frame>(),
        2 * core::mem::size_of::<usize>()
    );
    ax_assert_eq!(
        core::mem::align_of::<Frame>(),
        core::mem::align_of::<usize>()
    );

    #[cfg(target_arch = "x86_64")]
    ax_assert_eq!(frame.adjust_ip(), 0x1fff);
    #[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
    ax_assert_eq!(frame.adjust_ip(), 0x1ffc);
    #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
    ax_assert_eq!(frame.adjust_ip(), 0x1ffe);
}

#[axtest]
fn axbacktrace_capture_buffer_boundaries_hold() {
    let mut buf = CaptureBuf::EMPTY;
    ax_assert!(buf.push(Frame { fp: 1, ip: 0x10 }));
    ax_assert!(buf.push(Frame { fp: 2, ip: 0x20 }));

    buf.insert_front(Frame { fp: 0, ip: 0x05 });
    let boxed = buf.clone().into_boxed_slice();
    ax_assert_eq!(boxed.len(), 3);
    ax_assert_eq!(boxed[0], Frame { fp: 0, ip: 0x05 });
    ax_assert_eq!(boxed[1], Frame { fp: 1, ip: 0x10 });
    ax_assert_eq!(boxed[2], Frame { fp: 2, ip: 0x20 });

    let mut full = CaptureBuf::EMPTY;
    for i in 0..CAPTURE_CAPACITY {
        ax_assert!(full.push(Frame {
            fp: i,
            ip: 0x1000 + i
        }));
    }
    ax_assert!(!full.push(Frame { fp: 0, ip: 0 }));
    full.insert_front(Frame {
        fp: 0x99,
        ip: 0x9999,
    });

    let full = full.into_boxed_slice();
    ax_assert_eq!(full.len(), CAPTURE_CAPACITY);
    ax_assert_eq!(
        full[0],
        Frame {
            fp: 0x99,
            ip: 0x9999,
        }
    );
    ax_assert_eq!(full[CAPTURE_CAPACITY - 1].ip, 0x1000 + CAPTURE_CAPACITY - 2);
}

#[axtest]
fn axbacktrace_frame_read_and_depth_rules_hold() {
    ax_assert!(Frame::read(0).is_none());

    let align = core::mem::align_of::<Frame>();
    for offset in 1..align {
        ax_assert!(Frame::read(offset).is_none());
    }

    set_max_depth(7);
    ax_assert_eq!(max_depth(), 7);
    set_max_depth(0);
    ax_assert_eq!(max_depth(), 7);
    set_max_depth(CAPTURE_CAPACITY);
}

#[axtest]
fn axbacktrace_capture_trap_inserts_and_truncates_rules_hold() {
    let backtrace = Backtrace::capture_trap(0, 0x1000, 0xbeef);
    let frames = match &backtrace.inner {
        Inner::Captured(frames) => frames,
        _ => {
            ax_assert!(false);
            unreachable!();
        }
    };
    ax_assert_eq!(frames[0].ip, 0x1001);
    ax_assert_eq!(frames.len(), 1);
}

#[axtest]
fn axbacktrace_depth_limit_and_display_rules_hold() {
    let frames = vec![
        Frame {
            fp: 0x1000,
            ip: 0x2000,
        },
        Frame {
            fp: 0x3000,
            ip: 0x4000,
        },
    ]
    .into_boxed_slice();
    let backtrace = Backtrace {
        inner: Inner::Captured(frames),
        kind: None,
    };
    let human = format!("{backtrace}");
    ax_assert!(human.contains("Backtrace:"));
    ax_assert!(human.contains("fp=0x1000, ip=0x2000"));

    let backtrace = backtrace.kind("axtest");
    let raw = format!("{backtrace}");
    ax_assert!(raw.contains("BACKTRACE_BEGIN kind=axtest"));
    ax_assert!(raw.contains("BT 0 ip=0x2000 fp=0x1000"));
    ax_assert!(raw.contains("BACKTRACE_END"));
}
