//! SBI IPI decoding owned by the RISC-V vCPU boundary.

use rustsbi::Ipi;
use sbi_spec::binary::{HartMask, SbiRet};

use crate::types::{RiscvIpiAbi, RiscvIpiRequest};

/// IPI provider used to advertise the SBI extension through BASE probing.
///
/// Actual delivery is deferred to AxVM because only the VMM owns the guest
/// hart topology and target-vCPU runtime queues.
#[derive(Clone, Copy, Default)]
pub(crate) struct VirtualSbiIpi;

impl Ipi for VirtualSbiIpi {
    fn send_ipi(&self, _hart_mask: HartMask) -> SbiRet {
        SbiRet::not_supported()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HartMaskReadError {
    ShortRead { expected: usize, copied: usize },
}

pub(crate) fn decode_standard_request(hart_mask: usize, hart_mask_base: usize) -> RiscvIpiRequest {
    RiscvIpiRequest::new(hart_mask, hart_mask_base, RiscvIpiAbi::SbiV02)
}

pub(crate) fn decode_legacy_request(
    hart_mask_ptr: usize,
    mut copy_from_guest_va: impl FnMut(usize, &mut [u8]) -> usize,
) -> Result<RiscvIpiRequest, HartMaskReadError> {
    let mut mask_bytes = [0u8; core::mem::size_of::<usize>()];
    let copied = copy_from_guest_va(hart_mask_ptr, &mut mask_bytes);
    if copied != mask_bytes.len() {
        return Err(HartMaskReadError::ShortRead {
            expected: mask_bytes.len(),
            copied,
        });
    }

    Ok(RiscvIpiRequest::new(
        usize::from_ne_bytes(mask_bytes),
        0,
        RiscvIpiAbi::Legacy,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_request_preserves_mask_and_base() {
        let request = decode_standard_request(0b1010, 4);

        assert_eq!(request.hart_mask(), 0b1010);
        assert_eq!(request.hart_mask_base(), 4);
        assert_eq!(request.abi(), RiscvIpiAbi::SbiV02);
    }

    #[test]
    fn legacy_request_reads_one_rv64_mask_word() {
        let expected = 0b101usize;
        let request = decode_legacy_request(0x4000, |guest_va, bytes| {
            assert_eq!(guest_va, 0x4000);
            bytes.copy_from_slice(&expected.to_ne_bytes());
            bytes.len()
        })
        .unwrap();

        assert_eq!(request.hart_mask(), expected);
        assert_eq!(request.hart_mask_base(), 0);
        assert_eq!(request.abi(), RiscvIpiAbi::Legacy);
    }

    #[test]
    fn legacy_request_rejects_short_guest_reads() {
        assert_eq!(
            decode_legacy_request(0x4000, |_guest_va, _bytes| 0),
            Err(HartMaskReadError::ShortRead {
                expected: core::mem::size_of::<usize>(),
                copied: 0,
            })
        );
    }
}
