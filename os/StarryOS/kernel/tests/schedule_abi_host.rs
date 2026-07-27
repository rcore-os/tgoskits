//! Host-only gate for the pure Linux scheduling ABI conversion layer.

// The production `ax-std` facade carries platform symbols in its dependency
// graph even though this test only uses scheduler value types. Host tests do
// not execute any platform path, so one-byte anchors are sufficient to satisfy
// those otherwise linker-script-provided symbol references.
#[unsafe(no_mangle)]
static _percpu_load_start: u8 = 0;
#[unsafe(no_mangle)]
static __percpu_start: u8 = 0;
#[unsafe(no_mangle)]
static __percpu_end: u8 = 0;
#[unsafe(no_mangle)]
static STACK_SIZE: u8 = 0;
#[unsafe(no_mangle)]
static PAGE_SIZE: u8 = 0;

#[path = "../src/syscall/task/schedule_abi.rs"]
mod schedule_abi;
