//! Load-time structural validation for BPF programs.
//!
//! `kbpf-basic`'s preprocessor walks the instruction stream the kernel was
//! handed by `bpf(2)` and reads the slot after every wide load without checking
//! that the stream has one, so a stream it cannot walk panics inside the
//! preprocessor. The bytes come straight from userspace, so a malformed program
//! would be a kernel panic any process could ask for. The stream is therefore
//! walked here, before that stage, with the same rules the preprocessor uses.

/// A stream the preprocessor cannot walk: it is empty, it is not a whole number
/// of instructions, or it ends with a wide load whose operand slot is missing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MalformedStream {
    /// Length of the rejected stream in bytes.
    pub(crate) length: usize,
}

/// Number of whole instructions in `insns`, or `None` when the stream is empty
/// or is not a whole number of instructions.
fn instruction_count(insns: &[u8]) -> Option<usize> {
    if insns.is_empty() || !insns.len().is_multiple_of(rbpf::ebpf::INSN_SIZE) {
        return None;
    }
    Some(insns.len() / rbpf::ebpf::INSN_SIZE)
}

/// Rejects an instruction stream that is not well formed enough for the
/// preprocessor.
///
/// The preprocessor reads the slot after a wide load before it decides whether
/// that slot is part of the instruction: it consumes the slot only when it
/// relocates a map into it, and any other source leaves the slot in the stream
/// for the walk to read as an instruction of its own. Both cases are walked
/// here, in the order the preprocessor takes them.
pub(crate) fn check_structure(insns: &[u8]) -> Result<(), MalformedStream> {
    let Some(count) = instruction_count(insns) else {
        return Err(MalformedStream {
            length: insns.len(),
        });
    };

    let mut index = 0;
    while index < count {
        let insn = rbpf::ebpf::get_insn(insns, index);
        if insn.opc == rbpf::ebpf::LD_DW_IMM {
            // The operand slot is read before the source field is examined.
            if index + 1 >= count {
                return Err(MalformedStream {
                    length: insns.len(),
                });
            }
            // `BPF_PSEUDO_MAP_FD` and `BPF_PSEUDO_MAP_VALUE` are the two sources
            // the preprocessor relocates, and relocation is what makes it take
            // the operand slot as part of this instruction. Any other source
            // leaves the slot for the next iteration to examine.
            let relocated = matches!(
                u32::from(insn.src),
                kbpf_basic::linux_bpf::BPF_PSEUDO_MAP_FD
                    | kbpf_basic::linux_bpf::BPF_PSEUDO_MAP_VALUE
            );
            index += if relocated { 2 } else { 1 };
            continue;
        }
        index += 1;
    }
    Ok(())
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use alloc::vec::Vec;

    use super::{MalformedStream, check_structure};

    const MOV64_IMM: u8 = 0xb7;
    const EXIT: u8 = 0x95;
    const LD_DW_IMM: u8 = 0x18;

    fn insn(opc: u8, dst: u8, src: u8, off: i16, imm: i32) -> [u8; 8] {
        let mut bytes = [0u8; 8];
        bytes[0] = opc;
        bytes[1] = (src << 4) | dst;
        bytes[2..4].copy_from_slice(&off.to_le_bytes());
        bytes[4..8].copy_from_slice(&imm.to_le_bytes());
        bytes
    }

    fn program(insns: &[[u8; 8]]) -> Vec<u8> {
        insns.iter().flatten().copied().collect()
    }

    #[test]
    fn wide_load_without_an_operand_slot_is_rejected_before_preprocessing() {
        // The preprocessor indexes the slot after every wide load, so this
        // stream panics there unless it is rejected first.
        let prog = program(&[insn(LD_DW_IMM, 1, 0, 0, 0)]);
        assert_eq!(
            check_structure(&prog),
            Err(MalformedStream { length: 8 })
        );
    }

    #[test]
    fn wide_load_pair_without_relocation_is_rejected() {
        // The operand slot of a wide load is consumed only when a map is
        // relocated into it, so with a plain immediate the second wide load is
        // read as an instruction of its own and asks for a slot past the end.
        let prog = program(&[
            insn(LD_DW_IMM, 0, 0, 0, 0),
            insn(LD_DW_IMM, 0, 0, 0, 0),
        ]);
        assert_eq!(
            check_structure(&prog),
            Err(MalformedStream { length: 16 })
        );
    }

    #[test]
    fn partial_instruction_is_rejected() {
        // The empty stream reaches the check through its zero length, which is
        // a multiple of the instruction size, so the remainder case needs a
        // stream of its own.
        let prog = program(&[insn(MOV64_IMM, 0, 0, 0, 1), insn(EXIT, 0, 0, 0, 0)]);
        assert_eq!(
            check_structure(&prog[..prog.len() - 1]),
            Err(MalformedStream { length: 15 })
        );
    }

    #[test]
    fn well_formed_streams_pass_the_structural_check() {
        let prog = program(&[
            insn(LD_DW_IMM, 1, 0, 0, 0),
            insn(MOV64_IMM, 0, 0, 0, 0),
            insn(EXIT, 0, 0, 0, 0),
        ]);
        assert_eq!(check_structure(&prog), Ok(()));
        assert_eq!(check_structure(&[]), Err(MalformedStream { length: 0 }));
    }
}
