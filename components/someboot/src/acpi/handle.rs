use core::ptr::NonNull;

use crate::mem::phys_to_virt;

#[derive(Clone)]
pub struct AcpiHandle;

impl acpi::Handler for AcpiHandle {
    unsafe fn map_physical_region<T>(
        &self,
        physical_address: usize,
        size: usize,
    ) -> acpi::PhysicalMapping<Self, T> {
        acpi::PhysicalMapping {
            physical_start: physical_address,
            virtual_start: NonNull::new(phys_to_virt(physical_address) as _)
                .expect("Physical address should not be null"),
            region_length: size,
            mapped_length: size,
            handler: self.clone(),
        }
    }

    fn unmap_physical_region<T>(region: &acpi::PhysicalMapping<Self, T>) {
        let _ = region;
    }

    fn read_u8(&self, address: usize) -> u8 {
        unsafe { *(address as *const u8) }
    }

    fn read_u16(&self, address: usize) -> u16 {
        unsafe { *(address as *const u16) }
    }

    fn read_u32(&self, address: usize) -> u32 {
        unsafe { *(address as *const u32) }
    }

    fn read_u64(&self, address: usize) -> u64 {
        unsafe { *(address as *const u64) }
    }

    fn write_u8(&self, address: usize, value: u8) {
        unsafe {
            *(address as *mut u8) = value;
        }
    }

    fn write_u16(&self, address: usize, value: u16) {
        unsafe {
            *(address as *mut u16) = value;
        }
    }

    fn write_u32(&self, address: usize, value: u32) {
        unsafe {
            *(address as *mut u32) = value;
        }
    }

    fn write_u64(&self, address: usize, value: u64) {
        unsafe {
            *(address as *mut u64) = value;
        }
    }

    fn read_io_u8(&self, _port: u16) -> u8 {
        todo!()
    }

    fn read_io_u16(&self, _port: u16) -> u16 {
        todo!()
    }

    fn read_io_u32(&self, _port: u16) -> u32 {
        todo!()
    }

    fn write_io_u8(&self, _port: u16, _value: u8) {
        todo!()
    }

    fn write_io_u16(&self, _port: u16, _value: u16) {
        todo!()
    }

    fn write_io_u32(&self, _port: u16, _value: u32) {
        todo!()
    }

    fn read_pci_u8(&self, _address: acpi::PciAddress, _offset: u16) -> u8 {
        0 // 占位符实现
    }

    fn read_pci_u16(&self, _address: acpi::PciAddress, _offset: u16) -> u16 {
        0 // 占位符实现
    }

    fn read_pci_u32(&self, _address: acpi::PciAddress, _offset: u16) -> u32 {
        0 // 占位符实现
    }

    fn write_pci_u8(&self, _address: acpi::PciAddress, _offset: u16, _value: u8) {
        // 占位符实现
    }

    fn write_pci_u16(&self, _address: acpi::PciAddress, _offset: u16, _value: u16) {
        // 占位符实现
    }

    fn write_pci_u32(&self, _address: acpi::PciAddress, _offset: u16, _value: u32) {
        // 占位符实现
    }

    fn nanos_since_boot(&self) -> u64 {
        0 // 占位符实现
    }

    fn stall(&self, _microseconds: u64) {
        // ::uefi::boot::stall(Duration::from_micros(microseconds));
    }

    fn sleep(&self, _milliseconds: u64) {
        // ::uefi::boot::stall(Duration::from_millis(milliseconds));
    }

    fn create_mutex(&self) -> acpi::Handle {
        unsafe { core::mem::zeroed() } // 占位符实现
    }

    fn acquire(&self, _mutex: acpi::Handle, _timeout: u16) -> Result<(), acpi::aml::AmlError> {
        Ok(()) // 占位符实现
    }

    fn release(&self, _mutex: acpi::Handle) {
        // 占位符实现
    }
}
