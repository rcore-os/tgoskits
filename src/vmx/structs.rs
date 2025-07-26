use bit_field::BitField;
use bitflags::bitflags;

use memory_addr::PAGE_SIZE_4K as PAGE_SIZE;

use axaddrspace::{AxMmHal, HostPhysAddr, PhysFrame};
use axerrno::AxResult;

use crate::msr::{Msr, MsrReadWrite};

/// VMCS/VMXON region in 4K size. (SDM Vol. 3C, Section 24.2)
#[derive(Debug)]
pub struct VmxRegion<H: AxMmHal> {
    frame: PhysFrame<H>,
}

impl<H: AxMmHal> VmxRegion<H> {
    pub const unsafe fn uninit() -> Self {
        Self {
            frame: unsafe { PhysFrame::uninit() },
        }
    }

    pub fn new(revision_id: u32, shadow_indicator: bool) -> AxResult<Self> {
        let frame = PhysFrame::alloc_zero()?;
        unsafe {
            (*(frame.as_mut_ptr() as *mut u32))
                .set_bits(0..=30, revision_id)
                .set_bit(31, shadow_indicator);
        }
        Ok(Self { frame })
    }

    pub fn phys_addr(&self) -> HostPhysAddr {
        self.frame.start_paddr()
    }
}

// (SDM Vol. 3C, Section 25.6.4)
// The VM-execution control fields include the 64-bit physical addresses of I/O bitmaps A and B (each of which are 4 KBytes in size).
// I/O bitmap A contains one bit for each I/O port in the range 0000H through 7FFFH;
// I/O bitmap B contains bits for ports in the range 8000H through FFFFH.
#[derive(Debug)]
pub struct IOBitmap<H: AxMmHal> {
    io_bitmap_a_frame: PhysFrame<H>,
    io_bitmap_b_frame: PhysFrame<H>,
}

impl<H: AxMmHal> IOBitmap<H> {
    pub fn passthrough_all() -> AxResult<Self> {
        Ok(Self {
            io_bitmap_a_frame: PhysFrame::alloc_zero()?,
            io_bitmap_b_frame: PhysFrame::alloc_zero()?,
        })
    }

    #[allow(unused)]
    pub fn intercept_all() -> AxResult<Self> {
        let mut io_bitmap_a_frame = PhysFrame::alloc()?;
        io_bitmap_a_frame.fill(u8::MAX);
        let mut io_bitmap_b_frame = PhysFrame::alloc()?;
        io_bitmap_b_frame.fill(u8::MAX);
        Ok(Self {
            io_bitmap_a_frame,
            io_bitmap_b_frame,
        })
    }

    pub fn phys_addr(&self) -> (HostPhysAddr, HostPhysAddr) {
        (
            self.io_bitmap_a_frame.start_paddr(),
            self.io_bitmap_b_frame.start_paddr(),
        )
    }

    // Execution of an I/O instruction causes a VM exit
    // if any bit in the I/O bitmaps corresponding to a port it accesses is 1.
    // See Section 26.1.3 for details.
    pub fn set_intercept(&mut self, port: u32, intercept: bool) {
        let (port, io_bit_map_frame) = if port <= 0x7fff {
            (port, &mut self.io_bitmap_a_frame)
        } else {
            (port - 0x8000, &mut self.io_bitmap_b_frame)
        };
        let bitmap =
            unsafe { core::slice::from_raw_parts_mut(io_bit_map_frame.as_mut_ptr(), 1024) };
        let byte = (port / 8) as usize;
        let bits = port % 8;
        if intercept {
            bitmap[byte] |= 1 << bits;
        } else {
            bitmap[byte] &= !(1 << bits);
        }
    }

    pub fn set_intercept_of_range(&mut self, port_base: u32, count: u32, intercept: bool) {
        for port in port_base..port_base + count {
            self.set_intercept(port, intercept)
        }
    }
}

#[derive(Debug)]
pub struct MsrBitmap<H: AxMmHal> {
    frame: PhysFrame<H>,
}

impl<H: AxMmHal> MsrBitmap<H> {
    pub fn passthrough_all() -> AxResult<Self> {
        Ok(Self {
            frame: PhysFrame::alloc_zero()?,
        })
    }

    #[allow(unused)]
    pub fn intercept_all() -> AxResult<Self> {
        let mut frame = PhysFrame::alloc()?;
        frame.fill(u8::MAX);
        Ok(Self { frame })
    }

    pub fn phys_addr(&self) -> HostPhysAddr {
        self.frame.start_paddr()
    }

    fn set_intercept(&mut self, msr: u32, is_write: bool, intercept: bool) {
        let offset = if msr <= 0x1fff {
            if !is_write {
                0 // Read bitmap for low MSRs (0x0000_0000..0x0000_1FFF)
            } else {
                2 // Write bitmap for low MSRs (0x0000_0000..0x0000_1FFF)
            }
        } else if (0xc000_0000..=0xc000_1fff).contains(&msr) {
            if !is_write {
                1 // Read bitmap for high MSRs (0xC000_0000..0xC000_1FFF)
            } else {
                3 // Write bitmap for high MSRs (0xC000_0000..0xC000_1FFF)
            }
        } else {
            unreachable!()
        } * 1024;
        let bitmap =
            unsafe { core::slice::from_raw_parts_mut(self.frame.as_mut_ptr().add(offset), 1024) };
        let msr = msr & 0x1fff;
        let byte = (msr / 8) as usize;
        let bits = msr % 8;
        if intercept {
            bitmap[byte] |= 1 << bits;
        } else {
            bitmap[byte] &= !(1 << bits);
        }
    }

    pub fn set_read_intercept(&mut self, msr: u32, intercept: bool) {
        self.set_intercept(msr, false, intercept);
    }

    pub fn set_write_intercept(&mut self, msr: u32, intercept: bool) {
        self.set_intercept(msr, true, intercept);
    }
}

/// Reporting Register of Basic VMX Capabilities. (SDM Vol. 3D, Appendix A.1)
#[derive(Debug)]
pub struct VmxBasic {
    /// The 31-bit VMCS revision identifier used by the processor.
    pub revision_id: u32,
    /// The number of bytes that software should allocate for the VMXON region
    /// and any VMCS region.
    pub region_size: u16,
    /// The width of the physical addresses that may be used for the VMXON
    /// region, each VMCS, and data structures referenced by pointers in a VMCS.
    pub is_32bit_address: bool,
    /// The memory type that should be used for the VMCS, for data structures
    /// referenced by pointers in the VMCS.
    pub mem_type: u8,
    /// The processor reports information in the VM-exit instruction-information
    /// field on VM exits due to execution of the INS and OUTS instructions.
    pub io_exit_info: bool,
    /// If any VMX controls that default to 1 may be cleared to 0.
    pub vmx_flex_controls: bool,
}

impl MsrReadWrite for VmxBasic {
    const MSR: Msr = Msr::IA32_VMX_BASIC;
}

impl VmxBasic {
    pub const VMX_MEMORY_TYPE_WRITE_BACK: u8 = 6;

    /// Read the current IA32_VMX_BASIC flags.
    pub fn read() -> Self {
        let msr = Self::read_raw();
        Self {
            revision_id: msr.get_bits(0..31) as u32,
            region_size: msr.get_bits(32..45) as u16,
            is_32bit_address: msr.get_bit(48),
            mem_type: msr.get_bits(50..54) as u8,
            io_exit_info: msr.get_bit(54),
            vmx_flex_controls: msr.get_bit(55),
        }
    }
}

bitflags! {
    /// IA32_FEATURE_CONTROL flags.
    #[derive(Debug)]
    pub struct FeatureControlFlags: u64 {
       /// Lock bit: when set, locks this MSR from being written. when clear,
       /// VMXON causes a #GP.
       const LOCKED = 1 << 0;
       /// Enable VMX inside SMX operation.
       const VMXON_ENABLED_INSIDE_SMX = 1 << 1;
       /// Enable VMX outside SMX operation.
       const VMXON_ENABLED_OUTSIDE_SMX = 1 << 2;
   }
}

/// Control Features in Intel 64 Processor. (SDM Vol. 3C, Section 23.7)
pub struct FeatureControl;

impl MsrReadWrite for FeatureControl {
    const MSR: Msr = Msr::IA32_FEATURE_CONTROL;
}

impl FeatureControl {
    /// Read the current IA32_FEATURE_CONTROL flags.
    pub fn read() -> FeatureControlFlags {
        FeatureControlFlags::from_bits_truncate(Self::read_raw())
    }

    /// Write IA32_FEATURE_CONTROL flags, preserving reserved values.
    pub fn write(flags: FeatureControlFlags) {
        let old_value = Self::read_raw();
        let reserved = old_value & !(FeatureControlFlags::all().bits());
        let new_value = reserved | flags.bits();
        unsafe { Self::write_raw(new_value) };
    }
}

bitflags! {
    /// Extended-Page-Table Pointer. (SDM Vol. 3C, Section 24.6.11)
    #[derive(Debug)]
    pub struct EPTPointer: u64 {
        /// EPT paging-structure memory type: Uncacheable (UC).
        #[allow(clippy::identity_op)]
        const MEM_TYPE_UC = 0 << 0;
        /// EPT paging-structure memory type: Write-back (WB).
        #[allow(clippy::identity_op)]
        const MEM_TYPE_WB = 6 << 0;
        /// EPT page-walk length 1.
        const WALK_LENGTH_1 = 0 << 3;
        /// EPT page-walk length 2.
        const WALK_LENGTH_2 = 1 << 3;
        /// EPT page-walk length 3.
        const WALK_LENGTH_3 = 2 << 3;
        /// EPT page-walk length 4.
        const WALK_LENGTH_4 = 3 << 3;
        /// Setting this control to 1 enables accessed and dirty flags for EPT.
        const ENABLE_ACCESSED_DIRTY = 1 << 6;
    }
}

impl EPTPointer {
    pub fn from_table_phys(pml4_paddr: HostPhysAddr) -> Self {
        let aligned_addr = pml4_paddr.as_usize() & !(PAGE_SIZE - 1);
        let flags = Self::from_bits_retain(aligned_addr as u64);
        flags | Self::MEM_TYPE_WB | Self::WALK_LENGTH_4 | Self::ENABLE_ACCESSED_DIRTY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::mock::MockMmHal;
    use alloc::format;

    #[test]
    fn test_vmx_region_uninit() {
        let region = unsafe { VmxRegion::<MockMmHal>::uninit() };

        // Test that we can create an uninitialized region
        // Can't test much more without allocating memory
        let debug_str = format!("{:?}", region);
        assert!(!debug_str.is_empty());
    }

    #[test]
    fn test_vmx_region_new() {
        // Reset allocator for consistent testing
        MockMmHal::reset();

        // Test VmxRegion::new with valid parameters
        let region = VmxRegion::<MockMmHal>::new(0x12345, false);
        assert!(region.is_ok());

        let region = region.unwrap();
        let addr = region.phys_addr();
        assert_ne!(addr.as_usize(), 0);
        // Should be page-aligned
        assert_eq!(addr.as_usize() % 0x1000, 0);
    }

    #[test]
    fn test_vmx_region_new_with_shadow() {
        // Reset allocator for consistent testing
        MockMmHal::reset();

        // Test VmxRegion::new with different shadow indicator values
        let region_no_shadow = VmxRegion::<MockMmHal>::new(0x12345, false);
        assert!(region_no_shadow.is_ok());

        let region_with_shadow = VmxRegion::<MockMmHal>::new(0x12345, true);
        assert!(region_with_shadow.is_ok());

        // Test that both regions have valid physical addresses
        let region1 = region_no_shadow.unwrap();
        let region2 = region_with_shadow.unwrap();

        let addr1 = region1.phys_addr();
        let addr2 = region2.phys_addr();

        assert_ne!(addr1.as_usize(), 0);
        assert_ne!(addr2.as_usize(), 0);
        assert_ne!(addr1.as_usize(), addr2.as_usize());
        assert_eq!(addr1.as_usize() % 0x1000, 0);
        assert_eq!(addr2.as_usize() % 0x1000, 0);
    }

    #[test]
    fn test_io_bitmap_creation() {
        // Test IOBitmap creation methods
        MockMmHal::reset();

        // Test passthrough_all creation
        let passthrough_bitmap = IOBitmap::<MockMmHal>::passthrough_all();
        assert!(passthrough_bitmap.is_ok());

        // Test intercept_all creation
        let intercept_bitmap = IOBitmap::<MockMmHal>::intercept_all();
        assert!(intercept_bitmap.is_ok());

        // Test that phys_addr returns valid addresses
        let bitmap = passthrough_bitmap.unwrap();
        let (addr_a, addr_b) = bitmap.phys_addr();
        assert_ne!(addr_a.as_usize(), 0);
        assert_ne!(addr_b.as_usize(), 0);
        assert_ne!(addr_a.as_usize(), addr_b.as_usize());
    }

    #[test]
    fn test_msr_bitmap_creation() {
        // Test MsrBitmap creation methods
        MockMmHal::reset();

        // Test passthrough_all creation
        let passthrough_bitmap = MsrBitmap::<MockMmHal>::passthrough_all();
        assert!(passthrough_bitmap.is_ok());

        // Test intercept_all creation
        let intercept_bitmap = MsrBitmap::<MockMmHal>::intercept_all();
        assert!(intercept_bitmap.is_ok());

        // Test that phys_addr returns valid addresses
        let bitmap = passthrough_bitmap.unwrap();
        let addr = bitmap.phys_addr();
        assert_ne!(addr.as_usize(), 0);
        assert_eq!(addr.as_usize() % 0x1000, 0);
    }

    #[test]
    fn test_ept_pointer_creation() {
        // Test EPTPointer creation with from_table_phys method
        let ept_ptr1 = EPTPointer::from_table_phys(memory_addr::PhysAddr::from(0x1000));
        let ept_ptr2 = EPTPointer::from_table_phys(memory_addr::PhysAddr::from(0x2000));

        // Verify the EPT pointers were created successfully
        assert_ne!(ept_ptr1.0, ept_ptr2.0);
    }

    #[test]
    fn test_ept_pointer_getters() {
        let phys_addr = memory_addr::PhysAddr::from(0x3000);
        let ept_ptr = EPTPointer::from_table_phys(phys_addr);

        // Test that we can create EPT pointer and it has expected flags
        let bits = ept_ptr.bits();
        assert_ne!(bits, 0);

        // Should have the expected flags set
        let expected_flags =
            EPTPointer::MEM_TYPE_WB | EPTPointer::WALK_LENGTH_4 | EPTPointer::ENABLE_ACCESSED_DIRTY;
        assert_eq!(bits & expected_flags.bits(), expected_flags.bits());
    }

    #[test]
    fn test_vmx_basic_constants() {
        assert_eq!(VmxBasic::VMX_MEMORY_TYPE_WRITE_BACK, 6);
    }

    #[test]
    fn test_feature_control_flags() {
        let flags = FeatureControlFlags::LOCKED | FeatureControlFlags::VMXON_ENABLED_OUTSIDE_SMX;

        assert!(flags.contains(FeatureControlFlags::LOCKED));
        assert!(flags.contains(FeatureControlFlags::VMXON_ENABLED_OUTSIDE_SMX));
        assert!(!flags.contains(FeatureControlFlags::VMXON_ENABLED_INSIDE_SMX));
    }

    #[test]
    fn test_ept_pointer_flags() {
        use EPTPointer as EPT;

        // Test individual flags
        assert_eq!(EPT::MEM_TYPE_UC.bits(), 0);
        assert_eq!(EPT::MEM_TYPE_WB.bits(), 6);
        assert_eq!(EPT::WALK_LENGTH_4.bits(), 3 << 3);

        // Test flag combination
        let combined = EPT::MEM_TYPE_WB | EPT::WALK_LENGTH_4 | EPT::ENABLE_ACCESSED_DIRTY;
        assert!(combined.contains(EPT::MEM_TYPE_WB));
        assert!(combined.contains(EPT::WALK_LENGTH_4));
        assert!(combined.contains(EPT::ENABLE_ACCESSED_DIRTY));
    }

    #[test]
    fn test_ept_pointer_from_table_phys() {
        let pml4_addr = HostPhysAddr::from(0x12345000_usize); // Page-aligned address
        let ept_ptr = EPTPointer::from_table_phys(pml4_addr);

        // Should have the correct flags set
        assert!(ept_ptr.contains(EPTPointer::MEM_TYPE_WB));
        assert!(ept_ptr.contains(EPTPointer::WALK_LENGTH_4));
        assert!(ept_ptr.contains(EPTPointer::ENABLE_ACCESSED_DIRTY));

        // Address should be preserved (and aligned)
        let addr_part = ept_ptr.bits() & !0xfff;
        assert_eq!(addr_part, 0x12345000);
    }

    #[test]
    fn test_ept_pointer_from_unaligned_addr() {
        let unaligned_addr = HostPhysAddr::from(0x12345678_usize); // Not page-aligned
        let ept_ptr = EPTPointer::from_table_phys(unaligned_addr);

        // Address should be aligned down
        let addr_part = ept_ptr.bits() & !0xfff;
        // Should be aligned to 4K boundary
        assert_eq!(addr_part, 0x12345000);
    }

    #[test]
    fn test_debug_implementations() {
        // Test that all our structs implement Debug properly
        let vmx_region = unsafe { VmxRegion::<MockMmHal>::uninit() };
        let _debug_str = format!("{:?}", vmx_region);

        let io_bitmap = IOBitmap::<MockMmHal>::passthrough_all().unwrap();
        let _debug_str = format!("{:?}", io_bitmap);

        let msr_bitmap = MsrBitmap::<MockMmHal>::passthrough_all().unwrap();
        let _debug_str = format!("{:?}", msr_bitmap);

        let flags = FeatureControlFlags::LOCKED;
        let _debug_str = format!("{:?}", flags);

        let ept_flags = EPTPointer::MEM_TYPE_WB;
        let _debug_str = format!("{:?}", ept_flags);
    }
}
