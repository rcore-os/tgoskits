use x86::msr::{rdmsr, wrmsr};

/// X86 model-specific registers. (SDM Vol. 4)
#[repr(u32)]
#[derive(Debug, Copy, Clone)]
#[allow(non_camel_case_types, dead_code)]
pub enum Msr {
    IA32_FEATURE_CONTROL = 0x3a,

    IA32_PAT = 0x277,

    IA32_VMX_BASIC = 0x480,
    IA32_VMX_PINBASED_CTLS = 0x481,
    IA32_VMX_PROCBASED_CTLS = 0x482,
    IA32_VMX_EXIT_CTLS = 0x483,
    IA32_VMX_ENTRY_CTLS = 0x484,
    IA32_VMX_MISC = 0x485,
    IA32_VMX_CR0_FIXED0 = 0x486,
    IA32_VMX_CR0_FIXED1 = 0x487,
    IA32_VMX_CR4_FIXED0 = 0x488,
    IA32_VMX_CR4_FIXED1 = 0x489,
    IA32_VMX_PROCBASED_CTLS2 = 0x48b,
    IA32_VMX_EPT_VPID_CAP = 0x48c,
    IA32_VMX_TRUE_PINBASED_CTLS = 0x48d,
    IA32_VMX_TRUE_PROCBASED_CTLS = 0x48e,
    IA32_VMX_TRUE_EXIT_CTLS = 0x48f,
    IA32_VMX_TRUE_ENTRY_CTLS = 0x490,

    IA32_XSS = 0xda0,

    IA32_EFER = 0xc000_0080,
    IA32_STAR = 0xc000_0081,
    IA32_LSTAR = 0xc000_0082,
    IA32_CSTAR = 0xc000_0083,
    IA32_FMASK = 0xc000_0084,

    IA32_FS_BASE = 0xc000_0100,
    IA32_GS_BASE = 0xc000_0101,
    IA32_KERNEL_GSBASE = 0xc000_0102,
}

impl Msr {
    /// Read 64 bits msr register.
    #[inline(always)]
    pub fn read(self) -> u64 {
        unsafe { rdmsr(self as _) }
    }

    /// Write 64 bits to msr register.
    ///
    /// # Safety
    ///
    /// The caller must ensure that this write operation has no unsafe side
    /// effects.
    #[inline(always)]
    pub unsafe fn write(self, value: u64) {
        unsafe { wrmsr(self as _, value) }
    }
}

pub(super) trait MsrReadWrite {
    const MSR: Msr;

    fn read_raw() -> u64 {
        Self::MSR.read()
    }

    unsafe fn write_raw(flags: u64) {
        unsafe {
            Self::MSR.write(flags);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    #[test]
    fn test_msr_enum_values() {
        // Test that MSR enum values match the expected constants
        assert_eq!(Msr::IA32_FEATURE_CONTROL as u32, 0x3a);
        assert_eq!(Msr::IA32_PAT as u32, 0x277);
        assert_eq!(Msr::IA32_VMX_BASIC as u32, 0x480);
        assert_eq!(Msr::IA32_EFER as u32, 0xc000_0080);
        assert_eq!(Msr::IA32_LSTAR as u32, 0xc000_0082);
    }

    #[test]
    fn test_msr_debug() {
        // Test that MSR implements Debug properly
        let msr = Msr::IA32_VMX_BASIC;
        let debug_str = format!("{:?}", msr);
        assert!(!debug_str.is_empty());
        assert!(debug_str.contains("IA32_VMX_BASIC"));
    }

    #[test]
    fn test_msr_copy_clone() {
        // Test that MSR implements Copy and Clone
        let msr1 = Msr::IA32_EFER;
        let msr2 = msr1; // Copy
        let msr3 = msr1.clone(); // Clone

        assert_eq!(msr1 as u32, msr2 as u32);
        assert_eq!(msr1 as u32, msr3 as u32);
    }

    #[test]
    fn test_vmx_msr_ranges() {
        // Test VMX MSR values are in the correct range
        assert!(Msr::IA32_VMX_BASIC as u32 >= 0x480);
        assert!(Msr::IA32_VMX_TRUE_ENTRY_CTLS as u32 <= 0x490);

        // Test that VMX MSRs are consecutive where expected
        assert_eq!(
            Msr::IA32_VMX_BASIC as u32 + 1,
            Msr::IA32_VMX_PINBASED_CTLS as u32
        );
        assert_eq!(
            Msr::IA32_VMX_PINBASED_CTLS as u32 + 1,
            Msr::IA32_VMX_PROCBASED_CTLS as u32
        );
    }

    #[test]
    fn test_fs_gs_base_msr_values() {
        // Test FS/GS base MSRs
        assert_eq!(Msr::IA32_FS_BASE as u32, 0xc000_0100);
        assert_eq!(Msr::IA32_GS_BASE as u32, 0xc000_0101);
        assert_eq!(Msr::IA32_KERNEL_GSBASE as u32, 0xc000_0102);

        // These should be consecutive
        assert_eq!(Msr::IA32_FS_BASE as u32 + 1, Msr::IA32_GS_BASE as u32);
        assert_eq!(Msr::IA32_GS_BASE as u32 + 1, Msr::IA32_KERNEL_GSBASE as u32);
    }

    #[test]
    fn test_system_call_msr_values() {
        // Test system call related MSRs
        assert_eq!(Msr::IA32_STAR as u32, 0xc000_0081);
        assert_eq!(Msr::IA32_LSTAR as u32, 0xc000_0082);
        assert_eq!(Msr::IA32_CSTAR as u32, 0xc000_0083);
        assert_eq!(Msr::IA32_FMASK as u32, 0xc000_0084);

        // These should be consecutive
        assert_eq!(Msr::IA32_STAR as u32 + 1, Msr::IA32_LSTAR as u32);
        assert_eq!(Msr::IA32_LSTAR as u32 + 1, Msr::IA32_CSTAR as u32);
        assert_eq!(Msr::IA32_CSTAR as u32 + 1, Msr::IA32_FMASK as u32);
    }

    // Note: We can't test the actual read/write methods without running on real hardware
    // and having the appropriate privileges. Those would be integration tests.

    // Mock implementation for testing the MsrReadWrite trait
    struct TestMsr;

    impl MsrReadWrite for TestMsr {
        const MSR: Msr = Msr::IA32_PAT;
    }

    #[test]
    fn test_msr_read_write_trait() {
        // Test that the trait compiles and has the expected methods
        // We can't actually call read_raw() without MSR access
        assert_eq!(TestMsr::MSR as u32, 0x277);
    }

    #[test]
    fn test_msr_as_u32_conversion() {
        // Test that we can convert MSR enum to u32 properly
        let msrs = [
            Msr::IA32_FEATURE_CONTROL,
            Msr::IA32_VMX_BASIC,
            Msr::IA32_EFER,
            Msr::IA32_LSTAR,
        ];

        for msr in msrs.iter() {
            let value = *msr as u32;
            assert!(value > 0);
            // Values should be reasonable MSR numbers
            assert!(value < 0xffff_ffff);
        }
    }
}
