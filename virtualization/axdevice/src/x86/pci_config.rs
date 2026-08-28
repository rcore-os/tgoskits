//! x86 PCI configuration-mechanism #1 and memory-aperture adapters.

use alloc::{boxed::Box, sync::Arc};

use ax_sync::SpinLock;
use axdevice_base::*;

use crate::{
    ConfigOffset, DeviceLifecycle, DeviceManagerResult, PciBdf, PciRootBinding, PciRootState,
    PciSegment, all_ones, read_bytes,
};

const CONFIG_ADDRESS_ENABLE: u32 = 1 << 31;
const CONFIG_ADDRESS_PORT: u16 = 0xcf8;
const CONFIG_DATA_PORT: u16 = 0xcfc;

/// CF8/CFC frontend that only decodes x86 port accesses.
pub struct X86PciConfigFrontend {
    address: SpinLock<u32>,
    root: Arc<PciRootState>,
    resources: Box<[Resource]>,
}

impl X86PciConfigFrontend {
    /// Base of the PCI configuration address/data port window.
    pub const PORT_BASE: u16 = CONFIG_ADDRESS_PORT;
    /// Size of the combined address/data port window.
    pub const PORT_SIZE: u16 = 8;
    /// Creates a frontend for one generic PCI root.
    pub fn new(root: Arc<PciRootState>) -> Self {
        Self {
            address: SpinLock::new(0),
            root,
            resources: alloc::vec![Resource::PortRange {
                base: Self::PORT_BASE,
                size: Self::PORT_SIZE
            }]
            .into_boxed_slice(),
        }
    }

    fn decode_access(access: &DeviceAccess) -> DeviceResult<(u16, usize)> {
        if access.bus() != BusKind::Port {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        if access.width() == AccessWidth::Qword {
            return Err(DeviceError::Unsupported {
                operation: "access x86 PCI configuration port",
                detail: "CF8/CFC supports byte, word, and dword accesses only".into(),
            });
        }
        let port = u16::try_from(access.address()).map_err(|_| DeviceError::OutOfRange {
            addr: access.address(),
        })?;
        Ok((port, access.width().size()))
    }

    /// Classifies one data-port access against the latched CF8 address.
    ///
    /// Returns `Ok(None)` for a disabled enable bit or an out-of-range
    /// register window (PCI-compatible absent-function behavior), and a
    /// structured [`DeviceError::OutOfRange`] when the access leaves the
    /// 4-byte data window.
    fn selection(
        &self,
        data_offset: usize,
        size: usize,
    ) -> Result<Option<(PciBdf, ConfigOffset)>, DeviceError> {
        let address = *self.address.lock_irqsave();
        if address & CONFIG_ADDRESS_ENABLE == 0 {
            return Ok(None);
        }
        let crosses_window = data_offset
            .checked_add(size)
            .map(|end| end > 4)
            .unwrap_or(true);
        if crosses_window {
            return Err(DeviceError::OutOfRange {
                addr: u64::from(CONFIG_DATA_PORT + data_offset as u16),
            });
        }
        let Some(register) = (address as usize & 0xfc).checked_add(data_offset) else {
            return Ok(None);
        };
        let Ok(register) = u16::try_from(register) else {
            return Ok(None);
        };
        let bdf = match PciBdf::new(
            PciSegment::new(0),
            (address >> 16) as u8,
            ((address >> 11) & 0x1f) as u8,
            ((address >> 8) & 0x7) as u8,
        ) {
            Ok(bdf) => bdf,
            Err(_) => return Ok(None),
        };
        let Ok(register) = ConfigOffset::new(register) else {
            return Ok(None);
        };
        Ok(Some((bdf, register)))
    }

    /// Reads one data-window access, splitting unaligned lanes into
    /// single-byte config reads like the legacy frontend and real bridges.
    fn read_data_window(
        &self,
        bdf: PciBdf,
        register: ConfigOffset,
        data_offset: usize,
        size: usize,
        width: AccessWidth,
    ) -> DeviceResult<u64> {
        if data_offset.is_multiple_of(size) {
            return self
                .root
                .read_config(bdf, register, width)
                .map_err(pci_access_error);
        }
        let mut value = 0;
        for index in 0..size {
            let lane =
                ConfigOffset::new(register.value() + index as u16).map_err(pci_access_error)?;
            let byte = self
                .root
                .read_config(bdf, lane, AccessWidth::Byte)
                .map_err(pci_access_error)?;
            value |= byte << (index * 8);
        }
        Ok(value)
    }

    /// Writes one data-window access with the same lane-splitting rule.
    fn write_data_window(
        &self,
        bdf: PciBdf,
        register: ConfigOffset,
        data_offset: usize,
        size: usize,
        width: AccessWidth,
        value: u64,
    ) -> DeviceResult {
        if data_offset.is_multiple_of(size) {
            return self
                .root
                .write_config(bdf, register, width, value)
                .map_err(pci_access_error);
        }
        for index in 0..size {
            let lane =
                ConfigOffset::new(register.value() + index as u16).map_err(pci_access_error)?;
            self.root
                .write_config(bdf, lane, AccessWidth::Byte, value >> (index * 8))
                .map_err(pci_access_error)?;
        }
        Ok(())
    }
}

impl Device for X86PciConfigFrontend {
    fn name(&self) -> &str {
        "x86-pci-config"
    }
    fn resources(&self) -> &[Resource] {
        &self.resources
    }
    fn read(&self, access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        let (port, size) = Self::decode_access(access)?;
        if (CONFIG_ADDRESS_PORT..CONFIG_DATA_PORT).contains(&port) {
            let offset = usize::from(port - CONFIG_ADDRESS_PORT);
            if offset + size > 4 {
                return Err(DeviceError::OutOfRange {
                    addr: access.address(),
                });
            }
            return Ok(read_bytes(
                &self.address.lock_irqsave().to_le_bytes(),
                offset,
                size,
            ));
        }
        if (CONFIG_DATA_PORT..CONFIG_DATA_PORT + 4).contains(&port) {
            let offset = usize::from(port - CONFIG_DATA_PORT);
            let size = access.width().size();
            return match self.selection(offset, size)? {
                None => Ok(all_ones(size)),
                Some((bdf, register)) => {
                    self.read_data_window(bdf, register, offset, size, access.width())
                }
            };
        }
        Err(DeviceError::OutOfRange {
            addr: access.address(),
        })
    }
    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        let (port, size) = Self::decode_access(access)?;
        if (CONFIG_ADDRESS_PORT..CONFIG_DATA_PORT).contains(&port) {
            let offset = usize::from(port - CONFIG_ADDRESS_PORT);
            if offset + size > 4 {
                return Err(DeviceError::OutOfRange {
                    addr: access.address(),
                });
            }
            let mut address = self.address.lock_irqsave();
            let mut bytes = address.to_le_bytes();
            write_bytes(&mut bytes, offset, size, value);
            *address = u32::from_le_bytes(bytes);
            return Ok(());
        }
        if (CONFIG_DATA_PORT..CONFIG_DATA_PORT + 4).contains(&port) {
            let offset = usize::from(port - CONFIG_DATA_PORT);
            let size = access.width().size();
            return match self.selection(offset, size)? {
                None => Ok(()),
                Some((bdf, register)) => {
                    self.write_data_window(bdf, register, offset, size, access.width(), value)
                }
            };
        }
        Err(DeviceError::OutOfRange {
            addr: access.address(),
        })
    }
}

/// Single top-level MMIO device owning a PCI root's complete memory aperture.
pub struct PciMemoryApertureDevice {
    binding: Arc<PciRootBinding>,
    resources: Box<[Resource]>,
}
impl PciMemoryApertureDevice {
    /// Creates the aperture adapter from the graph-resolved range.
    pub fn new(base: u64, size: u64, binding: Arc<PciRootBinding>) -> Self {
        Self {
            binding,
            resources: alloc::vec![Resource::MmioRange { base, size }].into_boxed_slice(),
        }
    }
}
impl Device for PciMemoryApertureDevice {
    fn name(&self) -> &str {
        "pci-memory-aperture"
    }
    fn resources(&self) -> &[Resource] {
        &self.resources
    }
    fn read(&self, access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        match self.binding.read_bar(access.address(), access.width()) {
            Err(DeviceError::NotFound) => Ok(all_ones(access.width().size())),
            result => result,
        }
    }
    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        match self
            .binding
            .write_bar(access.address(), access.width(), value)
        {
            Err(DeviceError::NotFound) => Ok(()),
            result => result,
        }
    }
}

/// Lifecycle adapter restoring only root-owned PCI config and BAR state.
pub struct PciRootLifecycle(Arc<PciRootState>);
impl PciRootLifecycle {
    /// Creates a lifecycle adapter for one generic PCI root.
    pub const fn new(root: Arc<PciRootState>) -> Self {
        Self(root)
    }
}
impl DeviceLifecycle for PciRootLifecycle {
    fn reset(&self) -> DeviceManagerResult {
        self.0.reset();
        Ok(())
    }
    fn suspend(&self) -> DeviceManagerResult {
        Ok(())
    }
    fn resume(&self) -> DeviceManagerResult {
        Ok(())
    }
}

fn pci_access_error(error: crate::PciError) -> DeviceError {
    DeviceError::InvalidInput {
        operation: "access x86 PCI configuration",
        detail: alloc::format!("{error}"),
    }
}
fn write_bytes(bytes: &mut [u8], offset: usize, size: usize, value: u64) {
    for (index, byte) in bytes[offset..offset + size].iter_mut().enumerate() {
        *byte = (value >> (index * 8)) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        PciClass, PciEndpointIdentity, PciFunctionSpec, PciTopologyBuilder, ResourceRequest,
    };

    fn bdf(device: u8) -> PciBdf {
        PciBdf::new(PciSegment::new(0), 0, device, 0).unwrap()
    }

    fn frontend() -> X86PciConfigFrontend {
        let mut topology = PciTopologyBuilder::new();
        let host = PciFunctionSpec::new(
            crate::DeviceNodeId::new("host").unwrap(),
            PciEndpointIdentity::new(0x8086, 0x29c0, PciClass::new(0x06, 0, 0)),
        )
        .with_bdf(ResourceRequest::Fixed(bdf(0)));
        let lpc = PciFunctionSpec::new(
            crate::DeviceNodeId::new("lpc").unwrap(),
            PciEndpointIdentity::new(0x8086, 0x2918, PciClass::new(0x06, 1, 0)),
        )
        .with_bdf(ResourceRequest::Fixed(bdf(0x1f)))
        .with_platform_config_byte(ConfigOffset::new(0x0e).unwrap(), 0x80, 0)
        .unwrap()
        .with_platform_config_byte(ConfigOffset::new(0x40).unwrap(), 1, 0x80)
        .unwrap()
        .with_platform_config_byte(ConfigOffset::new(0x41).unwrap(), 0, 0xff)
        .unwrap()
        .with_platform_config_byte(ConfigOffset::new(0x44).unwrap(), 0, 0x87)
        .unwrap();
        topology.add_function(host).unwrap();
        topology.add_function(lpc).unwrap();
        X86PciConfigFrontend::new(Arc::new(PciRootState::new(Arc::new(
            topology.resolve(0xc000_0000..0xd000_0000).unwrap(),
        ))))
    }

    fn access(port: u16, width: AccessWidth) -> DeviceAccess {
        DeviceAccess::new(DeviceVcpuId::new(0), BusKind::Port, u64::from(port), width)
    }

    fn write(frontend: &X86PciConfigFrontend, port: u16, width: AccessWidth, value: u64) {
        frontend
            .write(
                &access(port, width),
                value,
                &mut NoopDeviceContext::new(DeviceId::new(0)),
            )
            .unwrap();
    }

    fn read(frontend: &X86PciConfigFrontend, port: u16, width: AccessWidth) -> u64 {
        frontend
            .read(
                &access(port, width),
                &mut NoopDeviceContext::new(DeviceId::new(0)),
            )
            .unwrap()
    }

    #[test]
    fn generic_root_preserves_q35_identity_and_lpc_pm_fields() {
        let frontend = frontend();
        write(
            &frontend,
            CONFIG_ADDRESS_PORT,
            AccessWidth::Dword,
            0x8000_0000,
        );
        assert_eq!(
            read(&frontend, CONFIG_DATA_PORT, AccessWidth::Dword),
            0x29c0_8086
        );

        write(
            &frontend,
            CONFIG_ADDRESS_PORT,
            AccessWidth::Dword,
            0x8000_f840,
        );
        write(&frontend, CONFIG_DATA_PORT, AccessWidth::Dword, 0x601);
        assert_eq!(read(&frontend, CONFIG_DATA_PORT, AccessWidth::Dword), 0x601);

        write(
            &frontend,
            CONFIG_ADDRESS_PORT,
            AccessWidth::Dword,
            0x8000_f844,
        );
        write(&frontend, CONFIG_DATA_PORT, AccessWidth::Byte, 0x80);
        assert_eq!(read(&frontend, CONFIG_DATA_PORT, AccessWidth::Byte), 0x80);

        write(
            &frontend,
            CONFIG_ADDRESS_PORT,
            AccessWidth::Dword,
            0x8000_0800,
        );
        assert_eq!(
            read(&frontend, CONFIG_DATA_PORT, AccessWidth::Dword),
            u32::MAX.into()
        );
    }

    #[test]
    fn qword_data_port_access_is_rejected_as_unsupported() {
        let frontend = frontend();
        let error = read_error(&frontend, CONFIG_DATA_PORT, AccessWidth::Qword);
        assert!(matches!(error, DeviceError::Unsupported { .. }));
    }

    #[test]
    fn cross_window_data_port_accesses_return_structured_errors() {
        let frontend = frontend();
        write(
            &frontend,
            CONFIG_ADDRESS_PORT,
            AccessWidth::Dword,
            0x8000_0000,
        );
        // A dword access at CFE and a word at CFF leave the 4-byte data window.
        for port in [
            CONFIG_DATA_PORT + 2, // dword straddle
            CONFIG_DATA_PORT + 3, // word straddle
        ] {
            let read_error = read_error(&frontend, port, AccessWidth::Dword);
            assert!(matches!(read_error, DeviceError::OutOfRange { .. }));
            let write_error = write_error(&frontend, port, AccessWidth::Dword, 0);
            assert!(matches!(write_error, DeviceError::OutOfRange { .. }));
        }
    }

    #[test]
    fn unaligned_word_access_merges_config_bytes_like_the_legacy_frontend() {
        let frontend = frontend();
        // Select vendor/device register 0; a word starting at CFD covers bytes
        // 1 and 2 (vendor-high 0x80, device-low 0xc0) and must merge
        // little-endian instead of failing.
        write(
            &frontend,
            CONFIG_ADDRESS_PORT,
            AccessWidth::Dword,
            0x8000_0000,
        );
        assert_eq!(
            read(&frontend, CONFIG_DATA_PORT + 1, AccessWidth::Word),
            0xc080
        );

        // The same byte lanes are written independently and honor write
        // masks: both selected lanes are read-only for this function, so the
        // write has no effect instead of failing the guest access.
        write(
            &frontend,
            CONFIG_ADDRESS_PORT,
            AccessWidth::Dword,
            0x8000_0004,
        );
        write(&frontend, CONFIG_DATA_PORT + 1, AccessWidth::Word, 0xffff);
        assert_eq!(read(&frontend, CONFIG_DATA_PORT + 1, AccessWidth::Word), 0);
        write(
            &frontend,
            CONFIG_ADDRESS_PORT,
            AccessWidth::Dword,
            0x8000_0004,
        );
        assert_eq!(read(&frontend, CONFIG_DATA_PORT, AccessWidth::Byte), 0);
    }

    fn read_error(frontend: &X86PciConfigFrontend, port: u16, width: AccessWidth) -> DeviceError {
        frontend
            .read(
                &access(port, width),
                &mut NoopDeviceContext::new(DeviceId::new(0)),
            )
            .unwrap_err()
    }

    fn write_error(
        frontend: &X86PciConfigFrontend,
        port: u16,
        width: AccessWidth,
        value: u64,
    ) -> DeviceError {
        frontend
            .write(
                &access(port, width),
                value,
                &mut NoopDeviceContext::new(DeviceId::new(0)),
            )
            .unwrap_err()
    }
}
