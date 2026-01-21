mod dtb;

pub use dtb::DeviceTree;

#[derive(Debug, Clone, Copy)]
pub enum PlatformDescriptor {
    DeviceTree(DeviceTree),
    Acpi,
    None,
}

pub fn get_platform_descriptor() -> PlatformDescriptor {
    if let Some(dtb) = crate::hal::al::platform::fdt_addr() {
        PlatformDescriptor::DeviceTree(DeviceTree::new(dtb))
    } else {
        PlatformDescriptor::None
    }
}
