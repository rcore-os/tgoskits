use super::super::{bpf_insn::BpfInsn, HelperFn};
use super::{JitBackend, JitBuffer};

pub(crate) struct X86_64Backend;

impl JitBackend for X86_64Backend {
    fn emit_prologue(_buf: &mut JitBuffer) -> usize {
        0
    }
    fn emit_epilogue(_buf: &mut JitBuffer) {}
    fn emit_alu(_buf: &mut JitBuffer, _insn: &BpfInsn, _is_64: bool) {}
    fn emit_jmp(
        _buf: &mut JitBuffer,
        _insn: &BpfInsn,
        _offsets: &[usize],
        _pc: usize,
        _is_64: bool,
    ) {
    }
    fn emit_st(_buf: &mut JitBuffer, _insn: &BpfInsn) {}
    fn emit_stx(_buf: &mut JitBuffer, _insn: &BpfInsn) {}
    fn emit_ldx(_buf: &mut JitBuffer, _insn: &BpfInsn) {}
    fn emit_ld_imm64(_buf: &mut JitBuffer, _insn: &BpfInsn, _next_imm: i32) {}
    fn emit_call(_buf: &mut JitBuffer, _helper_fn: HelperFn) {}
    fn insn_size(_insn: &BpfInsn) -> usize {
        0
    }
}
