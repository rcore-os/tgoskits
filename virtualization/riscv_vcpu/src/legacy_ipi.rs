use crate::{RiscvVmExit, consts::traps::irq::S_SOFT};

pub(crate) fn decode_legacy_send_ipi_exit(
    hart_mask_ptr: usize,
    copy_from_guest_va: impl FnMut(usize, &mut [u8]) -> usize,
) -> Result<RiscvVmExit, crate::sbi_ipi::HartMaskReadError> {
    let hart_mask = crate::sbi_ipi::read_hart_mask(hart_mask_ptr, copy_from_guest_va)?;

    Ok(RiscvVmExit::SendIPI {
        target_cpu: hart_mask as u64,
        target_cpu_aux: 0,
        send_to_all: false,
        send_to_self: false,
        vector: S_SOFT as u64,
    })
}

#[cfg(test)]
mod legacy_ipi_tests {
    use super::*;

    #[test]
    fn decode_legacy_send_ipi_reads_guest_mask_pointer() {
        let guest_mask_addr = 0x4000usize;
        let guest_hart_mask = 1usize << 5;
        let mask_bytes = guest_hart_mask.to_ne_bytes();
        let mut reads = 0usize;

        let exit = decode_legacy_send_ipi_exit(guest_mask_addr, |guest_va, bytes| {
            reads += 1;
            assert_eq!(guest_va, guest_mask_addr);
            bytes.copy_from_slice(&mask_bytes);
            bytes.len()
        })
        .unwrap();

        assert_eq!(reads, 1);

        match exit {
            RiscvVmExit::SendIPI {
                target_cpu,
                target_cpu_aux,
                send_to_all,
                send_to_self,
                vector,
            } => {
                assert_eq!(target_cpu, guest_hart_mask as u64);
                assert_eq!(target_cpu_aux, 0);
                assert!(!send_to_all);
                assert!(!send_to_self);
                assert_eq!(vector, S_SOFT as u64);
            }
            _ => panic!("legacy SEND_IPI must return SendIPI"),
        }
    }
}
