//! eBPF preprocessor for relocating map file descriptors in eBPF instructions.
//!
//! This module defines the `EbpfPreProcessor` struct and the `EbpfInst` trait.
//!
//! The `EbpfPreProcessor` struct is used to preprocess eBPF instructions to relocate map file descriptors, and the `EbpfInst` trait defines the interface for eBPF instructions that can be processed by the preprocessor.
//!
//! The preprocessor works by translating the raw eBPF instructions into a more structured format, identifying instructions that reference map file descriptors, and replacing those references with the actual pointers to the maps in kernel space. This allows the eBPF program to access the maps correctly when it is loaded into the kernel. The preprocessor also keeps track of the raw file pointers for the maps that are used in the program, which can be used for debugging or other purposes.
use alloc::{vec, vec::Vec};

use crate::{
    BpfResult as Result, KernelAuxiliaryOps,
    linux_bpf::{BPF_PSEUDO_MAP_FD, BPF_PSEUDO_MAP_VALUE},
};

/// eBPF preprocessor for relocating map file descriptors in eBPF instructions.
pub struct EbpfPreProcessor {
    new_insn: Vec<u8>,
    raw_file_ptr: Vec<usize>,
}

/// Trait for eBPF instructions that can be processed by the preprocessor.
pub trait EbpfInst: Clone {
    /// Get the opcode of the instruction.
    fn opc(&self) -> u8;
    /// Get the destination register of the instruction.
    fn imm(&self) -> i32;
    /// Get the source register of the instruction.
    fn src(&self) -> u8;
    /// set the immediate value of the instruction.
    fn set_imm(&mut self, imm: i32);
    /// Convert the instruction to a byte array.
    fn to_array(&self) -> [u8; 8];
}

const LD_DW_IMM: u8 = 0x18;

impl EbpfPreProcessor {
    /// Preprocess the instructions to relocate the map file descriptors.
    pub fn preprocess<F: KernelAuxiliaryOps>(mut instructions: Vec<u8>) -> Result<Self> {
        let mut fmt_insn = F::translate_instruction(instructions.clone())?;
        let mut index = 0;
        let mut raw_file_ptr = vec![];
        loop {
            if index >= fmt_insn.len() {
                break;
            }
            let mut insn = fmt_insn[index].clone();
            if insn.opc() == LD_DW_IMM {
                // relocate the instruction
                let mut next_insn = fmt_insn[index + 1].clone();
                // the imm is the map_fd because user lib has already done the relocation
                let map_fd = insn.imm() as usize;
                let src_reg = insn.src();
                // See https://www.kernel.org/doc/html/latest/bpf/standardization/instruction-set.html#id23
                let ptr = match src_reg as u32 {
                    BPF_PSEUDO_MAP_VALUE => {
                        // dst = map_val(map_by_fd(imm)) + next_imm
                        // map_val(map) gets the address of the first value in a given map
                        let value_ptr = F::get_unified_map_from_fd(map_fd as u32, |unified_map| {
                            unified_map.map().map_values_ptr_range()
                        })?;
                        let offset = next_insn.imm() as usize;
                        log::trace!(
                            "Relocate for BPF_PSEUDO_MAP_VALUE, instruction index: {}, map_fd: \
                             {}, ptr: {:#x}, offset: {}",
                            index,
                            map_fd,
                            value_ptr.start,
                            offset
                        );
                        Some(value_ptr.start + offset)
                    }
                    BPF_PSEUDO_MAP_FD => {
                        let map_ptr = F::get_unified_map_ptr_from_fd(map_fd as u32)? as usize;
                        log::trace!(
                            "Relocate for BPF_PSEUDO_MAP_FD, instruction index: {}, map_fd: {}, \
                             ptr: {:#x}",
                            index,
                            map_fd,
                            map_ptr
                        );
                        raw_file_ptr.push(map_ptr);
                        Some(map_ptr)
                    }
                    ty => {
                        log::error!(
                            "relocation for ty: {} not implemented, instruction index: {}",
                            ty,
                            index
                        );
                        None
                    }
                };
                if let Some(ptr) = ptr {
                    // The current ins store the map_data_ptr low 32 bits,
                    // the next ins store the map_data_ptr high 32 bits
                    insn.set_imm(ptr as i32);
                    next_insn.set_imm((ptr >> 32) as i32);
                    fmt_insn[index] = insn;
                    fmt_insn[index + 1] = next_insn;
                    index += 2;
                } else {
                    index += 1;
                }
            } else {
                index += 1;
            }
        }
        let mut idx = 0;
        for ins in fmt_insn {
            let bytes = ins.to_array();
            instructions[idx..idx + 8].copy_from_slice(&bytes);
            idx += 8;
        }
        Ok(Self {
            new_insn: instructions,
            raw_file_ptr,
        })
    }

    /// Get the new instructions after preprocessing.
    pub fn get_new_insn(&self) -> &Vec<u8> {
        self.new_insn.as_ref()
    }

    /// Get the raw file pointer after preprocessing.
    /// The raw file pointer is a list of pointers to the maps that are used in the program.
    /// The pointers are used to access the maps in the program.
    pub fn get_raw_file_ptr(&self) -> &Vec<usize> {
        self.raw_file_ptr.as_ref()
    }
}
