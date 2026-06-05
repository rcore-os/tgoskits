use alloc::{
    alloc::{Layout, alloc_zeroed, dealloc},
    collections::BTreeMap,
    vec,
    vec::Vec,
};

#[cfg(target_arch = "aarch64")]
use ax_memory_addr::VirtAddr;

pub(crate) use super::HelperFn;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub(crate) use super::bpf_insn;
use super::bpf_insn::{
    BPF_ALU, BPF_ALU64, BPF_EXIT, BPF_JMP, BPF_JMP32, BPF_LD, BPF_LDX, BPF_ST, BPF_STX, BpfInsn,
};

#[cfg(target_arch = "aarch64")]
mod jit_aarch64;
#[cfg(target_arch = "riscv64")]
mod jit_riscv64;
#[cfg(target_arch = "x86_64")]
mod jit_x86_64;

#[cfg(target_arch = "aarch64")]
use jit_aarch64::Aarch64Backend as Backend;
#[cfg(target_arch = "riscv64")]
use jit_riscv64::Riscv64Backend as Backend;
#[cfg(target_arch = "x86_64")]
use jit_x86_64::X86_64Backend as Backend;

pub struct JitBuffer {
    ptr: *mut u8,
    size: usize,
    pos: usize,
}

unsafe impl Send for JitBuffer {}
unsafe impl Sync for JitBuffer {}

impl core::fmt::Debug for JitBuffer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("JitBuffer")
            .field("size", &self.size)
            .field("pos", &self.pos)
            .finish()
    }
}

impl JitBuffer {
    pub fn new(requested_size: usize) -> Result<Self, &'static str> {
        let size = (requested_size + 4095) & !4095;
        if size == 0 {
            return Err("jit buffer size is zero");
        }
        let layout = Layout::from_size_align(size, 4096).map_err(|_| "invalid layout")?;
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            return Err("jit buffer allocation failed");
        }
        Ok(Self { ptr, size, pos: 0 })
    }

    #[cfg(target_arch = "riscv64")]
    pub fn emit_u32(&mut self, val: u32) {
        if self.pos + 4 > self.size {
            return;
        }
        unsafe {
            let dst = self.ptr.add(self.pos) as *mut u32;
            *dst = val.to_le();
        }
        self.pos += 4;
    }

    #[cfg(target_arch = "riscv64")]
    pub fn offset(&self) -> usize {
        self.pos
    }

    pub fn entry(&self) -> *const u8 {
        self.ptr
    }

    pub fn finalize(&mut self) {
        #[cfg(target_arch = "aarch64")]
        {
            let vaddr = VirtAddr::from_usize(self.ptr as usize);
            ax_runtime::hal::cpu::asm::clean_dcache_range_to_pou(vaddr, self.pos);
        }
        ax_runtime::hal::cpu::asm::flush_icache_all();
    }
}

impl Drop for JitBuffer {
    fn drop(&mut self) {
        if !self.ptr.is_null() && self.size > 0 {
            let layout = Layout::from_size_align(self.size, 4096).unwrap();
            unsafe {
                dealloc(self.ptr, layout);
            }
        }
    }
}

pub(crate) trait JitBackend {
    fn emit_prologue(buf: &mut JitBuffer) -> usize;
    fn emit_epilogue(buf: &mut JitBuffer);
    fn emit_alu(buf: &mut JitBuffer, insn: &BpfInsn, is_64: bool);
    fn emit_jmp(buf: &mut JitBuffer, insn: &BpfInsn, offsets: &[usize], pc: usize, is_64: bool);
    fn emit_st(buf: &mut JitBuffer, insn: &BpfInsn);
    fn emit_stx(buf: &mut JitBuffer, insn: &BpfInsn);
    fn emit_ldx(buf: &mut JitBuffer, insn: &BpfInsn);
    fn emit_ld_imm64(buf: &mut JitBuffer, insn: &BpfInsn, next_imm: i32);
    fn emit_call(buf: &mut JitBuffer, helper_fn: HelperFn);
    fn insn_size(insn: &BpfInsn) -> usize;
}

struct JitCompiler<'a> {
    insns: &'a [BpfInsn],
    offsets: Vec<usize>,
    helpers: &'a BTreeMap<u32, HelperFn>,
}

impl<'a> JitCompiler<'a> {
    fn new(insns: &'a [BpfInsn], helpers: &'a BTreeMap<u32, HelperFn>) -> Self {
        let offsets = vec![0; insns.len()];
        Self {
            insns,
            offsets,
            helpers,
        }
    }

    fn pass1_sizing(&mut self) -> usize {
        let mut insn_size_total: usize = 0;
        let num_insns = self.insns.len();
        let mut pc: usize = 0;
        while pc < num_insns {
            self.offsets[pc] = insn_size_total;
            let insn = &self.insns[pc];
            if insn.is_ld_dw_imm() {
                let sz = Backend::insn_size(insn);
                insn_size_total += sz;
                if pc + 1 < num_insns {
                    self.offsets[pc + 1] = insn_size_total;
                }
                pc += 2;
            } else {
                insn_size_total += Backend::insn_size(insn);
                pc += 1;
            }
        }
        insn_size_total
    }

    fn compile(&mut self) -> Result<JitBuffer, &'static str> {
        if self.insns.is_empty() {
            return Err("no instructions to compile");
        }

        let insn_size_total = self.pass1_sizing();

        let estimated = 128 + insn_size_total + 128 + 256;
        let mut buf = JitBuffer::new(estimated)?;

        let prologue_size = Backend::emit_prologue(&mut buf);

        for i in 0..self.offsets.len() {
            self.offsets[i] += prologue_size;
        }

        let num_insns = self.insns.len();
        let mut pc: usize = 0;
        while pc < num_insns {
            let insn = &self.insns[pc];
            let class = insn.class();

            match class {
                BPF_ALU | BPF_ALU64 => {
                    let is_64 = class == BPF_ALU64;
                    Backend::emit_alu(&mut buf, insn, is_64);
                    pc += 1;
                }
                BPF_JMP | BPF_JMP32 => {
                    let op = insn.code & 0xf0;
                    if op == BPF_EXIT {
                        Backend::emit_epilogue(&mut buf);
                        pc += 1;
                    } else if op == 0x80 {
                        let helper_id = insn.imm as u32;
                        if let Some(&helper_fn) = self.helpers.get(&helper_id) {
                            Backend::emit_call(&mut buf, helper_fn);
                        } else {
                            Backend::emit_call(&mut buf, |_a1, _a2, _a3, _a4, _a5| u64::MAX);
                        }
                        pc += 1;
                    } else {
                        let is_64 = class == BPF_JMP;
                        Backend::emit_jmp(&mut buf, insn, &self.offsets, pc, is_64);
                        pc += 1;
                    }
                }
                BPF_ST => {
                    Backend::emit_st(&mut buf, insn);
                    pc += 1;
                }
                BPF_STX => {
                    Backend::emit_stx(&mut buf, insn);
                    pc += 1;
                }
                BPF_LDX => {
                    Backend::emit_ldx(&mut buf, insn);
                    pc += 1;
                }
                BPF_LD => {
                    if insn.is_ld_dw_imm() {
                        let next_imm = if pc + 1 < num_insns {
                            self.insns[pc + 1].imm
                        } else {
                            0
                        };
                        Backend::emit_ld_imm64(&mut buf, insn, next_imm);
                        pc += 2;
                    } else {
                        Backend::emit_ldx(&mut buf, insn);
                        pc += 1;
                    }
                }
                _ => {
                    return Err("unsupported instruction class");
                }
            }
        }

        Backend::emit_epilogue(&mut buf);
        buf.finalize();
        Ok(buf)
    }
}

pub fn try_jit_compile(insns: &[BpfInsn], helpers: &BTreeMap<u32, HelperFn>) -> Option<JitBuffer> {
    let mut compiler = JitCompiler::new(insns, helpers);
    compiler.compile().ok()
}
