#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetArch {
    X86_64,
    Loongarch64,
}

impl TargetArch {
    pub const fn name(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Loongarch64 => "loongarch64",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootloaderTarget {
    pub board: &'static str,
    pub arch: TargetArch,
    pub uefi_target: &'static str,
    pub output_file: &'static str,
    pub default_kernel_load_addr: u64,
    pub preferred_entry_symbol: Option<&'static str>,
}

impl BootloaderTarget {
    pub const ASUS_NUC15CRH: Self = Self {
        board: "asus-nuc15crh",
        arch: TargetArch::X86_64,
        uefi_target: "x86_64-unknown-uefi",
        output_file: "BOOTX64.EFI",
        default_kernel_load_addr: 0x200000,
        preferred_entry_symbol: Some("httpboot_entry"),
    };
}

pub fn known_target(board: &str) -> Option<BootloaderTarget> {
    match board {
        "asus-nuc15crh" | "asus-nuc15crh-x86_64" | "Asus-nuc15-x86_64-vmx" => {
            Some(BootloaderTarget::ASUS_NUC15CRH)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{BootloaderTarget, TargetArch, known_target};

    #[test]
    fn asus_target_describes_current_loader_shape() {
        let target = BootloaderTarget::ASUS_NUC15CRH;

        assert_eq!(target.board, "asus-nuc15crh");
        assert_eq!(target.arch, TargetArch::X86_64);
        assert_eq!(target.arch.name(), "x86_64");
        assert_eq!(target.uefi_target, "x86_64-unknown-uefi");
        assert_eq!(target.output_file, "BOOTX64.EFI");
        assert_eq!(target.default_kernel_load_addr, 0x200000);
        assert_eq!(target.preferred_entry_symbol, Some("httpboot_entry"));
    }

    #[test]
    fn known_target_accepts_existing_board_name() {
        assert_eq!(
            known_target("Asus-nuc15-x86_64-vmx"),
            Some(BootloaderTarget::ASUS_NUC15CRH)
        );
    }
}
