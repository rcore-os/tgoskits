// Copyright 2026 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::types::{X86AccessWidth, X86GuestPhysAddr, X86GuestVirtAddr, X86VmExit};

/// Byte register selected by a ModRM.reg field for byte MOV instructions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct X86ByteRegister {
    /// General-purpose register index in the vCPU register file.
    pub(crate) gpr: u8,
    /// When true, the access targets the high byte (AH/CH/DH/BH).
    pub(crate) high: bool,
}

/// Decodes the byte register referenced by `modrm_reg` and a REX prefix byte.
///
/// Without REX, `modrm_reg` 0..3 selects AL/CL/DL/BL and 4..7 selects
/// AH/CH/DH/BH. With REX, `modrm_reg` plus REX.R selects the low byte of the
/// corresponding 64-bit general-purpose register, including SPL/BPL/SIL/DIL.
pub(crate) fn x86_byte_register(modrm_reg: u8, rex: u8) -> Option<X86ByteRegister> {
    if modrm_reg > 7 {
        return None;
    }
    if rex == 0 {
        Some(if modrm_reg < 4 {
            X86ByteRegister {
                gpr: modrm_reg,
                high: false,
            }
        } else {
            X86ByteRegister {
                gpr: modrm_reg - 4,
                high: true,
            }
        })
    } else {
        Some(X86ByteRegister {
            gpr: modrm_reg | ((rex & 0x4) << 1),
            high: false,
        })
    }
}

/// Extracts the byte selected by `byte_reg` from a full GPR value.
pub(crate) fn x86_byte_register_value(gpr_value: u64, byte_reg: X86ByteRegister) -> u8 {
    if byte_reg.high {
        (gpr_value >> 8) as u8
    } else {
        gpr_value as u8
    }
}

/// Merges a byte into a full GPR value without disturbing adjacent bytes.
pub(crate) fn x86_byte_register_merge(gpr_value: u64, byte_reg: X86ByteRegister, value: u8) -> u64 {
    if byte_reg.high {
        (gpr_value & !0xff00) | (u64::from(value) << 8)
    } else {
        (gpr_value & !0xff) | u64::from(value)
    }
}

/// Merges a 16-bit value into a full GPR value while preserving the upper
/// bits, matching the x86 `mov r16, m16` write semantics.
pub(crate) fn x86_word_register_merge(gpr_value: u64, value: u16) -> u64 {
    (gpr_value & !0xffff) | u64::from(value)
}

/// Merges an MMIO read result into the architectural RSP according to the
/// destination-operand width.
pub(crate) fn x86_rsp_merge(old_rsp: u64, width: X86AccessWidth, value: u64) -> u64 {
    match width {
        X86AccessWidth::Byte => x86_byte_register_merge(
            old_rsp,
            X86ByteRegister {
                gpr: 4,
                high: false,
            },
            value as u8,
        ),
        X86AccessWidth::Word => x86_word_register_merge(old_rsp, value as u16),
        X86AccessWidth::Dword => u64::from(value as u32),
        X86AccessWidth::Qword => value,
    }
}

/// Applies one x86 instruction-prefix byte to decoder state.
///
/// Returns true when `byte` is a supported prefix. Legacy prefixes after a REX
/// prefix invalidate that REX, matching the x86 rule that only the last REX
/// immediately preceding the opcode is effective.
pub(crate) fn x86_simple_prefix_update(
    byte: u8,
    rex: &mut u8,
    operand_size_override: &mut bool,
) -> bool {
    if byte == 0x66 {
        *operand_size_override = true;
        *rex = 0;
        true
    } else if (0x40..=0x4f).contains(&byte) {
        *rex = byte;
        true
    } else {
        false
    }
}

/// Returns the displacement size encoded by a memory-operand ModRM byte.
pub(crate) fn x86_modrm_displacement_size(modrm: u8, sib: Option<u8>, rex: u8) -> Option<usize> {
    let mode = modrm >> 6;
    let rm = modrm & 0x7;
    if mode == 0b11 {
        return None;
    }

    Some(match mode {
        0 => {
            let is_rip_relative = if rm == 0b100 {
                let sib = sib?;
                (sib & 0x7) == 0b101
            } else {
                rm == 0b101
            };
            if is_rip_relative && rex & 0x1 == 0 {
                4
            } else {
                0
            }
        }
        1 => 1,
        2 => 4,
        _ => return None,
    })
}

/// Builds an [`X86VmExit::MmioWrite`] for a decoded MOV register-to-memory
/// device MMIO instruction.
pub(crate) fn mov_mmio_write_exit(
    addr: X86GuestPhysAddr,
    opcode: u8,
    operand_size_override: bool,
    rex_w: bool,
    data: u64,
) -> Option<X86VmExit> {
    let width = X86AccessWidth::for_mov_opcode(opcode, operand_size_override, rex_w)?;
    match opcode {
        0x88 | 0x89 => Some(X86VmExit::MmioWrite {
            addr,
            width,
            data: width.mask_value(data),
        }),
        _ => None,
    }
}

/// Decoded instruction context for the LAPIC/IOAPIC MMIO fast path.
pub(crate) struct X86ApicMmioDecode {
    pub(crate) start: X86GuestVirtAddr,
    pub(crate) rip: X86GuestVirtAddr,
    pub(crate) modrm: u8,
    pub(crate) rex: u8,
    pub(crate) opcode: u8,
    pub(crate) addr: X86GuestPhysAddr,
    pub(crate) write: bool,
    pub(crate) local_apic: bool,
}

/// Returns the encoded immediate size of a MOV memory-store instruction.
pub(crate) fn mov_immediate_size(width: X86AccessWidth) -> usize {
    match width {
        X86AccessWidth::Byte => core::mem::size_of::<u8>(),
        X86AccessWidth::Word => core::mem::size_of::<u16>(),
        // C7 with REX.W still encodes a 32-bit immediate.
        X86AccessWidth::Dword | X86AccessWidth::Qword => core::mem::size_of::<u32>(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_register_decodes_high_bytes_without_rex() {
        assert_eq!(
            x86_byte_register(0, 0),
            Some(X86ByteRegister {
                gpr: 0,
                high: false
            })
        );
        assert_eq!(
            x86_byte_register(3, 0),
            Some(X86ByteRegister {
                gpr: 3,
                high: false
            })
        );
        assert_eq!(
            x86_byte_register(4, 0),
            Some(X86ByteRegister { gpr: 0, high: true })
        );
        assert_eq!(
            x86_byte_register(5, 0),
            Some(X86ByteRegister { gpr: 1, high: true })
        );
        assert_eq!(
            x86_byte_register(6, 0),
            Some(X86ByteRegister { gpr: 2, high: true })
        );
        assert_eq!(
            x86_byte_register(7, 0),
            Some(X86ByteRegister { gpr: 3, high: true })
        );
    }

    #[test]
    fn byte_register_decodes_rex_low_bytes_including_spl() {
        assert_eq!(
            x86_byte_register(4, 0x40),
            Some(X86ByteRegister {
                gpr: 4,
                high: false
            })
        );
        assert_eq!(
            x86_byte_register(4, 0x44),
            Some(X86ByteRegister {
                gpr: 12,
                high: false
            })
        );
        assert_eq!(
            x86_byte_register(5, 0x41),
            Some(X86ByteRegister {
                gpr: 5,
                high: false
            })
        );
        assert_eq!(
            x86_byte_register(1, 0x44),
            Some(X86ByteRegister {
                gpr: 9,
                high: false
            })
        );
    }

    #[test]
    fn byte_register_value_extracts_high_and_low_bytes() {
        let ah = X86ByteRegister { gpr: 0, high: true };
        assert_eq!(x86_byte_register_value(0x1234_5678_9abc_def0, ah), 0xde);
        let al = X86ByteRegister {
            gpr: 0,
            high: false,
        };
        assert_eq!(x86_byte_register_value(0x1234_5678_9abc_def0, al), 0xf0);
        let ch = X86ByteRegister { gpr: 1, high: true };
        assert_eq!(x86_byte_register_value(0x1234_5678_9abc_def0, ch), 0xde);
    }

    #[test]
    fn byte_register_merge_preserves_adjacent_bytes() {
        let ah = X86ByteRegister { gpr: 0, high: true };
        assert_eq!(
            x86_byte_register_merge(0x1234_5678_9abc_def0, ah, 0x11),
            0x1234_5678_9abc_11f0
        );
        let al = X86ByteRegister {
            gpr: 0,
            high: false,
        };
        assert_eq!(
            x86_byte_register_merge(0x1234_5678_9abc_def0, al, 0x11),
            0x1234_5678_9abc_de11
        );
        let bpl = X86ByteRegister {
            gpr: 5,
            high: false,
        };
        assert_eq!(
            x86_byte_register_merge(0x1234_5678_9abc_def0, bpl, 0x11),
            0x1234_5678_9abc_de11
        );
    }

    #[test]
    fn rsp_merge_obeys_destination_width() {
        assert_eq!(
            x86_rsp_merge(0x1234_5678_9abc_def0, X86AccessWidth::Byte, 0x11),
            0x1234_5678_9abc_de11
        );
        assert_eq!(
            x86_rsp_merge(0x1234_5678_9abc_def0, X86AccessWidth::Word, 0x1111),
            0x1234_5678_9abc_1111
        );
        assert_eq!(
            x86_rsp_merge(0x1234_5678_9abc_def0, X86AccessWidth::Dword, 0x1111_1111),
            0x1111_1111
        );
        assert_eq!(
            x86_rsp_merge(
                0x1234_5678_9abc_def0,
                X86AccessWidth::Qword,
                0x1111_1111_2222_2222
            ),
            0x1111_1111_2222_2222
        );
    }

    #[test]
    fn word_register_merge_preserves_upper_bits() {
        assert_eq!(
            x86_word_register_merge(0x1234_5678_9abc_def0, 0x1111),
            0x1234_5678_9abc_1111
        );
    }

    #[test]
    fn simple_prefix_update_clears_rex_after_operand_size_override() {
        let mut rex = 0x40;
        let mut operand_size_override = false;

        assert!(x86_simple_prefix_update(
            0x66,
            &mut rex,
            &mut operand_size_override
        ));
        assert_eq!(rex, 0);
        assert!(operand_size_override);

        assert!(x86_simple_prefix_update(
            0x40,
            &mut rex,
            &mut operand_size_override
        ));
        assert_eq!(rex, 0x40);
        assert!(operand_size_override);
    }

    #[test]
    fn simple_prefix_update_keeps_last_rex_before_opcode() {
        let mut rex = 0;
        let mut operand_size_override = false;

        assert!(x86_simple_prefix_update(
            0x66,
            &mut rex,
            &mut operand_size_override
        ));
        assert!(x86_simple_prefix_update(
            0x48,
            &mut rex,
            &mut operand_size_override
        ));
        assert_eq!(rex, 0x48);

        assert!(x86_simple_prefix_update(
            0x66,
            &mut rex,
            &mut operand_size_override
        ));
        assert_eq!(rex, 0);
    }

    #[test]
    fn modrm_displacement_size_handles_sib_rip_relative_and_r13() {
        assert_eq!(x86_modrm_displacement_size(0x04, Some(0x25), 0), Some(4));
        assert_eq!(x86_modrm_displacement_size(0x04, Some(0x25), 1), Some(0));
        assert_eq!(x86_modrm_displacement_size(0x05, None, 0), Some(4));
        assert_eq!(x86_modrm_displacement_size(0x05, None, 1), Some(0));
        assert_eq!(x86_modrm_displacement_size(0x40, None, 0), Some(1));
        assert_eq!(x86_modrm_displacement_size(0x80, None, 0), Some(4));
        assert_eq!(x86_modrm_displacement_size(0xc0, None, 0), None);
    }

    #[test]
    fn mov_mmio_write_exit_decodes_byte_word_dword_widths() {
        let addr = X86GuestPhysAddr::from_usize(0x8000_0014);
        let cases: [(u8, bool, bool, X86AccessWidth, u64); 3] = [
            (0x88, false, false, X86AccessWidth::Byte, 0x5a),
            (0x89, true, false, X86AccessWidth::Word, 0x5678),
            (0x89, false, false, X86AccessWidth::Dword, 0x1234_5678),
        ];

        for (opcode, operand_size_override, rex_w, width, expected_data) in cases {
            let data = match width {
                X86AccessWidth::Byte => 0x1234_5678_5a,
                X86AccessWidth::Word => 0x1234_5678,
                X86AccessWidth::Dword => 0x1234_5678,
                X86AccessWidth::Qword => unreachable!(),
            };
            let exit =
                mov_mmio_write_exit(addr, opcode, operand_size_override, rex_w, data).unwrap();

            match exit {
                X86VmExit::MmioWrite {
                    addr: actual_addr,
                    width: actual_width,
                    data: actual_data,
                } => {
                    assert_eq!(actual_addr, addr);
                    assert_eq!(actual_width, width);
                    assert_eq!(actual_data, expected_data);
                }
                other => panic!("unexpected MMIO exit: {other:?}"),
            }
        }
    }
}
