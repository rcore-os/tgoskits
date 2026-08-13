//! Device-graph factory for the x86 PCI configuration port window.

use std::sync::Arc;

use ax_driver::probe::pci::PciAddress;
use ax_std::os::arceos::sync::IrqSafeMutex;
use axdevice::*;
use axdevice_base::*;

const CONFIG_ADDRESS_ENABLE: u32 = 1 << 31;
const CONFIG_ADDRESS_PORT: u16 = 0xcf8;
const CONFIG_DATA_PORT: u16 = 0xcfc;
const CONFIG_SPACE_SIZE: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PciBdf {
    bus: u8,
    device: u8,
    function: u8,
}

impl PciBdf {
    const fn new(bus: u8, device: u8, function: u8) -> Self {
        Self {
            bus,
            device,
            function,
        }
    }
}

trait HostPciConfigAccess: Send + Sync {
    fn read_aligned_u32(&self, address: PciBdf, offset: u16) -> Option<u32>;
    fn update_aligned_u32(&self, address: PciBdf, offset: u16, byte_mask: u32, value: u32) -> bool;
}

struct CapturedEndpointConfig;

impl HostPciConfigAccess for CapturedEndpointConfig {
    fn read_aligned_u32(&self, address: PciBdf, offset: u16) -> Option<u32> {
        ax_driver::pci::read_handoff_config_u32(
            PciAddress::new(0, address.bus, address.device, address.function),
            offset,
        )
    }

    fn update_aligned_u32(&self, address: PciBdf, offset: u16, byte_mask: u32, value: u32) -> bool {
        ax_driver::pci::update_handoff_config_u32(
            PciAddress::new(0, address.bus, address.device, address.function),
            offset,
            byte_mask,
            value,
        )
    }
}

struct PassthroughPciConfigState {
    address: u32,
}

struct PassthroughPciConfigDevice {
    endpoint: PciBdf,
    backend: Arc<dyn HostPciConfigAccess>,
    q35: X86PciConfigDevice,
    state: IrqSafeMutex<PassthroughPciConfigState>,
    resources: Box<[Resource]>,
}

impl PassthroughPciConfigDevice {
    fn new(endpoint: PciBdf) -> Self {
        Self::new_with_backend(endpoint, Arc::new(CapturedEndpointConfig))
    }

    fn new_with_backend(endpoint: PciBdf, backend: Arc<dyn HostPciConfigAccess>) -> Self {
        Self {
            endpoint,
            backend,
            q35: X86PciConfigDevice::new(),
            state: IrqSafeMutex::new(PassthroughPciConfigState { address: 0 }),
            resources: std::vec![Resource::PortRange {
                base: CONFIG_ADDRESS_PORT,
                size: 8,
            }]
            .into_boxed_slice(),
        }
    }

    fn access_size(width: AccessWidth) -> DeviceResult<usize> {
        match width {
            AccessWidth::Byte | AccessWidth::Word | AccessWidth::Dword => Ok(width.size()),
            AccessWidth::Qword => Err(DeviceError::Unsupported {
                operation: "access passthrough PCI configuration ports",
                detail: "PCI configuration mechanism #1 supports accesses up to 32 bits".into(),
            }),
        }
    }

    fn selection(address: u32, data_offset: usize, size: usize) -> Option<(PciBdf, u16)> {
        if address & CONFIG_ADDRESS_ENABLE == 0 {
            return None;
        }
        let register = (address as usize & 0xfc) + data_offset;
        if register.checked_add(size)? > CONFIG_SPACE_SIZE {
            return None;
        }
        Some((
            PciBdf::new(
                (address >> 16) as u8,
                ((address >> 11) & 0x1f) as u8,
                ((address >> 8) & 0x7) as u8,
            ),
            register as u16,
        ))
    }

    const fn is_guest_owned_chipset(address: PciBdf) -> bool {
        matches!(
            (address.bus, address.device, address.function),
            (0, 0, 0) | (0, 0x1f, 0)
        )
    }
}

impl Device for PassthroughPciConfigDevice {
    fn name(&self) -> &str {
        "x86-passthrough-pci-config"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn access(
        &self,
        access: &BusAccess,
        context: &mut dyn DeviceAccess,
    ) -> DeviceResult<BusResponse> {
        if access.kind != BusKind::Port {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        let size = Self::access_size(access.width)?;
        let port = u16::try_from(access.addr)
            .map_err(|_| DeviceError::OutOfRange { addr: access.addr })?;
        let mut state = self.state.lock();

        if (CONFIG_ADDRESS_PORT..CONFIG_DATA_PORT).contains(&port) {
            let offset = usize::from(port - CONFIG_ADDRESS_PORT);
            if offset + size > 4 {
                return Err(DeviceError::OutOfRange { addr: access.addr });
            }
            if access.is_read {
                return Ok(BusResponse::Read {
                    value: read_bytes(&state.address.to_le_bytes(), offset, size),
                });
            }
            let mut address = state.address.to_le_bytes();
            write_bytes(&mut address, offset, size, access.data);
            state.address = u32::from_le_bytes(address);
            self.q35.access(access, context)?;
            return Ok(BusResponse::Write);
        }

        if !(CONFIG_DATA_PORT..CONFIG_DATA_PORT + 4).contains(&port) {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        let data_offset = usize::from(port - CONFIG_DATA_PORT);
        if data_offset + size > 4 {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        let Some((selected, register)) = Self::selection(state.address, data_offset, size) else {
            return Ok(if access.is_read {
                BusResponse::Read {
                    value: all_ones(size),
                }
            } else {
                BusResponse::Write
            });
        };
        if Self::is_guest_owned_chipset(selected) {
            return self.q35.access(access, context);
        }
        if selected != self.endpoint {
            return Ok(if access.is_read {
                BusResponse::Read {
                    value: all_ones(size),
                }
            } else {
                BusResponse::Write
            });
        }
        let aligned_register = register & !3;
        let byte_offset = usize::from(register & 3);
        if access.is_read {
            let value = self
                .backend
                .read_aligned_u32(selected, aligned_register)
                .unwrap_or(u32::MAX);
            Ok(BusResponse::Read {
                value: read_bytes(&value.to_le_bytes(), byte_offset, size),
            })
        } else {
            let mask = ((1_u64 << (size * 8)) - 1) as u32;
            let byte_mask = mask << (byte_offset * 8);
            let value = (access.data as u32 & mask) << (byte_offset * 8);
            let _ = self
                .backend
                .update_aligned_u32(selected, aligned_register, byte_mask, value);
            Ok(BusResponse::Write)
        }
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

fn write_bytes(bytes: &mut [u8], offset: usize, size: usize, value: u64) {
    for index in 0..size {
        bytes[offset + index] = (value >> (index * 8)) as u8;
    }
}

fn all_ones(size: usize) -> u64 {
    u64::MAX >> ((8 - size) * 8)
}

pub(super) struct X86PciConfigModel;

pub(super) struct X86PassthroughPciConfigModel {
    endpoint: PciBdf,
}

impl X86PassthroughPciConfigModel {
    pub(super) const fn new(bus: u8, device: u8, function: u8) -> Self {
        Self {
            endpoint: PciBdf::new(bus, device, function),
        }
    }
}

impl DeviceModel for X86PassthroughPciConfigModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new().with_pio(
            ResourceSlot::new("registers")?,
            axdevice::X86PciConfigDevice::PORT_SIZE,
            1,
            ResourceRequest::Fixed(axdevice::X86PciConfigDevice::PORT_BASE),
        )
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let range = context.pio(&ResourceSlot::new("registers")?)?;
        let expected = (
            axdevice::X86PciConfigDevice::PORT_BASE,
            axdevice::X86PciConfigDevice::PORT_SIZE,
        );
        if range != expected {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build x86 passthrough PCI configuration",
                detail: "planned PCI configuration range must be 0xcf8..=0xcff".into(),
            });
        }
        Ok(DeviceBundle::from_registration(DeviceRegistration::Device(
            Arc::new(PassthroughPciConfigDevice::new(self.endpoint)),
        )))
    }
}

impl DeviceModel for X86PciConfigModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new().with_pio(
            ResourceSlot::new("registers")?,
            axdevice::X86PciConfigDevice::PORT_SIZE,
            1,
            ResourceRequest::Fixed(axdevice::X86PciConfigDevice::PORT_BASE),
        )
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let range = context.pio(&ResourceSlot::new("registers")?)?;
        let expected = (
            axdevice::X86PciConfigDevice::PORT_BASE,
            axdevice::X86PciConfigDevice::PORT_SIZE,
        );
        if range != expected {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build x86 PCI configuration",
                detail: "planned PCI configuration range must be 0xcf8..=0xcff".into(),
            });
        }
        Ok(DeviceBundle::from_registration(DeviceRegistration::Device(
            std::sync::Arc::new(axdevice::X86PciConfigDevice::new()),
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axdevice_base::{
        AccessWidth, BusAccess, BusKind, BusResponse, Device, DeviceAccess, DeviceId,
    };

    use super::{HostPciConfigAccess, PassthroughPciConfigDevice, PciBdf};

    #[derive(Default)]
    struct FakeHostConfig {
        reads: Mutex<Vec<(PciBdf, u16)>>,
        writes: Mutex<Vec<(PciBdf, u16, u32)>>,
    }

    impl HostPciConfigAccess for FakeHostConfig {
        fn read_aligned_u32(&self, address: PciBdf, offset: u16) -> Option<u32> {
            self.reads.lock().unwrap().push((address, offset));
            Some(0x4433_2211)
        }

        fn update_aligned_u32(
            &self,
            address: PciBdf,
            offset: u16,
            byte_mask: u32,
            value: u32,
        ) -> bool {
            let old = 0x4433_2211;
            let updated = (old & !byte_mask) | (value & byte_mask);
            self.writes.lock().unwrap().push((address, offset, updated));
            true
        }
    }

    struct NoMemory;

    impl DeviceAccess for NoMemory {
        fn device_id(&self) -> DeviceId {
            DeviceId::new(0)
        }
    }

    fn write(device: &dyn Device, port: u16, width: AccessWidth, value: u64) {
        device
            .access(
                &BusAccess {
                    kind: BusKind::Port,
                    is_read: false,
                    addr: u64::from(port),
                    width,
                    data: value,
                },
                &mut NoMemory,
            )
            .unwrap();
    }

    fn read(device: &dyn Device, port: u16, width: AccessWidth) -> u64 {
        match device
            .access(
                &BusAccess {
                    kind: BusKind::Port,
                    is_read: true,
                    addr: u64::from(port),
                    width,
                    data: 0,
                },
                &mut NoMemory,
            )
            .unwrap()
        {
            BusResponse::Read { value } => value,
            BusResponse::Write => unreachable!(),
        }
    }

    #[test]
    fn passthrough_config_rejects_unassigned_non_chipset_bdfs_without_host_io() {
        let host = Arc::new(FakeHostConfig::default());
        let device =
            PassthroughPciConfigDevice::new_with_backend(PciBdf::new(0, 3, 0), host.clone());

        for selection in [0x8001_1800_u64, 0x8000_2000, 0x8000_1900] {
            write(&device, 0xcf8, AccessWidth::Dword, selection);
            assert_eq!(
                read(&device, 0xcfc, AccessWidth::Dword),
                u64::from(u32::MAX)
            );
            write(&device, 0xcfc, AccessWidth::Dword, 0xdead_beef);
        }

        assert!(host.reads.lock().unwrap().is_empty());
        assert!(host.writes.lock().unwrap().is_empty());
    }

    #[test]
    fn passthrough_config_forwards_only_the_assigned_endpoint() {
        let host = Arc::new(FakeHostConfig::default());
        let endpoint = PciBdf::new(0, 3, 0);
        let device = PassthroughPciConfigDevice::new_with_backend(endpoint, host.clone());

        write(&device, 0xcf8, AccessWidth::Dword, 0x8000_1810);
        assert_eq!(read(&device, 0xcfd, AccessWidth::Word), 0x3322);
        write(&device, 0xcfe, AccessWidth::Byte, 0xaa);

        assert_eq!(*host.reads.lock().unwrap(), [(endpoint, 0x10)]);
        assert_eq!(
            *host.writes.lock().unwrap(),
            [(endpoint, 0x10, 0x44aa_2211)]
        );
    }

    #[test]
    fn passthrough_config_keeps_guest_owned_q35_lpc_pm_base() {
        let host = Arc::new(FakeHostConfig::default());
        let device =
            PassthroughPciConfigDevice::new_with_backend(PciBdf::new(0, 3, 0), host.clone());

        write(&device, 0xcf8, AccessWidth::Dword, 0x8000_f840);
        write(&device, 0xcfc, AccessWidth::Dword, 0x601);
        assert_eq!(read(&device, 0xcfc, AccessWidth::Dword), 0x601);

        write(&device, 0xcf8, AccessWidth::Dword, 0x8000_f844);
        write(&device, 0xcfc, AccessWidth::Byte, 0x80);
        assert_eq!(read(&device, 0xcfc, AccessWidth::Byte), 0x80);

        assert!(host.reads.lock().unwrap().is_empty());
        assert!(host.writes.lock().unwrap().is_empty());
    }

    #[test]
    fn config_address_latch_is_guest_owned() {
        let host = Arc::new(FakeHostConfig::default());
        let device =
            PassthroughPciConfigDevice::new_with_backend(PciBdf::new(0, 3, 0), host.clone());

        write(&device, 0xcf8, AccessWidth::Dword, 0x8000_1810);

        assert_eq!(read(&device, 0xcf8, AccessWidth::Dword), 0x8000_1810);
        assert!(host.reads.lock().unwrap().is_empty());
        assert!(host.writes.lock().unwrap().is_empty());
    }
}
