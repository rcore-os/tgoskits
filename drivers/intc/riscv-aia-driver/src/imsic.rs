use crate::Error;

/// IMSIC file address geometry representable by an APLIC MSI target.
#[derive(Clone, Copy, Debug)]
pub struct FileLayout {
    hart_bits: u32,
    guest_bits: u32,
    group_bits: u32,
    group_shift: u32,
    stride: u64,
    index_mask: u64,
}

impl FileLayout {
    /// Validate the address geometry against the APLIC target field widths.
    pub fn new(
        hart_bits: u32,
        guest_bits: u32,
        group_bits: u32,
        group_shift: u32,
    ) -> Result<Self, Error> {
        // APLIC target encodes a 14-bit combined hart/group index. GuestIndex
        // stays zero; the file address may still reserve up to 128 guest pages.
        if hart_bits > 14
            || group_bits > 14 - hart_bits
            || guest_bits > 7
            || !(24..64).contains(&group_shift)
            || group_bits > 64 - group_shift
            || (group_bits > 0 && 12 + guest_bits + hart_bits > group_shift)
        {
            return Err(Error::InvalidLayout);
        }
        let stride = 1u64 << (12 + guest_bits);
        let index_mask = (((1u64 << hart_bits) - 1) << (12 + guest_bits))
            | (((1u64 << group_bits) - 1) << group_shift);
        Ok(Self {
            hart_bits,
            guest_bits,
            group_bits,
            group_shift,
            stride,
            index_mask,
        })
    }

    /// Fill caller-owned storage with distinct supervisor file addresses.
    /// Entries are not usable if validation fails; no hardware is accessed.
    pub fn file_addresses(
        &self,
        regs: impl Iterator<Item = (u64, Option<u64>)>,
        addresses: &mut [u64],
    ) -> Result<(), Error> {
        let mut regs = regs.peekable();
        let first = regs.peek().ok_or(Error::InvalidFileRange)?;
        let base = first.0 & !self.index_mask;
        let mut count = 0;
        for (start, size) in regs {
            if count == addresses.len() {
                break;
            }
            let size = size.ok_or(Error::InvalidFileRange)?;
            if start & 0xfff != 0 || size == 0 || size & 0xfff != 0 {
                return Err(Error::InvalidFileRange);
            }
            let end = start.checked_add(size).ok_or(Error::InvalidFileRange)?;
            let mut address = start;
            while address < end && count < addresses.len() {
                if address & !self.index_mask != base || addresses[..count].contains(&address) {
                    return Err(Error::InvalidFileRange);
                }
                addresses[count] = address;
                count += 1;
                address = address
                    .checked_add(self.stride)
                    .ok_or(Error::InvalidFileRange)?;
            }
        }
        if count != addresses.len() {
            return Err(Error::InsufficientFiles);
        }
        Ok(())
    }

    /// Encode the combined hart/group index of a supervisor interrupt file.
    pub fn target(&self, address: u64) -> Result<u32, Error> {
        if address & (self.stride - 1) != 0 {
            return Err(Error::InvalidLayout);
        }
        let hart = (address >> (12 + self.guest_bits)) & ((1u64 << self.hart_bits) - 1);
        let group = (address >> self.group_shift) & ((1u64 << self.group_bits) - 1);
        Ok((hart | (group << self.hart_bits)) as u32)
    }
}

/// Initialize the current supervisor file and enable EIID 1..=the given limit.
/// Identity allocation belongs to the caller. A zero limit clears all enables;
/// a limit beyond `num_ids` is rejected before any CSR access. Pending bits are
/// cleared and the indirect selector is restored before returning.
///
/// # Safety
/// The caller must execute in S-mode with a writable IMSIC supervisor file
/// implementing `num_ids` identities, own that file, and exclude local IRQs,
/// migration and all other users of siselect/sireg for the entire call.
#[cfg(target_arch = "riscv64")]
pub unsafe fn init_supervisor(num_ids: usize, enabled_identity_limit: usize) -> Result<(), Error> {
    if !(63..=2047).contains(&num_ids)
        || !(num_ids + 1).is_multiple_of(64)
        || enabled_identity_limit > num_ids
    {
        return Err(Error::InvalidIdentityCount);
    }
    let selector: usize;
    // SAFETY: the caller owns the privileged CSR transaction on this hart.
    unsafe {
        core::arch::asm!("csrr {value}, 0x150", value = out(reg) selector);
        write_indirect(0x70, 0);
        for first_id in (0..=num_ids).step_by(64) {
            let bank = first_id / 32;
            write_indirect(0x80 + bank, 0);
            let enabled = if first_id > enabled_identity_limit {
                0
            } else {
                // Avoid shifting by XLEN when the entire bank is enabled.
                let last_bit = (enabled_identity_limit - first_id).min(63);
                let mask = usize::MAX >> (63 - last_bit);
                if first_id == 0 { mask & !1 } else { mask }
            };
            write_indirect(0xc0 + bank, enabled);
        }
        write_indirect(0x72, 0);
        write_indirect(0x70, 1);
        core::arch::asm!("csrw 0x150, {value}", value = in(reg) selector);
    }
    Ok(())
}

/// Claim the current hart's highest-priority supervisor EIID in one exchange.
///
/// # Safety
/// The caller must execute in S-mode with an initialized IMSIC supervisor file
/// and own this hart's claim/completion transaction. No selector is modified.
#[cfg(target_arch = "riscv64")]
pub unsafe fn claim_supervisor() -> Option<core::num::NonZeroU32> {
    let value: usize;
    // SAFETY: the caller provides privileged access to its supervisor file.
    unsafe {
        core::arch::asm!("csrrw {value}, 0x15c, zero", value = out(reg) value);
    }
    core::num::NonZeroU32::new(((value >> 16) & 0x7ff) as u32)
}

/// # Safety
/// The current hart's supervisor indirect CSR pair must be exclusively owned.
#[cfg(target_arch = "riscv64")]
unsafe fn write_indirect(selector: usize, value: usize) {
    // SAFETY: init_supervisor's caller excludes IRQs and migration throughout.
    unsafe {
        core::arch::asm!("csrw 0x150, {selector}", "csrw 0x151, {value}",
            selector = in(reg) selector, value = in(reg) value);
    }
}
