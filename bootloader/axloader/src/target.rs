//! Target contracts shared by the loader frontends.

use httpboot_protocol::{BootArch, DeviceBootJob, ImageFormat};

/// Dynamic relocation forms accepted in a loadable ELF image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelocationSpec {
    /// The only relocation type the target entry can apply itself.
    pub relative: u32,
}

/// The ELF and handoff contract for one loader target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetSpec {
    pub boot_arch: BootArch,
    pub machine: u16,
    /// PE/COFF machine expected by the launcher and OTA slot validator.
    pub pe_machine: u16,
    pub entry_symbol: &'static str,
    /// Physical placement selected by the target's EFI image contract.
    pub load_address: Option<u64>,
    /// Link-time PT_LOAD base accepted for fixed-placement EFI images.
    pub link_address: Option<u64>,
    /// Dynamic relocations accepted when the target entry relocates itself.
    pub relocation: Option<RelocationSpec>,
}

impl TargetSpec {
    /// Checks the ELF machine value associated with this target.
    pub const fn matches_machine(self, machine: u16) -> bool {
        self.machine == machine
    }

    /// Checks the architecture, image encoding, and ABI entry symbol supplied by a boot job.
    pub fn accepts(
        self,
        arch: BootArch,
        image_format: ImageFormat,
        entry_symbol: Option<&str>,
    ) -> bool {
        arch == self.boot_arch
            && matches!(image_format, ImageFormat::Elf64)
            && matches!(entry_symbol, Some(symbol) if self.entry_symbol == symbol)
    }
}

/// Returns whether two adjacent PT_LOAD ranges overlap.
pub fn ranges_overlap(start: u64, size: u64, next_start: u64) -> bool {
    start.checked_add(size).is_none_or(|end| end > next_start)
}

/// Identifies a retry of the same immutable HTTP boot manifest.
pub fn is_idempotent_boot_job(existing: &DeviceBootJob, incoming: &DeviceBootJob) -> bool {
    existing.boot_id == incoming.boot_id && existing == incoming
}

pub const X86_64: TargetSpec = TargetSpec {
    boot_arch: BootArch::X86_64,
    machine: 62,
    pe_machine: 0x8664,
    entry_symbol: "__x86_64_efi_pe_entry",
    load_address: None,
    link_address: None,
    relocation: None,
};

pub const LOONGARCH64_EFI: TargetSpec = TargetSpec {
    boot_arch: BootArch::Loongarch64,
    machine: 258,
    pe_machine: 0x6264,
    entry_symbol: "efi_pe_entry",
    load_address: Some(0x20_0000),
    link_address: Some(0xffff_ffff_8000_0000),
    relocation: None,
};

/// Relocatable LoongArch target used by the native UEFI frontend.
///
/// The fixed-placement [`LOONGARCH64_EFI`] contract remains owned by the
/// legacy `arceos-efi` frontend.
pub const LOONGARCH64_UEFI: TargetSpec = TargetSpec {
    boot_arch: BootArch::Loongarch64,
    machine: 258,
    pe_machine: 0x6264,
    entry_symbol: "efi_pe_entry",
    load_address: None,
    link_address: None,
    relocation: Some(RelocationSpec { relative: 3 }),
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
    use httpboot_protocol::{BootArch, ImageFormat};

    use super::{LOONGARCH64_EFI, LOONGARCH64_UEFI, X86_64};

    #[test]
    fn target_contracts_keep_protocol_and_entry_abis_distinct() {
        assert_eq!(X86_64.boot_arch, BootArch::X86_64);
        assert_eq!(X86_64.machine, 62);
        assert_eq!(X86_64.pe_machine, 0x8664);
        assert_eq!(X86_64.entry_symbol, "__x86_64_efi_pe_entry");
        assert_eq!(X86_64.load_address, None);
        assert_eq!(X86_64.relocation, None);
        assert_eq!(LOONGARCH64_EFI.boot_arch, BootArch::Loongarch64);
        assert_eq!(LOONGARCH64_EFI.machine, 258);
        assert_eq!(LOONGARCH64_EFI.pe_machine, 0x6264);
        assert_eq!(LOONGARCH64_EFI.entry_symbol, "efi_pe_entry");
        assert_eq!(LOONGARCH64_EFI.load_address, Some(0x20_0000));
        assert_eq!(LOONGARCH64_EFI.link_address, Some(0xffff_ffff_8000_0000));
        assert_eq!(LOONGARCH64_EFI.relocation, None);
        assert_eq!(LOONGARCH64_UEFI.boot_arch, BootArch::Loongarch64);
        assert_eq!(LOONGARCH64_UEFI.load_address, None);
        assert_eq!(LOONGARCH64_UEFI.link_address, None);
        assert_eq!(LOONGARCH64_UEFI.relocation.unwrap().relative, 3);
    }

    #[test]
    fn target_contract_rejects_wrong_machine_or_entry() {
        assert!(!LOONGARCH64_EFI.matches_machine(X86_64.machine));
        assert!(LOONGARCH64_EFI.matches_machine(258));
        assert!(LOONGARCH64_EFI.accepts(
            BootArch::Loongarch64,
            ImageFormat::Elf64,
            Some("efi_pe_entry")
        ));
        assert!(!LOONGARCH64_EFI.accepts(
            BootArch::X86_64,
            ImageFormat::Elf64,
            Some("efi_pe_entry")
        ));
        assert!(!LOONGARCH64_EFI.accepts(
            BootArch::Loongarch64,
            ImageFormat::Elf64,
            Some("__x86_64_efi_pe_entry")
        ));
    }

    #[test]
    fn segment_ranges_reject_overlap_but_allow_adjacency() {
        assert!(super::ranges_overlap(0x1000, 0x1000, 0x1800));
        assert!(!super::ranges_overlap(0x1000, 0x1000, 0x2000));
        assert!(super::ranges_overlap(u64::MAX - 1, 2, u64::MAX));
    }

    #[test]
    fn identical_http_boot_jobs_are_idempotent() {
        let job: httpboot_protocol::DeviceBootJob = serde_json::from_value(serde_json::json!({
            "boot_id": "retryable",
            "arch": "x86_64",
            "image_format": "elf64",
            "kernel": { "size": 1, "sha256": "00".repeat(32) },
            "entry_symbol": "__x86_64_efi_pe_entry"
        }))
        .unwrap();
        assert!(super::is_idempotent_boot_job(&job, &job));
        let mut changed = job.clone();
        changed.boot_id = "different".into();
        assert!(!super::is_idempotent_boot_job(&job, &changed));
    }
}
