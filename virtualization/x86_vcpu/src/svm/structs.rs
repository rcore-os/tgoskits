use ax_errno::AxResult;
use ax_memory_addr::PAGE_SIZE_4K as PAGE_SIZE;
use axvm_types::HostPhysAddr;

use super::frame::ContiguousPhysFrames;
use crate::{host::PhysFrame, svm::vmcb::VmcbStruct};

/// Virtual-machine control block backing page.
#[derive(Debug)]
pub struct VmcbFrame {
    page: PhysFrame,
}

impl VmcbFrame {
    pub const unsafe fn uninit() -> Self {
        Self {
            page: unsafe { PhysFrame::uninit() },
        }
    }

    pub fn new() -> AxResult<Self> {
        Ok(Self {
            page: PhysFrame::alloc_zero()?,
        })
    }

    pub fn phys_addr(&self) -> HostPhysAddr {
        self.page.start_paddr()
    }

    pub fn as_mut_ptr(&self) -> *mut u8 {
        self.page.as_mut_ptr()
    }

    pub fn as_ptr_vmcb(&self) -> *const VmcbStruct {
        self.page.as_mut_ptr() as *const VmcbStruct
    }

    pub fn as_mut_ptr_vmcb(&self) -> *mut VmcbStruct {
        self.page.as_mut_ptr() as *mut VmcbStruct
    }
}

/// SVM I/O permissions map. A set bit intercepts the corresponding port.
#[derive(Debug)]
pub struct IOPm {
    frames: ContiguousPhysFrames,
}

impl IOPm {
    pub fn passthrough_all() -> AxResult<Self> {
        let frames = ContiguousPhysFrames::alloc_zero(3)?;
        let third_frame_start = frames.as_mut_ptr() as usize + 2 * PAGE_SIZE;
        unsafe {
            *(third_frame_start as *mut u8) |= 0x07;
        }
        Ok(Self { frames })
    }

    #[allow(unused)]
    pub fn intercept_all() -> AxResult<Self> {
        let mut frames = ContiguousPhysFrames::alloc(3)?;
        frames.fill(0xff);
        Ok(Self { frames })
    }

    pub fn phys_addr(&self) -> HostPhysAddr {
        self.frames.start_paddr()
    }

    pub fn set_intercept(&mut self, port: u32, intercept: bool) {
        let byte_index = port as usize / 8;
        let bit_offset = (port % 8) as u8;

        unsafe {
            let byte_ptr = self.frames.as_mut_ptr().add(byte_index);
            if intercept {
                *byte_ptr |= 1 << bit_offset;
            } else {
                *byte_ptr &= !(1 << bit_offset);
            }
        }
    }

    pub fn set_intercept_of_range(&mut self, port_base: u32, count: u32, intercept: bool) {
        for port in port_base..port_base + count {
            self.set_intercept(port, intercept)
        }
    }
}

/// SVM MSR permissions map. Each MSR has separate read/write intercept bits.
#[derive(Debug)]
pub struct MSRPm {
    frames: ContiguousPhysFrames,
}

impl MSRPm {
    pub fn passthrough_all() -> AxResult<Self> {
        Ok(Self {
            frames: ContiguousPhysFrames::alloc_zero(2)?,
        })
    }

    #[allow(unused)]
    pub fn intercept_all() -> AxResult<Self> {
        let mut frames = ContiguousPhysFrames::alloc(2)?;
        frames.fill(0xff);
        Ok(Self { frames })
    }

    pub fn phys_addr(&self) -> HostPhysAddr {
        self.frames.start_paddr()
    }

    pub fn set_intercept(&mut self, msr: u32, is_write: bool, intercept: bool) {
        let (segment, msr_low) = if msr <= 0x1fff {
            (0u32, msr)
        } else if (0xc000_0000..=0xc000_1fff).contains(&msr) {
            (1u32, msr & 0x1fff)
        } else if (0xc001_0000..=0xc001_1fff).contains(&msr) {
            (2u32, msr & 0x1fff)
        } else {
            unreachable!("MSR {:#x} is not covered by the SVM MSRPM", msr);
        };

        let base_offset = (segment * 2048) as usize;
        let byte_in_segment = msr_low as usize / 4;
        let bit_pair_offset = ((msr_low & 0b11) * 2) as u8;
        let bit_offset = bit_pair_offset + is_write as u8;

        unsafe {
            let byte_ptr = self.frames.as_mut_ptr().add(base_offset + byte_in_segment);
            let old = core::ptr::read_volatile(byte_ptr);
            let new = if intercept {
                old | (1u8 << bit_offset)
            } else {
                old & !(1u8 << bit_offset)
            };
            core::ptr::write_volatile(byte_ptr, new);
        }
    }

    pub fn set_read_intercept(&mut self, msr: u32, intercept: bool) {
        self.set_intercept(msr, false, intercept);
    }

    pub fn set_write_intercept(&mut self, msr: u32, intercept: bool) {
        self.set_intercept(msr, true, intercept);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::mock::MockMmHal;

    #[test]
    fn svm_permission_maps_use_contiguous_frames() {
        MockMmHal::run_test(|| {
            {
                let iopm = IOPm::passthrough_all().unwrap();
                assert_eq!(iopm.phys_addr().as_usize(), 0x1000);
                assert_eq!(MockMmHal::allocated_count(), 3);

                let msrpm = MSRPm::passthrough_all().unwrap();
                assert_eq!(msrpm.phys_addr().as_usize(), 0x4000);
                assert_eq!(MockMmHal::allocated_count(), 5);
            }

            assert_eq!(MockMmHal::allocated_count(), 0);
        });
    }
}
