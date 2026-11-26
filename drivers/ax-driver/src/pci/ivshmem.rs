//! ivshmem PCI function discovery and endpoint handoff.
//!
//! BAR addresses are handed to the caller unchanged as guest CPU-physical
//! ranges, matching the identity-mapped PCI aperture assumed by
//! [`PciIrqLease`]. Non-identity bus-to-CPU translations are out of scope.

extern crate alloc;

use alloc::{format, string::String};
use core::ops::Range;

use pcie::{Bar, CommandRegister, Endpoint, PciCapability};
use rdrive::{
    DriverGeneric,
    probe::{
        OnProbeError,
        pci::{FnOnProbe, ProbePci},
    },
};

use super::PciIrqLease;

const VENDOR_ID: u16 = 0x1af4;
const DEVICE_ID: u16 = 0x1110;
// Both transports expose the same vendor-specific PCI class. Revision 1 is
// paired only with the existing Normal-NC protocol so old UIO auto-binds do
// not claim the coherent profile; the coherent profile uses revision 2.
const IVSHMEM_CLASS_CODE: u32 = 0xff00_0000;
const NORMAL_NC_CLASS_REVISION: u32 = IVSHMEM_CLASS_CODE | 1;
const NORMAL_WB_CLASS_REVISION: u32 = IVSHMEM_CLASS_CODE | 2;
const AXVISOR_CAPABILITY_ID: u8 = 0x09;
const AXVISOR_CAPABILITY_LENGTH: u8 = 36;
const AXVISOR_CAPABILITY_VERSION: u8 = 1;
const AXVISOR_CAPABILITY_MAGIC: u32 = 0x5649_5841; // "AXIV"
const AXVISOR_TRANSPORT_PROTOCOL_NC: u16 = 0x4000;
const AXVISOR_TRANSPORT_PROTOCOL_WB: u16 = 0x4001;
const AXVISOR_TRANSPORT_FEATURE_PEER_EPOCH: u32 = 1 << 0;
const MAX_PEERS_LIMIT: u32 = 15;
/// BAR2-relative base of the host-owned per-peer reset/detach epoch table.
pub const STATE_EPOCH_OFFSET: u64 = 0x100;
/// Spacing between adjacent peer epoch entries.
pub const STATE_EPOCH_STRIDE: u64 = 8;
const REGISTER_BAR: u8 = 0;
const MSIX_BAR: u8 = 1;
const SHARED_BAR: u8 = 2;
const VECTOR_COUNT: u16 = 1;

crate::model_register!(
    name: "ivshmem PCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe as FnOnProbe,
    }],
);

/// Cache and shareability policy for the BAR2 CPU-owned RAM alias.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IvshmemBar2Policy {
    /// Existing transport policy: Normal Inner Shareable non-cacheable RAM.
    NormalNonCacheable,
    /// Coherent transport policy: Normal Inner Shareable write-back RAM.
    NormalWriteBackInnerShareable,
}

/// One probed ivshmem function and its disabled MSI-X allocation.
pub struct IvshmemPciEndpoint {
    registers: Range<usize>,
    shared: Range<usize>,
    bar2_policy: IvshmemBar2Policy,
    irq: PciIrqLease,
    // Fields drop in declaration order. Tear down MSI-X while memory decode is
    // still enabled, then let the config owner disable the BAR aperture.
    _owner: EndpointOwner,
}

impl IvshmemPciEndpoint {
    /// BAR0 guest CPU-physical range containing the ivshmem registers.
    pub fn registers(&self) -> Range<usize> {
        self.registers.clone()
    }

    /// BAR2 guest CPU-physical range containing the link's shared memory.
    pub fn shared(&self) -> Range<usize> {
        self.shared.clone()
    }

    /// The cache policy selected by the AxVisor transport capability.
    ///
    /// Every CPU alias of [`Self::shared`] must use this policy.
    pub const fn bar2_policy(&self) -> IvshmemBar2Policy {
        self.bar2_policy
    }

    /// The single MSI-X vector owned by this endpoint, initially disabled.
    pub fn irq(&self) -> &PciIrqLease {
        &self.irq
    }
}

/// Takes the unique probed ivshmem endpoint.
///
/// Returns an error when no endpoint was probed, more than one matched, the
/// sole match was unusable, or the endpoint was already taken.
pub fn take_single_endpoint() -> Result<IvshmemPciEndpoint, OnProbeError> {
    if !rdrive::is_initialized() {
        return Err(OnProbeError::other("rdrive is not initialized"));
    }

    let mut devices = rdrive::get_list::<IvshmemRegistration>();
    match devices.len() {
        0 => Err(OnProbeError::other("ivshmem PCI endpoint was not found")),
        1 => {
            let device = devices.pop().ok_or_else(|| {
                OnProbeError::other("ivshmem PCI registration disappeared during handoff")
            })?;
            device
                .lock()
                .map_err(|error| {
                    OnProbeError::other(format!(
                        "failed to lock ivshmem PCI registration: {error:?}"
                    ))
                })?
                .take_endpoint()
        }
        _ => Err(OnProbeError::other(format!(
            "multiple ivshmem PCI endpoints found: {}",
            devices.len()
        ))),
    }
}

struct IvshmemRegistration {
    state: RegistrationState,
}

enum RegistrationState {
    Available(IvshmemPciEndpoint),
    Rejected(String),
    Taken,
    Duplicate,
}

impl IvshmemRegistration {
    fn new(endpoint: IvshmemPciEndpoint) -> Self {
        Self {
            state: RegistrationState::Available(endpoint),
        }
    }

    fn rejected(reason: String) -> Self {
        Self {
            state: RegistrationState::Rejected(reason),
        }
    }

    fn take_endpoint(&mut self) -> Result<IvshmemPciEndpoint, OnProbeError> {
        match core::mem::replace(&mut self.state, RegistrationState::Taken) {
            RegistrationState::Available(endpoint) => Ok(endpoint),
            RegistrationState::Rejected(reason) => {
                let error = OnProbeError::other(reason.clone());
                self.state = RegistrationState::Rejected(reason);
                Err(error)
            }
            RegistrationState::Taken => Err(OnProbeError::other(
                "ivshmem PCI endpoint was already taken",
            )),
            RegistrationState::Duplicate => {
                self.state = RegistrationState::Duplicate;
                Err(OnProbeError::other(
                    "multiple ivshmem PCI endpoints matched the supported profile",
                ))
            }
        }
    }

    fn mark_duplicate(&mut self) {
        self.state = RegistrationState::Duplicate;
    }
}

impl DriverGeneric for IvshmemRegistration {
    fn name(&self) -> &str {
        "ivshmem PCI endpoint"
    }
}

fn probe(mut probe: ProbePci<'_>) -> Result<(), OnProbeError> {
    let endpoint = probe.endpoint();
    if endpoint.vendor_id() != VENDOR_ID || endpoint.device_id() != DEVICE_ID {
        return Err(OnProbeError::NotMatch);
    }

    let class = endpoint.revision_and_class();
    let class_revision = (u32::from(class.base_class) << 24)
        | (u32::from(class.sub_class) << 16)
        | (u32::from(class.interface) << 8)
        | u32::from(class.revision_id);
    let protocol = parse_axvisor_transport_capability(endpoint)?;
    let Some(bar2_policy) = policy_for_profile(class_revision, protocol) else {
        return Err(OnProbeError::other(format!(
            "ivshmem PCI endpoint {} has unsupported class/revision {class_revision:#010x} and \
             AxVisor protocol {:#06x}; expected {NORMAL_NC_CLASS_REVISION:#010x} with \
             {AXVISOR_TRANSPORT_PROTOCOL_NC:#06x}, or {NORMAL_WB_CLASS_REVISION:#010x} with \
             {AXVISOR_TRANSPORT_PROTOCOL_WB:#06x}",
            endpoint.address(),
            protocol
        )));
    };

    if let Some(device) = rdrive::get_one::<IvshmemRegistration>() {
        if let Ok(mut registration) = device.lock() {
            registration.mark_duplicate();
        }
        let reason = "multiple ivshmem PCI endpoints matched the supported profile";
        probe
            .into_platform_device()
            .register(IvshmemRegistration::rejected(reason.into()));
        return Err(OnProbeError::other(reason));
    }

    match configure(&mut probe) {
        Ok((registers, shared, irq)) => {
            let owner = EndpointOwner(probe.take_endpoint());
            probe
                .into_platform_device()
                .register(IvshmemRegistration::new(IvshmemPciEndpoint {
                    registers,
                    shared,
                    bar2_policy,
                    irq,
                    _owner: owner,
                }));
            Ok(())
        }
        Err(error) => {
            let reason = format!("unusable ivshmem PCI endpoint: {error}");
            probe
                .into_platform_device()
                .register(IvshmemRegistration::rejected(reason.clone()));
            Err(OnProbeError::other(reason))
        }
    }
}

fn parse_axvisor_transport_capability(endpoint: &Endpoint) -> Result<u16, OnProbeError> {
    for capability in endpoint.capabilities() {
        let PciCapability::Vendor(address) = capability else {
            continue;
        };
        let offset = address.offset;
        let header = endpoint.read(offset);
        if header as u8 != AXVISOR_CAPABILITY_ID {
            continue;
        }
        let length = (header >> 16) as u8;
        let version = (header >> 24) as u8;
        let malformed = || {
            OnProbeError::other(format!(
                "ivshmem PCI endpoint {} has a malformed AxVisor transport capability at config \
                 offset {offset:#x}",
                endpoint.address()
            ))
        };
        if length != AXVISOR_CAPABILITY_LENGTH || version != AXVISOR_CAPABILITY_VERSION {
            return Err(malformed());
        }
        let magic = endpoint.read(offset + 4);
        let protocol_and_padding = endpoint.read(offset + 8);
        let features = endpoint.read(offset + 12);
        let epoch_offset = endpoint.read(offset + 16);
        let epoch_stride = endpoint.read(offset + 20);
        let common_size = endpoint.read(offset + 24);
        let output_size = endpoint.read(offset + 28);
        let max_peers = endpoint.read(offset + 32);
        if protocol_and_padding >> 16 != 0 {
            return Err(malformed());
        }
        let policy = transport_policy(protocol_and_padding as u16).ok_or_else(|| {
            OnProbeError::other(format!(
                "ivshmem PCI endpoint {} has unknown AxVisor transport protocol {:#06x} at config \
                 offset {offset:#x}",
                endpoint.address(),
                protocol_and_padding as u16
            ))
        })?;
        let peer_geometry_valid = match policy {
            IvshmemBar2Policy::NormalNonCacheable => (1..=MAX_PEERS_LIMIT).contains(&max_peers),
            IvshmemBar2Policy::NormalWriteBackInnerShareable => max_peers == 2 && common_size == 0,
        };
        if magic == AXVISOR_CAPABILITY_MAGIC
            && features & AXVISOR_TRANSPORT_FEATURE_PEER_EPOCH != 0
            && epoch_offset == STATE_EPOCH_OFFSET as u32
            && epoch_stride == STATE_EPOCH_STRIDE as u32
            && common_size.is_multiple_of(0x1000)
            && output_size != 0
            && output_size.is_multiple_of(0x1000)
            && peer_geometry_valid
        {
            return Ok(protocol_and_padding as u16);
        }
        return Err(malformed());
    }
    Err(OnProbeError::other(format!(
        "ivshmem PCI endpoint {} is missing the AxVisor 0x09 transport capability",
        endpoint.address()
    )))
}

const fn transport_policy(protocol: u16) -> Option<IvshmemBar2Policy> {
    match protocol {
        AXVISOR_TRANSPORT_PROTOCOL_NC => Some(IvshmemBar2Policy::NormalNonCacheable),
        AXVISOR_TRANSPORT_PROTOCOL_WB => Some(IvshmemBar2Policy::NormalWriteBackInnerShareable),
        _ => None,
    }
}

const fn policy_for_profile(class_revision: u32, protocol: u16) -> Option<IvshmemBar2Policy> {
    match (class_revision, protocol) {
        (NORMAL_NC_CLASS_REVISION, AXVISOR_TRANSPORT_PROTOCOL_NC) => {
            Some(IvshmemBar2Policy::NormalNonCacheable)
        }
        (NORMAL_WB_CLASS_REVISION, AXVISOR_TRANSPORT_PROTOCOL_WB) => {
            Some(IvshmemBar2Policy::NormalWriteBackInnerShareable)
        }
        _ => None,
    }
}

fn configure(
    probe: &mut ProbePci<'_>,
) -> Result<(Range<usize>, Range<usize>, PciIrqLease), OnProbeError> {
    let endpoint = probe.endpoint();
    let registers = memory_bar(endpoint, REGISTER_BAR, false, "BAR0")?;
    let msix = memory_bar(endpoint, MSIX_BAR, false, "BAR1")?;
    let shared = memory_bar(endpoint, SHARED_BAR, true, "BAR2")?;
    ensure_nonoverlapping_bars(&registers, &msix, &shared)?;

    let table = endpoint.msix_table_info().map_err(|error| {
        OnProbeError::other(format!(
            "ivshmem PCI endpoint {} has no valid MSI-X table: {error}",
            endpoint.address()
        ))
    })?;
    if table.bar != MSIX_BAR || table.pba_bar != MSIX_BAR {
        return Err(OnProbeError::other(format!(
            "ivshmem PCI endpoint {} uses MSI-X table BAR{} and PBA BAR{}, expected BAR{}",
            endpoint.address(),
            table.bar,
            table.pba_bar,
            MSIX_BAR
        )));
    }
    validate_msix_extents(endpoint, &msix, table)?;

    probe.endpoint_mut().update_command(|mut command| {
        command.remove(CommandRegister::IO_ENABLE | CommandRegister::BUS_MASTER_ENABLE);
        command.insert(CommandRegister::MEMORY_ENABLE | CommandRegister::INTERRUPT_DISABLE);
        command
    });

    let info = probe.info();
    let irq = match PciIrqLease::allocate(probe.endpoint_mut(), info, VECTOR_COUNT) {
        Ok(irq) => irq,
        Err(error) => {
            disable_memory_decoding(probe.endpoint_mut());
            return Err(error);
        }
    };

    Ok((registers, shared, irq))
}

struct EndpointOwner(Endpoint);

impl Drop for EndpointOwner {
    fn drop(&mut self) {
        disable_memory_decoding(&mut self.0);
    }
}

fn disable_memory_decoding(endpoint: &mut Endpoint) {
    endpoint.update_command(|mut command| {
        command.remove(CommandRegister::MEMORY_ENABLE | CommandRegister::BUS_MASTER_ENABLE);
        command
    });
}

fn ensure_nonoverlapping_bars(
    registers: &Range<usize>,
    msix: &Range<usize>,
    shared: &Range<usize>,
) -> Result<(), OnProbeError> {
    let bars = [("BAR0", registers), ("BAR1", msix), ("BAR2", shared)];
    for (index, (name, range)) in bars.iter().copied().enumerate() {
        for (other_name, other) in bars[index + 1..].iter().copied() {
            if ranges_overlap(range, other) {
                return Err(OnProbeError::other(format!(
                    "ivshmem {name} overlaps {other_name} in the PCI BAR aperture"
                )));
            }
        }
    }
    Ok(())
}

fn validate_msix_extents(
    endpoint: &Endpoint,
    msix_bar: &Range<usize>,
    table: pcie::MsixTableInfo,
) -> Result<(), OnProbeError> {
    if table.entries == 0 {
        return Err(OnProbeError::other("ivshmem MSI-X table has no entries"));
    }

    let table_range = table.table_range(msix_bar.clone()).map_err(|error| {
        OnProbeError::other(format!(
            "ivshmem PCI endpoint {} MSI-X table is outside BAR{}: {error}",
            endpoint.address(),
            MSIX_BAR
        ))
    })?;
    let pba_len = usize::from(table.entries).div_ceil(8);
    let pba_start = msix_bar
        .start
        .checked_add(table.pba_offset)
        .ok_or_else(|| OnProbeError::other("ivshmem MSI-X PBA offset overflows usize"))?;
    let pba_end = pba_start
        .checked_add(pba_len)
        .ok_or_else(|| OnProbeError::other("ivshmem MSI-X PBA range overflows usize"))?;
    if pba_end > msix_bar.end {
        return Err(OnProbeError::other(format!(
            "ivshmem PCI endpoint {} MSI-X PBA is outside BAR{}",
            endpoint.address(),
            MSIX_BAR
        )));
    }
    let pba = pba_start..pba_end;
    if ranges_overlap(&table_range, &pba) {
        return Err(OnProbeError::other(format!(
            "ivshmem PCI endpoint {} MSI-X table overlaps its PBA",
            endpoint.address()
        )));
    }
    Ok(())
}

fn ranges_overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

fn memory_bar(
    endpoint: &Endpoint,
    index: u8,
    prefetchable: bool,
    name: &str,
) -> Result<Range<usize>, OnProbeError> {
    let bar = endpoint
        .bar(index)
        .ok_or_else(|| OnProbeError::other(format!("ivshmem {name} is absent")))?;
    let (address, size, actual_prefetchable) = match bar {
        Bar::Memory32 {
            address,
            size,
            prefetchable,
        } => (u64::from(address), u64::from(size), prefetchable),
        Bar::Memory64 {
            address,
            size,
            prefetchable,
        } => (address, size, prefetchable),
        Bar::Io { .. } => {
            return Err(OnProbeError::other(format!(
                "ivshmem {name} is not a memory BAR"
            )));
        }
    };
    if actual_prefetchable != prefetchable {
        return Err(OnProbeError::other(format!(
            "ivshmem {name} prefetchable attribute does not match the profile"
        )));
    }

    let start = usize::try_from(address)
        .map_err(|_| OnProbeError::other(format!("ivshmem {name} address exceeds usize")))?;
    let size = usize::try_from(size)
        .map_err(|_| OnProbeError::other(format!("ivshmem {name} size exceeds usize")))?;
    if start == 0 || size == 0 || !start.is_multiple_of(size) {
        return Err(OnProbeError::other(format!(
            "ivshmem {name} is not an assigned, naturally aligned memory BAR"
        )));
    }
    let end = start
        .checked_add(size)
        .ok_or_else(|| OnProbeError::other(format!("ivshmem {name} range overflows usize")))?;
    Ok(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_policy_requires_the_exact_class_revision_protocol_pair() {
        assert_eq!(
            policy_for_profile(NORMAL_NC_CLASS_REVISION, AXVISOR_TRANSPORT_PROTOCOL_NC),
            Some(IvshmemBar2Policy::NormalNonCacheable)
        );
        assert_eq!(
            policy_for_profile(NORMAL_WB_CLASS_REVISION, AXVISOR_TRANSPORT_PROTOCOL_WB),
            Some(IvshmemBar2Policy::NormalWriteBackInnerShareable)
        );
        assert_eq!(
            policy_for_profile(NORMAL_NC_CLASS_REVISION, AXVISOR_TRANSPORT_PROTOCOL_WB),
            None
        );
        assert_eq!(
            policy_for_profile(NORMAL_WB_CLASS_REVISION, AXVISOR_TRANSPORT_PROTOCOL_NC),
            None
        );
        assert_eq!(
            policy_for_profile(IVSHMEM_CLASS_CODE, AXVISOR_TRANSPORT_PROTOCOL_NC),
            None
        );
        assert_eq!(
            policy_for_profile(
                IVSHMEM_CLASS_CODE | 0x80_0000,
                AXVISOR_TRANSPORT_PROTOCOL_WB
            ),
            None
        );
    }
}
