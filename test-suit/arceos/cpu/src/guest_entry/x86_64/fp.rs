//! Real x87 ownership probe around a guest machine window.

#[repr(C, align(64))]
struct Image([u8; 512]);

pub struct HostFp {
    original: Image,
    expected: Image,
    ymm: [u8; 32],
    avx: bool,
}

impl HostFp {
    /// The test owns the current CPU's FPU until `finish` restores it.
    /// `avx` requires hardware AVX with SSE/YMM enabled in host XCR0.
    pub unsafe fn begin(avx: bool) -> Self {
        let mut saved = Self {
            original: Image([0; 512]),
            expected: Image([0; 512]),
            ymm: [0; 32],
            avx,
        };
        // SAFETY: the CPU supports FXSAVE, both images are exclusive aligned
        // storage, and IRQs/migration remain excluded throughout the probe.
        unsafe {
            core::arch::asm!("fxsave64 [{}]", in(reg) saved.original.0.as_mut_ptr(), options(nostack));
            if avx {
                core::arch::asm!("vmovdqu [{}], ymm0", "vxorps ymm0, ymm0, ymm0", "vcmpps ymm0, ymm0, ymm0, 0", in(reg) saved.ymm.as_mut_ptr(), options(nostack));
            }
            core::arch::asm!("fninit", "fldpi", options(nostack));
            core::arch::asm!("fxsave64 [{}]", in(reg) saved.expected.0.as_mut_ptr(), options(nostack));
        }
        saved
    }

    pub unsafe fn finish(self) {
        let mut actual = Image([0; 512]);
        let mut actual_ymm = [0u8; 32];
        // SAFETY: the synchronous entry has returned on the owning CPU; save
        // its actual x87 bank and restore the original host image before any
        // assertion can invoke runtime diagnostics.
        unsafe {
            core::arch::asm!("fxsave64 [{}]", in(reg) actual.0.as_mut_ptr(), options(nostack));
            if self.avx {
                core::arch::asm!("vmovdqu [{}], ymm0", in(reg) actual_ymm.as_mut_ptr(), options(nostack));
            }
            core::arch::asm!("fxrstor64 [{}]", in(reg) self.original.0.as_ptr(), options(nostack));
            if self.avx {
                core::arch::asm!("vmovdqu ymm0, [{}]", in(reg) self.ymm.as_ptr(), options(nostack));
            }
        }
        if self.avx {
            assert_eq!(
                actual_ymm, [0xff; 32],
                "guest YMM data leaked into the host"
            );
        }
        assert_eq!(
            &actual.0[0..5],
            &self.expected.0[0..5],
            "host x87 control/status/tag changed"
        );
        assert_eq!(
            &actual.0[32..42],
            &self.expected.0[32..42],
            "guest x87 value leaked into the host"
        );
    }
}

pub fn clear_ymm(avx: bool) -> [u8; 4] {
    if avx {
        [0xc5, 0xfc, 0x57, 0xc0]
    } else {
        [0x90; 4]
    }
}

pub fn store_ymm(avx: bool, address: u32) -> [u8; 9] {
    let a = address.to_le_bytes();
    if avx {
        [0x67, 0xc5, 0xfe, 0x7f, 0x05, a[0], a[1], a[2], a[3]]
    } else {
        [0x90; 9]
    }
}
