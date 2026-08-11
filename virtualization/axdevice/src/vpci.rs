use alloc::{boxed::Box, collections::BTreeMap, format, string::String, sync::Arc, vec};

use ax_kspin::SpinNoIrq;
use axdevice_base::{
    AccessWidth, BusAccess, BusKind, BusResponse, ControllerInputId, Device, DeviceAccess,
    DeviceError, DeviceResult, InterruptControllerId, InterruptSharing, InterruptTrigger, IrqLine,
    Resource,
};
use axvm_types::GuestPhysAddr;

use crate::{
    DeviceBuildContext, DeviceBundle, DeviceFirmwareSpec, DeviceManagerResult, DeviceModel,
    DeviceRequirements, ResourceRequest, ResourceSlot, SharedMemoryRequest,
};

const PCI_CONFIG_SPACE_SIZE: usize = 256;
const PCI_ECAM_FUNCTION_SIZE: usize = 4096;
const ECAM_SLOT: &str = "ecam";
const BAR0_SLOT: &str = "bar0";
const BAR2_SLOT: &str = "bar2";
const IRQ_SLOT: &str = "irq";

const DEFAULT_BUS: u8 = 0;
const DEFAULT_DEVICE: u8 = 5;
const DEFAULT_FUNCTION: u8 = 0;
const DEFAULT_VENDOR_ID: u16 = 0xaaaa;
const DEFAULT_DEVICE_ID: u16 = 0x0001;
const IVSHMEM_VENDOR_ID: u16 = 0x1af4;
const IVSHMEM_DEVICE_ID: u16 = 0x1110;
const DEFAULT_BAR0_SIZE: usize = 0x1000;

const PCI_COMMAND_OFFSET: usize = 0x04;
const PCI_STATUS_OFFSET: usize = 0x06;
const PCI_BAR0_OFFSET: usize = 0x10;
const PCI_BAR2_OFFSET: usize = 0x18;
const PCI_INTERRUPT_LINE_OFFSET: usize = 0x3c;
const PCI_INTERRUPT_PIN_OFFSET: usize = 0x3d;
const PCI_BAR_SIZE_PROBE: u32 = u32::MAX;
const PCI_BAR_MEM_SPACE: u32 = 0x0;
const PCI_BAR_MEM32_MASK: u32 = 0xffff_fff0;
const PCI_COMMAND_MEMORY_SPACE: u16 = 1 << 1;
const PCI_COMMAND_INTX_DISABLE: u16 = 1 << 10;
const PCI_COMMAND_WRITABLE_BITS: u16 = PCI_COMMAND_MEMORY_SPACE | PCI_COMMAND_INTX_DISABLE;
const PCI_STATUS_INTERRUPT: u16 = 1 << 3;
const PCI_INTERRUPT_LINE_NONE: u8 = 0xff;
const PCI_INTERRUPT_PIN_NONE: u8 = 0;
const PCI_INTERRUPT_PIN_INTA: u8 = 1;

const BAR0_ID_OFFSET: usize = 0x00;
const BAR0_SCRATCH_OFFSET: usize = 0x08;
const BAR0_ID: u32 = 0x4158_5043; // "AXPC"

const IVSHMEM_BAR0_INT_CONTROL_OFFSET: usize = 0x00;
const IVSHMEM_BAR0_INT_STATUS_OFFSET: usize = 0x04;
const IVSHMEM_BAR0_ID_OFFSET: usize = 0x08;
const IVSHMEM_BAR0_DOORBELL_OFFSET: usize = 0x0c;
const IVSHMEM_BAR0_MAX_PEERS_OFFSET: usize = 0x10;
const IVSHMEM_BAR0_STATE_OFFSET: usize = 0x14;
const IVSHMEM_BAR0_LINK_STATUS_OFFSET: usize = 0x18;
const IVSHMEM_BAR0_FEATURE_FLAGS_OFFSET: usize = 0x1c;
const IVSHMEM_LINK_READY: u32 = 1;
const IVSHMEM_INT_STATUS_DOORBELL: u32 = 1;

static IVSHMEM_DOORBELL_ENDPOINTS: SpinNoIrq<BTreeMap<(u32, u32), IvshmemDoorbellEndpoint>> =
    SpinNoIrq::new(BTreeMap::new());

#[derive(Clone)]
struct IvshmemDoorbellEndpoint {
    irq: Option<IrqLine>,
    state: Arc<SpinNoIrq<VirtualPciState>>,
}

/// Endpoint behavior exposed behind a virtual PCI host bridge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VirtualPciEndpointKind {
    /// Minimal dummy endpoint used by vPCI infrastructure tests.
    Dummy,
    /// AxVisor-compatible ivshmem-like endpoint prototype.
    Ivshmem(IvshmemPciConfig),
}

/// Configuration for the first ivshmem-like PCI endpoint prototype.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IvshmemPciConfig {
    /// Link identifier shared by peers in one communication group.
    pub link_id: u32,
    /// Current peer identifier.
    pub peer_id: u32,
    /// Number of peers in the link.
    pub peers: u32,
    /// Feature bits exposed to the guest.
    pub feature_flags: u32,
}

impl Default for IvshmemPciConfig {
    fn default() -> Self {
        Self {
            link_id: 0,
            peer_id: 0,
            peers: 2,
            feature_flags: 0,
        }
    }
}

/// A minimal PCI endpoint exposed through a virtual ECAM host bridge.
#[derive(Clone, Copy, Debug)]
pub struct VirtualPciEndpointConfig {
    /// PCI bus number.
    pub bus: u8,
    /// PCI device number.
    pub device: u8,
    /// PCI function number.
    pub function: u8,
    /// PCI vendor ID.
    pub vendor_id: u16,
    /// PCI device ID.
    pub device_id: u16,
    /// Guest physical base address of BAR0.
    pub bar0_base: u64,
    /// BAR0 size in bytes.
    pub bar0_size: u64,
    /// Guest physical base address of BAR2.
    pub bar2_base: u64,
    /// BAR2 size in bytes.
    pub bar2_size: u64,
    /// Optional host physical backing address for BAR2 shared memory.
    pub bar2_host_backing: u64,
    /// Legacy PCI interrupt line exposed in configuration space.
    pub interrupt_line: u8,
    /// Legacy PCI interrupt pin exposed in configuration space.
    pub interrupt_pin: u8,
    /// Endpoint-specific behavior.
    pub kind: VirtualPciEndpointKind,
}

impl Default for VirtualPciEndpointConfig {
    fn default() -> Self {
        Self {
            bus: DEFAULT_BUS,
            device: DEFAULT_DEVICE,
            function: DEFAULT_FUNCTION,
            vendor_id: DEFAULT_VENDOR_ID,
            device_id: DEFAULT_DEVICE_ID,
            bar0_base: 0,
            bar0_size: DEFAULT_BAR0_SIZE as u64,
            bar2_base: 0,
            bar2_size: 0,
            bar2_host_backing: 0,
            interrupt_line: PCI_INTERRUPT_LINE_NONE,
            interrupt_pin: PCI_INTERRUPT_PIN_NONE,
            kind: VirtualPciEndpointKind::Dummy,
        }
    }
}

impl VirtualPciEndpointConfig {
    fn from_cfg_list(cfg_list: &[usize]) -> DeviceManagerResult<Self> {
        Self::from_cfg_list_with_kind(cfg_list, VirtualPciEndpointKind::Dummy)
    }

    fn from_cfg_list_with_kind(
        cfg_list: &[usize],
        kind: VirtualPciEndpointKind,
    ) -> DeviceManagerResult<Self> {
        let mut config = Self {
            kind,
            ..Self::default()
        };
        if let Some(value) = cfg_list.first().copied() {
            config.bus = checked_u8(value, "PCI bus")?;
        }
        if let Some(value) = cfg_list.get(1).copied() {
            config.device = checked_u8(value, "PCI device")?;
            if config.device >= 32 {
                return invalid_vpci_config("PCI device must be less than 32");
            }
        }
        if let Some(value) = cfg_list.get(2).copied() {
            config.function = checked_u8(value, "PCI function")?;
            if config.function >= 8 {
                return invalid_vpci_config("PCI function must be less than 8");
            }
        }
        if let Some(value) = cfg_list.get(3).copied() {
            config.vendor_id = checked_u16(value, "PCI vendor ID")?;
        }
        if let Some(value) = cfg_list.get(4).copied() {
            config.device_id = checked_u16(value, "PCI device ID")?;
        }
        if let Some(value) = cfg_list.get(7).copied() {
            config.bar0_base = value as u64;
        } else if let Some(value) = cfg_list.get(5).copied() {
            config.bar0_base = value as u64;
        }
        if let Some(value) = cfg_list.get(8).copied() {
            config.bar0_size = value as u64;
        }
        validate_bar0_config(config.bar0_base, config.bar0_size)?;
        if let Some(value) = cfg_list.get(13).copied() {
            config.bar2_base = value as u64;
        }
        if let Some(value) = cfg_list.get(14).copied() {
            config.bar2_size = value as u64;
        }
        if let Some(value) = cfg_list.get(15).copied() {
            config.bar2_host_backing = value as u64;
        }
        if config.bar2_size != 0 || config.bar2_base != 0 {
            validate_bar_config("BAR2", config.bar2_base, config.bar2_size)?;
        }
        Ok(config)
    }

    fn ivshmem_from_cfg_list(cfg_list: &[usize]) -> DeviceManagerResult<Self> {
        let mut ivshmem = IvshmemPciConfig::default();
        if let Some(value) = cfg_list.get(9).copied() {
            ivshmem.link_id = checked_u32(value, "ivshmem link ID")?;
        }
        if let Some(value) = cfg_list.get(10).copied() {
            ivshmem.peer_id = checked_u32(value, "ivshmem peer ID")?;
        }
        if let Some(value) = cfg_list.get(11).copied() {
            ivshmem.peers = checked_u32(value, "ivshmem peer count")?;
        }
        if ivshmem.peers == 0 {
            return invalid_vpci_config("ivshmem peer count must be non-zero");
        }
        if ivshmem.peer_id >= ivshmem.peers {
            return invalid_vpci_config("ivshmem peer ID must be less than peer count");
        }
        if let Some(value) = cfg_list.get(12).copied() {
            ivshmem.feature_flags = checked_u32(value, "ivshmem feature flags")?;
        }

        let mut config =
            Self::from_cfg_list_with_kind(cfg_list, VirtualPciEndpointKind::Ivshmem(ivshmem))?;
        config.vendor_id = cfg_list
            .get(3)
            .copied()
            .map(|value| checked_u16(value, "PCI vendor ID"))
            .transpose()?
            .unwrap_or(IVSHMEM_VENDOR_ID);
        config.device_id = cfg_list
            .get(4)
            .copied()
            .map(|value| checked_u16(value, "PCI device ID"))
            .transpose()?
            .unwrap_or(IVSHMEM_DEVICE_ID);
        Ok(config)
    }
}

struct VirtualPciState {
    command: u16,
    bar0_value: u32,
    bar0_probe: bool,
    bar2_value: u32,
    bar2_probe: bool,
    scratch: u32,
    ivshmem_state: u32,
    int_control: u32,
    int_status: u32,
    last_doorbell: u32,
}

/// Minimal virtual PCI host bridge backed by ECAM MMIO configuration space.
pub struct VirtualPciHost {
    name: String,
    base: GuestPhysAddr,
    size: usize,
    endpoint: VirtualPciEndpointConfig,
    resources: Box<[Resource]>,
    config_space: [u8; PCI_CONFIG_SPACE_SIZE],
    state: Arc<SpinNoIrq<VirtualPciState>>,
    irq: Option<IrqLine>,
}

impl VirtualPciHost {
    /// Creates a virtual PCI host bridge with one dummy endpoint.
    pub fn new(
        name: String,
        base: GuestPhysAddr,
        size: usize,
        endpoint: VirtualPciEndpointConfig,
        irq: Option<IrqLine>,
    ) -> DeviceManagerResult<Self> {
        if size < PCI_ECAM_FUNCTION_SIZE {
            return invalid_vpci_config("ECAM range must be at least one 4 KiB function");
        }

        let mut config_space = [0u8; PCI_CONFIG_SPACE_SIZE];
        config_space[0x00..0x02].copy_from_slice(&endpoint.vendor_id.to_le_bytes());
        config_space[0x02..0x04].copy_from_slice(&endpoint.device_id.to_le_bytes());
        config_space[0x08] = 0x00; // revision id
        match endpoint.kind {
            VirtualPciEndpointKind::Dummy => {
                config_space[0x09] = 0x00; // programming interface
                config_space[0x0a] = 0x00; // subclass
                config_space[0x0b] = 0xff; // vendor-specific class
            }
            VirtualPciEndpointKind::Ivshmem(_) => {
                config_space[0x09] = 0x00; // programming interface
                config_space[0x0a] = 0x00; // RAM memory
                config_space[0x0b] = 0x05; // memory controller
            }
        }
        config_space[0x0e] = 0x00; // endpoint header
        config_space[PCI_INTERRUPT_LINE_OFFSET] = endpoint.interrupt_line;
        config_space[PCI_INTERRUPT_PIN_OFFSET] = endpoint.interrupt_pin;
        config_space[PCI_BAR0_OFFSET..PCI_BAR0_OFFSET + 4]
            .copy_from_slice(&(endpoint.bar0_base as u32 | PCI_BAR_MEM_SPACE).to_le_bytes());
        if endpoint.bar2_size != 0 {
            config_space[PCI_BAR2_OFFSET..PCI_BAR2_OFFSET + 4]
                .copy_from_slice(&(endpoint.bar2_base as u32 | PCI_BAR_MEM_SPACE).to_le_bytes());
        }

        let resources = vec![
            Resource::MmioRange {
                base: base.as_usize() as u64,
                size: size as u64,
            },
            Resource::MmioRange {
                base: endpoint.bar0_base,
                size: endpoint.bar0_size,
            },
        ]
        .into_boxed_slice();
        let state = Arc::new(SpinNoIrq::new(VirtualPciState {
            command: 0,
            bar0_value: endpoint.bar0_base as u32 | PCI_BAR_MEM_SPACE,
            bar0_probe: false,
            bar2_value: endpoint.bar2_base as u32 | PCI_BAR_MEM_SPACE,
            bar2_probe: false,
            scratch: 0,
            ivshmem_state: 0,
            int_control: 0,
            int_status: 0,
            last_doorbell: 0,
        }));

        if let VirtualPciEndpointKind::Ivshmem(config) = endpoint.kind {
            register_ivshmem_doorbell_endpoint(
                config.link_id,
                config.peer_id,
                state.clone(),
                irq.clone(),
            );
        }

        Ok(Self {
            name,
            base,
            size,
            endpoint,
            resources,
            config_space,
            state,
            irq,
        })
    }

    fn contains(&self, addr: GuestPhysAddr) -> bool {
        let base = self.base.as_usize();
        let end = base.saturating_add(self.size);
        let addr = addr.as_usize();
        addr >= base && addr < end
    }

    fn decode_ecam(&self, addr: GuestPhysAddr) -> Option<(u8, u8, u8, usize)> {
        if !self.contains(addr) {
            return None;
        }
        let offset = addr.as_usize() - self.base.as_usize();
        let bus = ((offset >> 20) & 0xff) as u8;
        let device = ((offset >> 15) & 0x1f) as u8;
        let function = ((offset >> 12) & 0x07) as u8;
        let register = offset & 0xfff;
        Some((bus, device, function, register))
    }

    fn selected_endpoint(&self, addr: GuestPhysAddr) -> Option<usize> {
        let (bus, device, function, register) = self.decode_ecam(addr)?;
        (bus == self.endpoint.bus
            && device == self.endpoint.device
            && function == self.endpoint.function
            && register < PCI_CONFIG_SPACE_SIZE)
            .then_some(register)
    }

    fn contains_bar0(&self, addr: GuestPhysAddr) -> bool {
        let base = self.endpoint.bar0_base;
        let end = base.saturating_add(self.endpoint.bar0_size);
        let addr = addr.as_usize() as u64;
        addr >= base && addr < end
    }

    fn bar0_offset(&self, addr: GuestPhysAddr) -> Option<usize> {
        self.contains_bar0(addr)
            .then_some((addr.as_usize() as u64 - self.endpoint.bar0_base) as usize)
    }

    fn absent_value(width: AccessWidth) -> u64 {
        match width {
            AccessWidth::Byte => u8::MAX as u64,
            AccessWidth::Word => u16::MAX as u64,
            AccessWidth::Dword => u32::MAX as u64,
            AccessWidth::Qword => u64::MAX,
        }
    }

    fn read_config(&self, addr: GuestPhysAddr, width: AccessWidth) -> DeviceResult<u64> {
        let Some(register) = self.selected_endpoint(addr) else {
            return Ok(Self::absent_value(width));
        };

        if register_access_touches(register, width, PCI_COMMAND_OFFSET, 4) {
            let state = self.state.lock();
            let command = state.command;
            let mut status = read_le_u16(&self.config_space, PCI_STATUS_OFFSET);
            if matches!(self.endpoint.kind, VirtualPciEndpointKind::Ivshmem(_))
                && state.int_status & state.int_control != 0
            {
                status |= PCI_STATUS_INTERRUPT;
            }
            let command_status = command as u32 | ((status as u32) << 16);
            return Ok(read_window(
                command_status as u64,
                register,
                width,
                PCI_COMMAND_OFFSET,
                4,
            ));
        }
        if register_access_touches(register, width, PCI_BAR0_OFFSET, 4) {
            let state = self.state.lock();
            let value = if state.bar0_probe {
                bar_size_mask(self.endpoint.bar0_size) | PCI_BAR_MEM_SPACE
            } else {
                state.bar0_value
            };
            return Ok(read_window(
                value as u64,
                register,
                width,
                PCI_BAR0_OFFSET,
                4,
            ));
        }
        if self.endpoint.bar2_size != 0
            && register_access_touches(register, width, PCI_BAR2_OFFSET, 4)
        {
            let state = self.state.lock();
            let value = if state.bar2_probe {
                bar_size_mask(self.endpoint.bar2_size) | PCI_BAR_MEM_SPACE
            } else {
                state.bar2_value
            };
            return Ok(read_window(
                value as u64,
                register,
                width,
                PCI_BAR2_OFFSET,
                4,
            ));
        }

        let mut value = 0u64;
        for byte in 0..width.size() {
            let Some(data) = self.config_space.get(register + byte) else {
                break;
            };
            value |= (*data as u64) << (byte * 8);
        }
        Ok(value)
    }

    fn write_config(&self, addr: GuestPhysAddr, width: AccessWidth, value: u64) -> DeviceResult {
        if !self.contains(addr) {
            return Err(DeviceError::OutOfRange {
                addr: addr.as_usize() as u64,
            });
        }
        let Some(register) = self.selected_endpoint(addr) else {
            return Ok(());
        };
        if width.size() > core::mem::size_of::<u64>() {
            return Err(DeviceError::InvalidInput {
                operation: "write virtual pci config space",
                detail: format!("unsupported access width {width:?}"),
            });
        }
        if register_access_touches(register, width, PCI_COMMAND_OFFSET, 2) {
            let mut state = self.state.lock();
            let merged = merge_window(
                state.command as u64,
                value,
                register,
                width,
                PCI_COMMAND_OFFSET,
                2,
            );
            state.command = (merged as u16) & PCI_COMMAND_WRITABLE_BITS;
            let irq_asserted = ivshmem_interrupt_asserted(&self.endpoint.kind, &state);
            drop(state);
            self.set_ivshmem_irq_level(irq_asserted)?;
            return Ok(());
        }
        if register_access_touches(register, width, PCI_BAR0_OFFSET, 4) {
            let mut state = self.state.lock();
            let merged = merge_window(
                state.bar0_value as u64,
                value,
                register,
                width,
                PCI_BAR0_OFFSET,
                4,
            ) as u32;
            if merged == PCI_BAR_SIZE_PROBE {
                state.bar0_probe = true;
            } else {
                state.bar0_probe = false;
                state.bar0_value = (merged & PCI_BAR_MEM32_MASK) | PCI_BAR_MEM_SPACE;
            }
            return Ok(());
        }
        if self.endpoint.bar2_size != 0
            && register_access_touches(register, width, PCI_BAR2_OFFSET, 4)
        {
            let mut state = self.state.lock();
            let merged = merge_window(
                state.bar2_value as u64,
                value,
                register,
                width,
                PCI_BAR2_OFFSET,
                4,
            ) as u32;
            if merged == PCI_BAR_SIZE_PROBE {
                state.bar2_probe = true;
            } else {
                state.bar2_probe = false;
                state.bar2_value = (merged & PCI_BAR_MEM32_MASK) | PCI_BAR_MEM_SPACE;
            }
            return Ok(());
        }
        Ok(())
    }

    fn read_bar0(&self, addr: GuestPhysAddr, width: AccessWidth) -> DeviceResult<u64> {
        let Some(offset) = self.bar0_offset(addr) else {
            return Err(DeviceError::OutOfRange {
                addr: addr.as_usize() as u64,
            });
        };
        let mut state = self.state.lock();
        let mut update_irq = false;
        let value = match self.endpoint.kind {
            VirtualPciEndpointKind::Dummy => match offset {
                BAR0_ID_OFFSET => BAR0_ID,
                BAR0_SCRATCH_OFFSET => state.scratch,
                _ => 0,
            },
            VirtualPciEndpointKind::Ivshmem(config) => match offset {
                IVSHMEM_BAR0_ID_OFFSET => config.peer_id,
                IVSHMEM_BAR0_MAX_PEERS_OFFSET => config.peers,
                IVSHMEM_BAR0_INT_CONTROL_OFFSET => state.int_control,
                IVSHMEM_BAR0_INT_STATUS_OFFSET => {
                    let status = state.int_status;
                    state.int_status = 0;
                    update_irq = true;
                    status
                }
                IVSHMEM_BAR0_DOORBELL_OFFSET => state.last_doorbell,
                IVSHMEM_BAR0_STATE_OFFSET => state.ivshmem_state,
                IVSHMEM_BAR0_LINK_STATUS_OFFSET => IVSHMEM_LINK_READY,
                IVSHMEM_BAR0_FEATURE_FLAGS_OFFSET => config.feature_flags,
                _ => 0,
            },
        };
        let irq_asserted =
            update_irq.then(|| ivshmem_interrupt_asserted(&self.endpoint.kind, &state));
        drop(state);
        if let Some(asserted) = irq_asserted {
            self.set_ivshmem_irq_level(asserted)?;
        }
        Ok(width_mask(value as u64, width))
    }

    fn write_bar0(&self, addr: GuestPhysAddr, width: AccessWidth, value: u64) -> DeviceResult {
        let Some(offset) = self.bar0_offset(addr) else {
            return Err(DeviceError::OutOfRange {
                addr: addr.as_usize() as u64,
            });
        };
        let mut doorbell = None;
        let mut state = self.state.lock();
        let mut update_irq = false;
        match self.endpoint.kind {
            VirtualPciEndpointKind::Dummy => {
                if offset == BAR0_SCRATCH_OFFSET {
                    let old = state.scratch as u64;
                    state.scratch = merge_value(old, value, width) as u32;
                }
            }
            VirtualPciEndpointKind::Ivshmem(_) => match offset {
                IVSHMEM_BAR0_INT_CONTROL_OFFSET => {
                    let old = state.int_control as u64;
                    state.int_control = merge_value(old, value, width) as u32;
                    update_irq = true;
                }
                IVSHMEM_BAR0_INT_STATUS_OFFSET => {
                    let clear_mask = width_mask(value, width) as u32;
                    state.int_status &= !clear_mask;
                    update_irq = true;
                }
                IVSHMEM_BAR0_DOORBELL_OFFSET => {
                    let value = merge_value(state.last_doorbell as u64, value, width) as u32;
                    state.last_doorbell = value;
                    doorbell = Some(value);
                }
                IVSHMEM_BAR0_STATE_OFFSET => {
                    let old = state.ivshmem_state as u64;
                    state.ivshmem_state = merge_value(old, value, width) as u32;
                }
                _ => {}
            },
        }
        let irq_asserted =
            update_irq.then(|| ivshmem_interrupt_asserted(&self.endpoint.kind, &state));
        drop(state);

        if let Some(asserted) = irq_asserted {
            self.set_ivshmem_irq_level(asserted)?;
        }
        if let (VirtualPciEndpointKind::Ivshmem(config), Some(value)) =
            (self.endpoint.kind, doorbell)
        {
            pulse_ivshmem_doorbell(config, value)?;
        }
        Ok(())
    }

    fn set_ivshmem_irq_level(&self, asserted: bool) -> DeviceResult {
        if !matches!(self.endpoint.kind, VirtualPciEndpointKind::Ivshmem(_)) {
            return Ok(());
        }
        let Some(irq) = &self.irq else {
            return Ok(());
        };
        let result = if asserted {
            irq.assert()
        } else {
            irq.deassert()
        };
        result.map_err(|error| DeviceError::Unsupported {
            operation: "set ivshmem interrupt level",
            detail: format!("{error}"),
        })
    }
}

impl Device for VirtualPciHost {
    fn name(&self) -> &str {
        &self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn access(
        &self,
        access: &BusAccess,
        _context: &mut dyn DeviceAccess,
    ) -> DeviceResult<BusResponse> {
        if access.kind != BusKind::Mmio {
            return Err(DeviceError::NotFound);
        }

        let addr = GuestPhysAddr::from(access.addr as usize);
        if self.contains(addr) {
            if access.is_read {
                Ok(BusResponse::Read {
                    value: self.read_config(addr, access.width)?,
                })
            } else {
                self.write_config(addr, access.width, access.data)?;
                Ok(BusResponse::Write)
            }
        } else if self.contains_bar0(addr) && access.is_read {
            Ok(BusResponse::Read {
                value: self.read_bar0(addr, access.width)?,
            })
        } else if self.contains_bar0(addr) {
            self.write_bar0(addr, access.width, access.data)?;
            Ok(BusResponse::Write)
        } else {
            Err(DeviceError::NotFound)
        }
    }
}

/// Device graph model for a minimal virtual PCI host bridge.
pub struct VirtualPciHostModel {
    name: String,
    base: GuestPhysAddr,
    size: usize,
    endpoint: VirtualPciEndpointConfig,
    irq: Option<(InterruptControllerId, ControllerInputId)>,
}

impl VirtualPciHostModel {
    /// Creates a virtual PCI host model from a typed endpoint configuration.
    pub fn new(
        name: String,
        base: GuestPhysAddr,
        size: usize,
        mut endpoint: VirtualPciEndpointConfig,
        irq: Option<(InterruptControllerId, ControllerInputId)>,
    ) -> DeviceManagerResult<Self> {
        if let Some((_, input)) = irq {
            endpoint.interrupt_line = checked_u8(input.value(), "virtual PCI interrupt line")?;
            endpoint.interrupt_pin = PCI_INTERRUPT_PIN_INTA;
        }
        Ok(Self {
            name,
            base,
            size,
            endpoint,
            irq,
        })
    }

    /// Creates a dummy virtual PCI host model from the legacy cfg-list layout.
    pub fn from_cfg_list(
        name: String,
        base: GuestPhysAddr,
        size: usize,
        cfg_list: &[usize],
    ) -> DeviceManagerResult<Self> {
        Self::new(
            name,
            base,
            size,
            VirtualPciEndpointConfig::from_cfg_list(cfg_list)?,
            None,
        )
    }

    /// Creates an ivshmem PCI model from the legacy cfg-list layout.
    pub fn ivshmem_from_cfg_list(
        name: String,
        base: GuestPhysAddr,
        size: usize,
        irq: Option<(InterruptControllerId, ControllerInputId)>,
        cfg_list: &[usize],
    ) -> DeviceManagerResult<Self> {
        Self::new(
            name,
            base,
            size,
            VirtualPciEndpointConfig::ivshmem_from_cfg_list(cfg_list)?,
            irq,
        )
    }
}

impl DeviceModel for VirtualPciHostModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        let mut requirements = DeviceRequirements::new()
            .with_mmio(
                ResourceSlot::new(ECAM_SLOT)?,
                self.size as u64,
                PCI_ECAM_FUNCTION_SIZE as u64,
                ResourceRequest::Fixed(self.base.as_usize() as u64),
            )?
            .with_mmio(
                ResourceSlot::new(BAR0_SLOT)?,
                self.endpoint.bar0_size,
                self.endpoint.bar0_size,
                ResourceRequest::Fixed(self.endpoint.bar0_base),
            )?;
        if self.endpoint.bar2_size != 0 {
            requirements = match self.endpoint.kind {
                VirtualPciEndpointKind::Ivshmem(config) => requirements.with_shared_memory(
                    ResourceSlot::new(BAR2_SLOT)?,
                    self.endpoint.bar2_size,
                    self.endpoint.bar2_size,
                    ResourceRequest::Fixed(self.endpoint.bar2_base),
                    SharedMemoryRequest::new(
                        config.link_id as u64,
                        self.endpoint.bar2_host_backing,
                    ),
                )?,
                VirtualPciEndpointKind::Dummy => requirements.with_mmio(
                    ResourceSlot::new(BAR2_SLOT)?,
                    self.endpoint.bar2_size,
                    self.endpoint.bar2_size,
                    ResourceRequest::Fixed(self.endpoint.bar2_base),
                )?,
            };
        }
        if let Some((controller, input)) = self.irq {
            requirements = requirements.with_wired_irq(
                ResourceSlot::new(IRQ_SLOT)?,
                controller,
                InterruptTrigger::LevelTriggered,
                InterruptSharing::Exclusive,
                ResourceRequest::Fixed(input),
            )?;
        }
        Ok(requirements)
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::default()
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let _ = context.mmio(ECAM_SLOT)?;
        let _ = context.mmio(BAR0_SLOT)?;
        if self.endpoint.bar2_size != 0 {
            match self.endpoint.kind {
                VirtualPciEndpointKind::Ivshmem(_) => {
                    let _ = context.shared_memory(BAR2_SLOT)?;
                }
                VirtualPciEndpointKind::Dummy => {
                    let _ = context.mmio(BAR2_SLOT)?;
                }
            }
        }
        let irq = if self.irq.is_some() {
            Some(context.irq(IRQ_SLOT)?)
        } else {
            None
        };
        let device =
            VirtualPciHost::new(self.name.clone(), self.base, self.size, self.endpoint, irq)?;
        let mut bundle = DeviceBundle::new();
        bundle.add_device(Arc::new(device));
        Ok(bundle)
    }
}

fn register_ivshmem_doorbell_endpoint(
    link_id: u32,
    peer_id: u32,
    state: Arc<SpinNoIrq<VirtualPciState>>,
    irq: Option<IrqLine>,
) {
    IVSHMEM_DOORBELL_ENDPOINTS
        .lock()
        .insert((link_id, peer_id), IvshmemDoorbellEndpoint { irq, state });
}

fn pulse_ivshmem_doorbell(config: IvshmemPciConfig, value: u32) -> DeviceResult {
    let target_peer = value >> 16;
    let vector = value & 0xffff;
    if target_peer >= config.peers {
        return Err(DeviceError::InvalidInput {
            operation: "write ivshmem doorbell",
            detail: format!(
                "target peer {target_peer} is outside peer count {}",
                config.peers
            ),
        });
    }

    let target = IVSHMEM_DOORBELL_ENDPOINTS
        .lock()
        .get(&(config.link_id, target_peer))
        .cloned()
        .ok_or_else(|| DeviceError::InvalidInput {
            operation: "write ivshmem doorbell",
            detail: format!(
                "target peer {target_peer} for link {} is not registered",
                config.link_id
            ),
        })?;

    let pulse_irq = {
        let mut state = target.state.lock();
        state.int_status |= IVSHMEM_INT_STATUS_DOORBELL;
        state.last_doorbell = ((config.peer_id & 0xffff) << 16) | (vector & 0xffff);
        ivshmem_interrupt_asserted(&VirtualPciEndpointKind::Ivshmem(config), &state)
    };

    if pulse_irq && let Some(irq) = target.irq {
        irq.assert().map_err(|error| DeviceError::Unsupported {
            operation: "pulse ivshmem doorbell interrupt",
            detail: format!("{error}"),
        })?;
    }

    Ok(())
}

fn ivshmem_interrupt_asserted(kind: &VirtualPciEndpointKind, state: &VirtualPciState) -> bool {
    matches!(kind, VirtualPciEndpointKind::Ivshmem(_))
        && state.int_status & state.int_control != 0
        && state.command & PCI_COMMAND_INTX_DISABLE == 0
}

fn checked_u8(value: usize, field: &'static str) -> DeviceManagerResult<u8> {
    u8::try_from(value).map_err(|_| crate::DeviceManagerError::InvalidInput {
        operation: "build virtual pci host",
        detail: format!("{field} value {value:#x} does not fit in u8"),
    })
}

fn checked_u16(value: usize, field: &'static str) -> DeviceManagerResult<u16> {
    u16::try_from(value).map_err(|_| crate::DeviceManagerError::InvalidInput {
        operation: "build virtual pci host",
        detail: format!("{field} value {value:#x} does not fit in u16"),
    })
}

fn checked_u32(value: usize, field: &'static str) -> DeviceManagerResult<u32> {
    u32::try_from(value).map_err(|_| crate::DeviceManagerError::InvalidInput {
        operation: "build virtual pci host",
        detail: format!("{field} value {value:#x} does not fit in u32"),
    })
}

fn invalid_vpci_config<T>(detail: &'static str) -> DeviceManagerResult<T> {
    Err(crate::DeviceManagerError::InvalidInput {
        operation: "build virtual pci host",
        detail: detail.into(),
    })
}

fn validate_bar0_config(base: u64, size: u64) -> DeviceManagerResult {
    validate_bar_config("BAR0", base, size)
}

fn validate_bar_config(name: &'static str, base: u64, size: u64) -> DeviceManagerResult {
    if base == 0 {
        return Err(crate::DeviceManagerError::InvalidInput {
            operation: "build virtual pci host",
            detail: format!("{name} base must be non-zero"),
        });
    }
    if size == 0 || !size.is_power_of_two() || size < 16 {
        return Err(crate::DeviceManagerError::InvalidInput {
            operation: "build virtual pci host",
            detail: format!("{name} size must be a power of two and at least 16 bytes"),
        });
    }
    if base & (size - 1) != 0 {
        return Err(crate::DeviceManagerError::InvalidInput {
            operation: "build virtual pci host",
            detail: format!("{name} base must be aligned to {name} size"),
        });
    }
    Ok(())
}

fn bar_size_mask(size: u64) -> u32 {
    (!(size as u32 - 1)) & PCI_BAR_MEM32_MASK
}

fn register_access_touches(
    offset: usize,
    width: AccessWidth,
    field: usize,
    field_size: usize,
) -> bool {
    let end = offset.saturating_add(width.size());
    let field_end = field.saturating_add(field_size);
    offset < field_end && field < end
}

fn read_window(
    value: u64,
    offset: usize,
    width: AccessWidth,
    field: usize,
    field_size: usize,
) -> u64 {
    let byte_offset = offset.saturating_sub(field);
    if byte_offset >= field_size {
        return 0;
    }
    width_mask(value >> (byte_offset * 8), width)
}

fn merge_window(
    old: u64,
    value: u64,
    offset: usize,
    width: AccessWidth,
    field: usize,
    field_size: usize,
) -> u64 {
    let byte_offset = offset.saturating_sub(field);
    if byte_offset >= field_size {
        return old;
    }
    let mask = width_mask(u64::MAX, width) << (byte_offset * 8);
    (old & !mask) | ((value << (byte_offset * 8)) & mask)
}

fn merge_value(old: u64, value: u64, width: AccessWidth) -> u64 {
    let mask = width_mask(u64::MAX, width);
    (old & !mask) | (value & mask)
}

fn width_mask(value: u64, width: AccessWidth) -> u64 {
    match width {
        AccessWidth::Byte => value & u8::MAX as u64,
        AccessWidth::Word => value & u16::MAX as u64,
        AccessWidth::Dword => value & u32::MAX as u64,
        AccessWidth::Qword => value,
    }
}

fn read_le_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

#[cfg(test)]
mod tests {
    use axdevice_base::{DeviceId, NoopDeviceAccess};

    use super::*;

    const ECAM_BASE: usize = 0x5000_0000;
    const BAR0_BASE: usize = 0x5010_0000;
    const BAR2_BASE: usize = 0x5020_0000;

    fn test_device() -> VirtualPciHost {
        VirtualPciHost::new(
            "vpci-host".into(),
            GuestPhysAddr::from(ECAM_BASE),
            0x10_0000,
            VirtualPciEndpointConfig {
                bus: 0,
                device: 5,
                function: 0,
                vendor_id: 0xaaaa,
                device_id: 0x0001,
                bar0_base: BAR0_BASE as u64,
                bar0_size: 0x1000,
                bar2_base: 0,
                bar2_size: 0,
                bar2_host_backing: 0,
                interrupt_line: PCI_INTERRUPT_LINE_NONE,
                interrupt_pin: PCI_INTERRUPT_PIN_NONE,
                kind: VirtualPciEndpointKind::Dummy,
            },
            None,
        )
        .unwrap()
    }

    fn test_ivshmem_device() -> VirtualPciHost {
        test_ivshmem_device_with_peer(7, 1, 3)
    }

    fn test_ivshmem_device_with_peer(link_id: u32, peer_id: u32, peers: u32) -> VirtualPciHost {
        VirtualPciHost::new(
            "ivshmem-pci".into(),
            GuestPhysAddr::from(ECAM_BASE),
            0x10_0000,
            VirtualPciEndpointConfig {
                bus: 0,
                device: 5,
                function: 0,
                vendor_id: IVSHMEM_VENDOR_ID,
                device_id: IVSHMEM_DEVICE_ID,
                bar0_base: BAR0_BASE as u64,
                bar0_size: 0x1000,
                bar2_base: BAR2_BASE as u64,
                bar2_size: 0x20_0000,
                bar2_host_backing: 0,
                interrupt_line: 60,
                interrupt_pin: PCI_INTERRUPT_PIN_INTA,
                kind: VirtualPciEndpointKind::Ivshmem(IvshmemPciConfig {
                    link_id,
                    peer_id,
                    peers,
                    feature_flags: 0x5,
                }),
            },
            None,
        )
        .unwrap()
    }

    fn read(device: &VirtualPciHost, addr: usize) -> u64 {
        read_with_width(device, addr, AccessWidth::Dword)
    }

    fn read_with_width(device: &VirtualPciHost, addr: usize, width: AccessWidth) -> u64 {
        let mut access = NoopDeviceAccess::new(DeviceId::new(0));
        match device
            .access(
                &BusAccess {
                    kind: BusKind::Mmio,
                    is_read: true,
                    addr: addr as u64,
                    width,
                    data: 0,
                },
                &mut access,
            )
            .unwrap()
        {
            BusResponse::Read { value } => value,
            BusResponse::Write => panic!("unexpected write response"),
        }
    }

    fn write(device: &VirtualPciHost, addr: usize, value: u64) {
        let mut access = NoopDeviceAccess::new(DeviceId::new(0));
        assert!(matches!(
            device
                .access(
                    &BusAccess {
                        kind: BusKind::Mmio,
                        is_read: false,
                        addr: addr as u64,
                        width: AccessWidth::Dword,
                        data: value,
                    },
                    &mut access,
                )
                .unwrap(),
            BusResponse::Write
        ));
    }

    #[test]
    fn bar0_size_probe_returns_mask() {
        let device = test_device();
        let bar0_config_addr = ECAM_BASE + (5 << 15) + PCI_BAR0_OFFSET;

        assert_eq!(read(&device, bar0_config_addr), BAR0_BASE as u64);
        write(&device, bar0_config_addr, PCI_BAR_SIZE_PROBE as u64);
        assert_eq!(
            read(&device, bar0_config_addr),
            bar_size_mask(DEFAULT_BAR0_SIZE as u64) as u64
        );
        write(&device, bar0_config_addr, BAR0_BASE as u64);
        assert_eq!(read(&device, bar0_config_addr), BAR0_BASE as u64);
    }

    #[test]
    fn ivshmem_bar2_size_probe_returns_mask() {
        let device = test_ivshmem_device();
        let bar2_config_addr = ECAM_BASE + (5 << 15) + PCI_BAR2_OFFSET;

        assert_eq!(read(&device, bar2_config_addr), BAR2_BASE as u64);
        write(&device, bar2_config_addr, PCI_BAR_SIZE_PROBE as u64);
        assert_eq!(
            read(&device, bar2_config_addr),
            bar_size_mask(0x20_0000) as u64
        );
        write(&device, bar2_config_addr, BAR2_BASE as u64);
        assert_eq!(read(&device, bar2_config_addr), BAR2_BASE as u64);
    }

    #[test]
    fn bar0_registers_trap_to_device_model() {
        let device = test_device();

        assert_eq!(read(&device, BAR0_BASE), BAR0_ID as u64);
        write(&device, BAR0_BASE + BAR0_SCRATCH_OFFSET, 0x1357_9bdf);
        assert_eq!(read(&device, BAR0_BASE + BAR0_SCRATCH_OFFSET), 0x1357_9bdf);
    }

    #[test]
    fn ivshmem_bar0_registers_expose_peer_metadata_and_state() {
        let device = test_ivshmem_device();

        assert_eq!(read(&device, BAR0_BASE + IVSHMEM_BAR0_ID_OFFSET), 1);
        assert_eq!(read(&device, BAR0_BASE + IVSHMEM_BAR0_MAX_PEERS_OFFSET), 3);
        assert_eq!(
            read(&device, BAR0_BASE + IVSHMEM_BAR0_LINK_STATUS_OFFSET),
            IVSHMEM_LINK_READY as u64
        );
        assert_eq!(
            read(&device, BAR0_BASE + IVSHMEM_BAR0_FEATURE_FLAGS_OFFSET),
            0x5
        );

        write(&device, BAR0_BASE + IVSHMEM_BAR0_STATE_OFFSET, 0x22);
        assert_eq!(read(&device, BAR0_BASE + IVSHMEM_BAR0_STATE_OFFSET), 0x22);
    }

    #[test]
    fn ivshmem_config_space_exposes_legacy_intx_line_and_pin() {
        let device = test_ivshmem_device();
        let interrupt_line_addr = ECAM_BASE + (5 << 15) + PCI_INTERRUPT_LINE_OFFSET;
        let interrupt_pin_addr = ECAM_BASE + (5 << 15) + PCI_INTERRUPT_PIN_OFFSET;

        assert_eq!(
            read_with_width(&device, interrupt_line_addr, AccessWidth::Byte),
            60
        );
        assert_eq!(
            read_with_width(&device, interrupt_pin_addr, AccessWidth::Byte),
            PCI_INTERRUPT_PIN_INTA as u64
        );
    }

    #[test]
    fn ivshmem_doorbell_sets_target_status_only() {
        let peer0 = test_ivshmem_device_with_peer(11, 0, 3);
        let peer1 = test_ivshmem_device_with_peer(11, 1, 3);
        let peer2 = test_ivshmem_device_with_peer(11, 2, 3);

        write(
            &peer0,
            BAR0_BASE + IVSHMEM_BAR0_DOORBELL_OFFSET,
            (1u64 << 16) | 7,
        );

        assert_eq!(
            read(&peer1, BAR0_BASE + IVSHMEM_BAR0_INT_STATUS_OFFSET),
            IVSHMEM_INT_STATUS_DOORBELL as u64
        );
        assert_eq!(read(&peer1, BAR0_BASE + IVSHMEM_BAR0_INT_STATUS_OFFSET), 0);
        assert_eq!(read(&peer2, BAR0_BASE + IVSHMEM_BAR0_INT_STATUS_OFFSET), 0);
        assert_eq!(read(&peer1, BAR0_BASE + IVSHMEM_BAR0_DOORBELL_OFFSET), 7);
    }

    #[test]
    fn ivshmem_pending_doorbell_sets_pci_status_interrupt_bit() {
        let peer0 = test_ivshmem_device_with_peer(12, 0, 2);
        let peer1 = test_ivshmem_device_with_peer(12, 1, 2);
        let status_addr = ECAM_BASE + (5 << 15) + PCI_STATUS_OFFSET;

        assert_eq!(
            read_with_width(&peer1, status_addr, AccessWidth::Word) & PCI_STATUS_INTERRUPT as u64,
            0
        );
        write(&peer1, BAR0_BASE + IVSHMEM_BAR0_INT_CONTROL_OFFSET, 1);
        write(
            &peer0,
            BAR0_BASE + IVSHMEM_BAR0_DOORBELL_OFFSET,
            (1u64 << 16) | 3,
        );

        assert_eq!(
            read_with_width(&peer1, status_addr, AccessWidth::Word) & PCI_STATUS_INTERRUPT as u64,
            PCI_STATUS_INTERRUPT as u64
        );
        write(&peer1, BAR0_BASE + IVSHMEM_BAR0_INT_STATUS_OFFSET, 1);
        assert_eq!(
            read_with_width(&peer1, status_addr, AccessWidth::Word) & PCI_STATUS_INTERRUPT as u64,
            0
        );
    }

    #[test]
    fn ivshmem_command_dword_read_includes_dynamic_status_interrupt_bit() {
        let peer0 = test_ivshmem_device_with_peer(13, 0, 2);
        let peer1 = test_ivshmem_device_with_peer(13, 1, 2);
        let command_addr = ECAM_BASE + (5 << 15) + PCI_COMMAND_OFFSET;

        write(&peer1, BAR0_BASE + IVSHMEM_BAR0_INT_CONTROL_OFFSET, 1);
        write(
            &peer0,
            BAR0_BASE + IVSHMEM_BAR0_DOORBELL_OFFSET,
            (1u64 << 16) | 3,
        );

        let command_status = read(&peer1, command_addr);
        assert_eq!(
            (command_status >> 16) & PCI_STATUS_INTERRUPT as u64,
            PCI_STATUS_INTERRUPT as u64
        );
    }
}
