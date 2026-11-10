//! Target contracts shared by the loader frontends.

/// Dynamic relocation forms accepted in a loadable ELF image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelocationSpec {
    /// The only relocation type the target entry can apply itself.
    pub relative: u32,
}

/// The ELF and handoff contract for one loader target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetSpec {
    /// Human-readable architecture name used only for diagnostics.
    pub arch_name: &'static str,
    /// ELF `e_machine` value accepted by this loader.
    pub machine: u16,
    /// EFI ABI entry symbol resolved from the uploaded ELF symbol table.
    pub entry_symbol: &'static str,
    /// Physical placement selected by the target's EFI image contract.
    pub load_address: Option<u64>,
    /// Link-time PT_LOAD base accepted for fixed-placement EFI images.
    pub link_address: Option<u64>,
}

impl TargetSpec {
    /// Checks the ELF machine value associated with this target.
    pub const fn matches_machine(self, machine: u16) -> bool {
        self.machine == machine
    }

    /// Returns the PE/COFF machine accepted by this target's slot validator.
    pub const fn pe_machine(self) -> u16 {
        match self.machine {
            62 => 0x8664,
            258 => 0x6264,
            _ => 0,
        }
    }

    /// Returns the relocation policy for targets whose entry applies a load
    /// bias itself. Fixed-placement targets return `None`.
    pub const fn relocation(self) -> Option<RelocationSpec> {
        if self.machine == 258 && self.load_address.is_none() && self.link_address.is_none() {
            Some(RelocationSpec { relative: 3 })
        } else {
            None
        }
    }
}

/// Returns whether two adjacent PT_LOAD ranges overlap.
pub fn ranges_overlap(start: u64, size: u64, next_start: u64) -> bool {
    start.checked_add(size).is_none_or(|end| end > next_start)
}

pub const X86_64: TargetSpec = TargetSpec {
    arch_name: "x86_64",
    machine: 62,
    entry_symbol: "__x86_64_efi_pe_entry",
    load_address: None,
    link_address: None,
};

pub const LOONGARCH64_EFI: TargetSpec = TargetSpec {
    arch_name: "loongarch64",
    machine: 258,
    entry_symbol: "efi_pe_entry",
    load_address: Some(0x20_0000),
    link_address: Some(0xffff_ffff_8000_0000),
};

/// Relocatable LoongArch target used by the native UEFI frontend.
///
/// The fixed-placement [`LOONGARCH64_EFI`] contract remains owned by the
/// `arceos-efi` frontend.
pub const LOONGARCH64_UEFI: TargetSpec = TargetSpec {
    arch_name: "loongarch64",
    machine: 258,
    entry_symbol: "efi_pe_entry",
    load_address: None,
    link_address: None,
};

#[cfg(all(target_os = "uefi", target_arch = "x86_64"))]
pub const CURRENT: TargetSpec = X86_64;

#[cfg(all(target_os = "uefi", target_arch = "loongarch64"))]
pub const CURRENT: TargetSpec = LOONGARCH64_UEFI;

#[cfg(all(not(target_os = "uefi"), target_arch = "loongarch64"))]
pub const CURRENT: TargetSpec = LOONGARCH64_EFI;

#[cfg(not(any(target_os = "uefi", target_arch = "loongarch64",)))]
pub const CURRENT: TargetSpec = X86_64;

#[cfg(test)]
mod tests {
    use super::{LOONGARCH64_EFI, LOONGARCH64_UEFI, X86_64};

    #[test]
    fn target_contracts_keep_local_machine_and_entry_abis() {
        assert_eq!(X86_64.arch_name, "x86_64");
        assert_eq!(X86_64.machine, 62);
        assert_eq!(X86_64.pe_machine(), 0x8664);
        assert_eq!(X86_64.entry_symbol, "__x86_64_efi_pe_entry");
        assert_eq!(X86_64.load_address, None);
        assert_eq!(X86_64.relocation(), None);
        assert_eq!(LOONGARCH64_EFI.arch_name, "loongarch64");
        assert_eq!(LOONGARCH64_EFI.machine, 258);
        assert_eq!(LOONGARCH64_EFI.pe_machine(), 0x6264);
        assert_eq!(LOONGARCH64_EFI.entry_symbol, "efi_pe_entry");
        assert_eq!(LOONGARCH64_EFI.load_address, Some(0x20_0000));
        assert_eq!(LOONGARCH64_EFI.link_address, Some(0xffff_ffff_8000_0000));
        assert_eq!(LOONGARCH64_EFI.relocation(), None);
        assert_eq!(LOONGARCH64_UEFI.arch_name, "loongarch64");
        assert_eq!(LOONGARCH64_UEFI.load_address, None);
        assert_eq!(LOONGARCH64_UEFI.link_address, None);
        assert_eq!(LOONGARCH64_UEFI.relocation().unwrap().relative, 3);
    }

    #[test]
    fn target_contract_rejects_wrong_machine() {
        assert!(!LOONGARCH64_EFI.matches_machine(X86_64.machine));
        assert!(LOONGARCH64_EFI.matches_machine(258));
    }

    #[test]
    fn segment_ranges_reject_overlap_but_allow_adjacency() {
        assert!(super::ranges_overlap(0x1000, 0x1000, 0x1800));
        assert!(!super::ranges_overlap(0x1000, 0x1000, 0x2000));
        assert!(super::ranges_overlap(u64::MAX - 1, 2, u64::MAX));
    }
}
