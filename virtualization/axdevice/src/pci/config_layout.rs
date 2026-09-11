//! Conventional PCI Type-0 configuration-space layout constants.

pub(crate) const CONFIG_SPACE_SIZE: usize = 0x100;

pub(crate) const CONFIG_VENDOR_ID_OFFSET: usize = 0x00;
pub(crate) const CONFIG_DEVICE_ID_OFFSET: usize = 0x02;
pub(crate) const CONFIG_COMMAND_OFFSET: usize = 0x04;
pub(crate) const CONFIG_COMMAND_SIZE: usize = 2;
pub(crate) const CONFIG_DWORD_SIZE: usize = 4;
pub(crate) const CONFIG_STATUS_OFFSET: usize = 0x06;
pub(crate) const CONFIG_REVISION_OFFSET: usize = 0x08;
pub(crate) const CONFIG_PROGRAMMING_INTERFACE_OFFSET: usize = 0x09;
pub(crate) const CONFIG_SUBCLASS_OFFSET: usize = 0x0a;
pub(crate) const CONFIG_BASE_CLASS_OFFSET: usize = 0x0b;
pub(crate) const CONFIG_HEADER_TYPE_OFFSET: usize = 0x0e;
pub(crate) const CONFIG_BAR_START: usize = 0x10;
pub(crate) const CONFIG_BAR_REGISTER_SIZE: usize = 4;
pub(crate) const CONFIG_BAR_END: usize = 0x28;
pub(crate) const CONFIG_BAR_MEMORY_ADDRESS_MASK: u32 = 0xffff_fff0;
pub(crate) const CONFIG_SUBSYSTEM_VENDOR_ID_OFFSET: usize = 0x2c;
pub(crate) const CONFIG_SUBSYSTEM_DEVICE_ID_OFFSET: usize = 0x2e;
pub(crate) const CONFIG_CAPABILITY_POINTER_OFFSET: usize = 0x34;
pub(crate) const CONFIG_INTERRUPT_LINE_OFFSET: usize = 0x3c;
pub(crate) const CONFIG_INTERRUPT_PIN_OFFSET: usize = 0x3d;
pub(crate) const CONFIG_STANDARD_HEADER_END: usize = 0x40;

pub(crate) const BAR_COUNT: u8 = 6;

pub(crate) const COMMAND_MEMORY_SPACE_ENABLE: u8 = 0x02;
pub(crate) const COMMAND_BUS_MASTER_ENABLE: u8 = 0x04;
pub(crate) const COMMAND_INTERRUPT_DISABLE: u8 = 0x04;
pub(crate) const STATUS_CAPABILITIES_LIST: u8 = 0x10;
pub(crate) const STATUS_INTERRUPT_PENDING: u8 = 0x08;
