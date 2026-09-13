//! The subset of QEMU 11.1.1's plugin API v7 used by qperf.
//!
//! Source: https://github.com/qemu/qemu/blob/v11.1.1/include/plugins/qemu-plugin.h
//! Keep callback signatures and the complete register descriptor layout in sync.

use std::ffi::{c_char, c_int, c_uint, c_void};

pub const API_VERSION: c_int = 7;
pub const NO_REGS: c_uint = 0;
pub const R_REGS: c_uint = 1;

#[repr(C)]
pub struct Info {
    pub target_name: *const c_char,
    pub version: Version,
    pub system_emulation: bool,
    pub mode: Mode,
}

#[repr(C)]
pub struct Version {
    pub min: c_int,
    pub cur: c_int,
}

#[repr(C)]
pub union Mode {
    pub system: System,
}

// This union member is valid only when system_emulation is true.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct System {
    pub smp_vcpus: c_int,
    pub max_vcpus: c_int,
}

#[repr(C)]
pub struct TranslationBlock {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct Instruction {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct Register {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct RegisterDescriptor {
    pub handle: *mut Register,
    pub name: *const c_char,
    pub feature: *const c_char,
    pub is_readonly: bool,
}

#[repr(C)]
pub struct GArray {
    pub data: *mut c_char,
    pub len: c_uint,
}

#[repr(C)]
pub struct GByteArray {
    pub data: *mut u8,
    pub len: c_uint,
}

type VcpuCallback = unsafe extern "C" fn(c_uint, *mut c_void);
type Callback = unsafe extern "C" fn(*mut c_void);
type TranslateCallback = unsafe extern "C" fn(*mut TranslationBlock, *mut c_void);

// QEMU and its GLib dependency resolve these symbols when loading the plugin.
unsafe extern "C" {
    pub fn qemu_plugin_register_vcpu_init_cb(id: u64, cb: VcpuCallback, userdata: *mut c_void);
    pub fn qemu_plugin_register_vcpu_exit_cb(id: u64, cb: VcpuCallback, userdata: *mut c_void);
    pub fn qemu_plugin_register_vcpu_tb_trans_cb(
        id: u64,
        cb: TranslateCallback,
        userdata: *mut c_void,
    );
    pub fn qemu_plugin_register_flush_cb(id: u64, cb: Callback, userdata: *mut c_void);
    pub fn qemu_plugin_register_atexit_cb(id: u64, cb: Callback, userdata: *mut c_void);
    pub fn qemu_plugin_register_vcpu_tb_exec_cb(
        tb: *mut TranslationBlock,
        cb: VcpuCallback,
        flags: c_uint,
        userdata: *mut c_void,
    );
    pub fn qemu_plugin_register_vcpu_insn_exec_cb(
        insn: *mut Instruction,
        cb: VcpuCallback,
        flags: c_uint,
        userdata: *mut c_void,
    );
    pub fn qemu_plugin_tb_vaddr(tb: *const TranslationBlock) -> u64;
    pub fn qemu_plugin_tb_n_insns(tb: *const TranslationBlock) -> usize;
    pub fn qemu_plugin_tb_get_insn(tb: *const TranslationBlock, index: usize) -> *mut Instruction;
    pub fn qemu_plugin_insn_vaddr(insn: *const Instruction) -> u64;
    pub fn qemu_plugin_get_registers() -> *mut GArray;
    pub fn qemu_plugin_read_register(register: *mut Register, output: *mut GByteArray) -> bool;
    pub fn qemu_plugin_read_memory_vaddr(addr: u64, output: *mut GByteArray, len: usize) -> bool;
    pub fn g_array_free(array: *mut GArray, free_segment: c_int) -> *mut c_char;
    pub fn g_byte_array_new() -> *mut GByteArray;
    pub fn g_byte_array_free(array: *mut GByteArray, free_segment: c_int) -> *mut u8;
}
