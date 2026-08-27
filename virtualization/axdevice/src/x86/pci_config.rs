//! Minimal Q35 PCI configuration mechanism used by x86 guest firmware.

use alloc::boxed::Box;

use ax_sync::SpinLock as Mutex;
use axdevice_base::*;

const CONFIG_ADDRESS_ENABLE: u32 = 1 << 31;
const CONFIG_ADDRESS_PORT: u16 = 0xcf8;
const CONFIG_DATA_PORT: u16 = 0xcfc;
const CONFIG_SPACE_SIZE: usize = 256;

/// Q35-compatible PCI configuration mechanism #1 with a host bridge and LPC bridge.
pub struct X86PciConfigDevice {
    state: Mutex<PciConfigState>,
    resources: Box<[Resource]>,
}

struct PciConfigState {
    address: u32,
    host_bridge: PciFunction,
    lpc_bridge: PciFunction,
}

struct PciFunction {
    config: [u8; CONFIG_SPACE_SIZE],
    write_mask: [u8; CONFIG_SPACE_SIZE],
}

impl X86PciConfigDevice {
    /// Base of the PCI configuration address/data port window.
    pub const PORT_BASE: u16 = CONFIG_ADDRESS_PORT;
    /// Size of the combined address/data port window.
    pub const PORT_SIZE: u16 = 8;

    /// Creates the minimal Q35 topology required by OVMF.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(PciConfigState::new()),
            resources: alloc::vec![Resource::PortRange {
                base: Self::PORT_BASE,
                size: Self::PORT_SIZE,
            }]
            .into_boxed_slice(),
        }
    }

    fn access_size(width: AccessWidth) -> Result<usize, DeviceError> {
        match width {
            AccessWidth::Byte | AccessWidth::Word | AccessWidth::Dword => Ok(width.size()),
            AccessWidth::Qword => Err(DeviceError::Unsupported {
                operation: "access x86 PCI configuration ports",
                detail: "PCI configuration mechanism #1 supports accesses up to 32 bits".into(),
            }),
        }
    }

    fn decode_access(access: &DeviceAccess) -> Result<(u16, usize), DeviceError> {
        if access.bus() != BusKind::Port {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        let size = Self::access_size(access.width())?;
        let port = u16::try_from(access.address()).map_err(|_| DeviceError::OutOfRange {
            addr: access.address(),
        })?;
        Ok((port, size))
    }
}

impl Default for X86PciConfigDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl Device for X86PciConfigDevice {
    fn name(&self) -> &str {
        "x86-pci-config"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn read(&self, access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        let (port, size) = Self::decode_access(access)?;
        let mut state = self.state.lock_irqsave();

        if (CONFIG_ADDRESS_PORT..CONFIG_DATA_PORT).contains(&port) {
            let offset = usize::from(port - CONFIG_ADDRESS_PORT);
            if offset + size > 4 {
                return Err(DeviceError::OutOfRange {
                    addr: access.address(),
                });
            }
            return Ok(read_bytes(&state.address.to_le_bytes(), offset, size));
        }

        if (CONFIG_DATA_PORT..CONFIG_DATA_PORT + 4).contains(&port) {
            let data_offset = usize::from(port - CONFIG_DATA_PORT);
            let Some((bus, device, function, register)) = state.selection(data_offset, size) else {
                return Ok(all_ones(size));
            };
            let Some(config) = state.function_mut(bus, device, function) else {
                return Ok(all_ones(size));
            };
            Ok(config.read(register, size))
        } else {
            Err(DeviceError::OutOfRange {
                addr: access.address(),
            })
        }
    }

    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        let (port, size) = Self::decode_access(access)?;
        let mut state = self.state.lock_irqsave();

        if (CONFIG_ADDRESS_PORT..CONFIG_DATA_PORT).contains(&port) {
            let offset = usize::from(port - CONFIG_ADDRESS_PORT);
            if offset + size > 4 {
                return Err(DeviceError::OutOfRange {
                    addr: access.address(),
                });
            }
            let mut address = state.address.to_le_bytes();
            write_bytes(&mut address, offset, size, value, &[u8::MAX; 4]);
            state.address = u32::from_le_bytes(address);
            return Ok(());
        }

        if (CONFIG_DATA_PORT..CONFIG_DATA_PORT + 4).contains(&port) {
            let data_offset = usize::from(port - CONFIG_DATA_PORT);
            if let Some((bus, device, function, register)) = state.selection(data_offset, size)
                && let Some(config) = state.function_mut(bus, device, function)
            {
                config.write(register, size, value);
            }
            Ok(())
        } else {
            Err(DeviceError::OutOfRange {
                addr: access.address(),
            })
        }
    }
}

impl PciConfigState {
    fn new() -> Self {
        Self {
            address: 0,
            host_bridge: PciFunction::host_bridge(),
            lpc_bridge: PciFunction::lpc_bridge(),
        }
    }

    fn selection(&self, data_offset: usize, size: usize) -> Option<(u8, u8, u8, usize)> {
        if self.address & CONFIG_ADDRESS_ENABLE == 0 {
            return None;
        }
        let register = (self.address as usize & 0xfc) + data_offset;
        if register.checked_add(size)? > CONFIG_SPACE_SIZE {
            return None;
        }
        Some((
            (self.address >> 16) as u8,
            ((self.address >> 11) & 0x1f) as u8,
            ((self.address >> 8) & 0x7) as u8,
            register,
        ))
    }

    fn function_mut(&mut self, bus: u8, device: u8, function: u8) -> Option<&mut PciFunction> {
        match (bus, device, function) {
            (0, 0, 0) => Some(&mut self.host_bridge),
            (0, 0x1f, 0) => Some(&mut self.lpc_bridge),
            _ => None,
        }
    }
}

impl PciFunction {
    fn host_bridge() -> Self {
        Self::new(0x8086, 0x29c0, 0x06, 0x00, 0x00, 0x00)
    }

    fn lpc_bridge() -> Self {
        let mut function = Self::new(0x8086, 0x2918, 0x06, 0x01, 0x00, 0x80);
        function.config[0x40] = 0x01;
        function.write_mask[0x40] = 0x80;
        function.write_mask[0x41] = 0xff;
        function.write_mask[0x44] = 0x87;
        function
    }

    fn new(
        vendor_id: u16,
        device_id: u16,
        class: u8,
        subclass: u8,
        programming_interface: u8,
        header_type: u8,
    ) -> Self {
        let mut function = Self {
            config: [0; CONFIG_SPACE_SIZE],
            write_mask: [0; CONFIG_SPACE_SIZE],
        };
        function.config[0..2].copy_from_slice(&vendor_id.to_le_bytes());
        function.config[2..4].copy_from_slice(&device_id.to_le_bytes());
        function.config[9] = programming_interface;
        function.config[10] = subclass;
        function.config[11] = class;
        function.config[14] = header_type;
        function.write_mask[4] = 0x07;
        function
    }

    fn read(&self, offset: usize, size: usize) -> u64 {
        read_bytes(&self.config, offset, size)
    }

    fn write(&mut self, offset: usize, size: usize, value: u64) {
        write_bytes(&mut self.config, offset, size, value, &self.write_mask);
    }
}

fn read_bytes(bytes: &[u8], offset: usize, size: usize) -> u64 {
    bytes[offset..offset + size]
        .iter()
        .enumerate()
        .fold(0, |value, (index, byte)| {
            value | (u64::from(*byte) << (index * 8))
        })
}

fn write_bytes(bytes: &mut [u8], offset: usize, size: usize, value: u64, masks: &[u8]) {
    for index in 0..size {
        let mask = masks[offset + index];
        let update = (value >> (index * 8)) as u8;
        bytes[offset + index] = (bytes[offset + index] & !mask) | (update & mask);
    }
}

fn all_ones(size: usize) -> u64 {
    u64::MAX >> ((8 - size) * 8)
}

#[cfg(test)]
mod tests {
    use axdevice_base::{DeviceContext, DeviceId, DeviceVcpuId};

    use super::*;

    struct NoMemory;

    impl DeviceContext for NoMemory {
        fn device_id(&self) -> DeviceId {
            DeviceId::new(0)
        }
    }

    fn write(device: &X86PciConfigDevice, port: u16, width: AccessWidth, value: u64) {
        device
            .write(
                &DeviceAccess::new(DeviceVcpuId::new(0), BusKind::Port, u64::from(port), width),
                value,
                &mut NoMemory,
            )
            .unwrap();
    }

    fn read(device: &X86PciConfigDevice, port: u16, width: AccessWidth) -> u64 {
        device
            .read(
                &DeviceAccess::new(DeviceVcpuId::new(0), BusKind::Port, u64::from(port), width),
                &mut NoMemory,
            )
            .unwrap()
    }

    #[test]
    fn q35_identity_and_lpc_pm_base_are_guest_owned() {
        let device = X86PciConfigDevice::new();
        write(
            &device,
            CONFIG_ADDRESS_PORT,
            AccessWidth::Dword,
            0x8000_0000,
        );
        assert_eq!(
            read(&device, CONFIG_DATA_PORT, AccessWidth::Dword),
            0x29c0_8086
        );

        write(
            &device,
            CONFIG_ADDRESS_PORT,
            AccessWidth::Dword,
            0x8000_f840,
        );
        write(&device, CONFIG_DATA_PORT, AccessWidth::Dword, 0x601);
        assert_eq!(read(&device, CONFIG_DATA_PORT, AccessWidth::Dword), 0x601);

        write(
            &device,
            CONFIG_ADDRESS_PORT,
            AccessWidth::Dword,
            0x8000_f844,
        );
        write(&device, CONFIG_DATA_PORT, AccessWidth::Byte, 0x80);
        assert_eq!(read(&device, CONFIG_DATA_PORT, AccessWidth::Byte), 0x80);
    }
}
