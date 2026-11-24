use core::{
    fmt,
    marker::PhantomData,
    mem::size_of,
    ptr::NonNull,
    sync::atomic::{self, AtomicU64, Ordering},
};

use ax_std::os::arceos::modules::ax_hal::{self, mem::PhysAddr};

const ECAM_SIZE: usize = 0x10_0000;
const PCI_DEVICE_COUNT: u8 = 32;
const PCI_FUNCTION_STRIDE: usize = 1 << 15;
const PCI_ID_OFFSET: usize = 0x00;
const PCI_COMMAND_OFFSET: usize = 0x04;
const PCI_STATUS_OFFSET: usize = 0x06;
const PCI_CLASS_REVISION_OFFSET: usize = 0x08;
const PCI_BAR0_OFFSET: usize = 0x10;
const PCI_BAR2_OFFSET: usize = 0x18;
const PCI_CAPABILITY_POINTER_OFFSET: usize = 0x34;
const PCI_COMMAND_MEMORY_ENABLE: u16 = 1 << 1;
const PCI_STATUS_CAPABILITY_LIST: u16 = 1 << 4;
const PCI_CAPABILITY_ID_VENDOR: u8 = 0x09;
const PCI_CAPABILITY_LIST_LIMIT: usize = 64;
const PCI_CONFIG_SPACE_SIZE: usize = 0x100;
const PCI_BAR_IO_SPACE: u32 = 1 << 0;
const PCI_BAR_TYPE_MASK: u32 = 0b110;
const PCI_BAR_ADDRESS_MASK: u32 = 0xffff_fff0;

/// Event Status bit asserted for the profile's supported MSI-X vector.
pub const EVENT_VECTOR_0: u32 = 1 << 0;

const BAR0_SIZE: usize = 0x1000;
const BAR2_SIZE: usize = 0x1_0000;
const STATE_TABLE_SIZE: usize = 0x1000;
const REGISTER_PEER_ID: usize = 0x00;
const REGISTER_MAX_PEERS: usize = 0x04;
const REGISTER_DOORBELL: usize = 0x0c;
const REGISTER_STATE: usize = 0x10;
const REGISTER_EVENT_STATUS: usize = 0x14;
const REGISTER_COMMON_SIZE: usize = 0x18;
const REGISTER_OUTPUT_SIZE: usize = 0x1c;
const OUTPUT_SECTION_BASE: usize = STATE_TABLE_SIZE;
const PEER_EPOCH_OFFSET: usize = 0x100;
const PEER_EPOCH_STRIDE: usize = 8;
const MAX_PROFILE_PEERS: u32 = 15;
const BASELINE_OUTPUT_SIZE: usize = 0x7000;
const THREE_PEER_OUTPUT_SIZE: usize = 0x5000;
const BASELINE_MAX_PEERS: u32 = 2;
const THREE_PEER_MAX_PEERS: u32 = 3;
const IVSHMEM_VENDOR_DEVICE: u32 = 0x1110_1af4;
// Revision 1 is required by upstream uio_ivshmem; class 0xff0000 and the AXIV
// capability distinguish AxVisor from classic QEMU functions.
const IVSHMEM_CLASS_REVISION: u32 = 0xff00_0001;
const AXVISOR_CAPABILITY_LENGTH: u8 = 36;
const AXVISOR_CAPABILITY_VERSION: u8 = 1;
const AXVISOR_CAPABILITY_MAGIC: u32 = 0x5649_5841; // "AXIV"
const AXVISOR_TRANSPORT_PROTOCOL: u16 = 0x4000;
const AXVISOR_TRANSPORT_FEATURE_PEER_EPOCH: u32 = 1 << 0;
const AXVISOR_TRANSPORT_CAPABILITY_BODY_SIZE: usize = 34;

/// BAR2 layout advertised by one AxVisor ivshmem link profile.
///
/// The first page is the read-only state table, followed by the optional
/// common section and one page-aligned output section per peer. Discovery
/// validates all three sizes against this explicit layout and BAR2 before the
/// device can expose a section.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IvshmemLayout {
    max_peers: u32,
    common_size: usize,
    output_size: usize,
    bar2_size: usize,
}

impl IvshmemLayout {
    /// Layout used by the single-peer compatibility smoke.
    pub const fn single_peer() -> Self {
        Self {
            max_peers: 1,
            common_size: 0,
            output_size: BASELINE_OUTPUT_SIZE,
            bar2_size: BAR2_SIZE,
        }
    }

    /// Two-peer baseline with 28 KiB output sections.
    pub const fn two_peer() -> Self {
        Self {
            max_peers: BASELINE_MAX_PEERS,
            common_size: 0,
            output_size: BASELINE_OUTPUT_SIZE,
            bar2_size: BAR2_SIZE,
        }
    }

    /// Three-peer suite profile with 20 KiB output sections.
    pub const fn three_peer() -> Self {
        Self {
            max_peers: THREE_PEER_MAX_PEERS,
            common_size: 0,
            output_size: THREE_PEER_OUTPUT_SIZE,
            bar2_size: BAR2_SIZE,
        }
    }

    /// Start offset of one peer's output section.
    pub fn output_offset(self, peer: u32) -> Result<usize, IvshmemError> {
        self.check_peer(peer)?;
        let peer = usize::try_from(peer).map_err(|_| IvshmemError::AccessOutOfRange {
            offset: self.bar2_size,
            width: self.output_size,
            region_size: self.bar2_size,
        })?;
        // Constructors and discovery keep the complete geometry within BAR2,
        // so the peer range check above makes this addition bounded.
        Ok(OUTPUT_SECTION_BASE + self.common_size + peer * self.output_size)
    }

    fn check_peer(self, peer: u32) -> Result<(), IvshmemError> {
        if peer >= self.max_peers {
            return Err(IvshmemError::PeerOutOfRange {
                peer,
                max_peers: self.max_peers,
            });
        }
        Ok(())
    }

    /// Replaces the expected geometry with the validated values advertised by
    /// BAR0. The configured profile remains an explicit compatibility
    /// requirement rather than a source of guessed section boundaries.
    fn with_advertised_geometry(
        self,
        common_size: u32,
        output_size: u32,
    ) -> Result<Self, IvshmemError> {
        let advertised_common_size = common_size;
        let advertised_output_size = output_size;
        let invalid = || IvshmemError::InvalidGeometry {
            common_size: advertised_common_size,
            output_size: advertised_output_size,
        };
        let common_size =
            usize::try_from(common_size).map_err(|_| IvshmemError::InvalidGeometry {
                common_size,
                output_size,
            })?;
        let output_size = usize::try_from(output_size).map_err(|_| invalid())?;
        if output_size == 0
            || !common_size.is_multiple_of(STATE_TABLE_SIZE)
            || !output_size.is_multiple_of(STATE_TABLE_SIZE)
            || common_size != self.common_size
            || output_size != self.output_size
        {
            return Err(invalid());
        }
        let max_peers = usize::try_from(self.max_peers).map_err(|_| invalid())?;
        let section_bytes = max_peers.checked_mul(output_size).ok_or_else(invalid)?;
        let end = STATE_TABLE_SIZE
            .checked_add(common_size)
            .and_then(|offset| offset.checked_add(section_bytes))
            .ok_or_else(invalid)?;
        if end > self.bar2_size {
            return Err(invalid());
        }
        Ok(Self {
            common_size,
            output_size,
            ..self
        })
    }
}

/// Revisions and PCI function identity used for the ivshmem register model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IvshmemIdentity {
    device: u8,
}

impl fmt::Display for IvshmemIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "0000:00:{:02x}.0 {:04x}:{:04x} rev={:02x} class={:06x}",
            self.device,
            IVSHMEM_VENDOR_DEVICE as u16,
            (IVSHMEM_VENDOR_DEVICE >> 16) as u16,
            IVSHMEM_CLASS_REVISION as u8,
            IVSHMEM_CLASS_REVISION >> 8,
        )
    }
}

/// One discovered ivshmem function and the mappings owned by it.
///
/// Construction maps ECAM and BAR0 as Device memory and BAR2 as Normal
/// non-cacheable memory through `ax_mm::iomap_uncached`. The object owns the
/// local mapping handles. Section and output borrows cannot outlive it.
///
/// ArceOS does not currently expose an owner-aware `iounmap` for mappings that
/// may overlap a platform direct map. Dropping this object therefore revokes
/// local Rust access but does not tear down the underlying kernel mapping.
pub struct IvshmemDevice {
    _ecam: Mapping,
    registers: Mapping,
    shared: Mapping,
    identity: IvshmemIdentity,
    bar2_bus_address: u32,
    layout: IvshmemLayout,
    peer_id: u32,
    max_peers: u32,
}

impl IvshmemDevice {
    /// Discovers the unique ivshmem function on PCI bus zero and maps its
    /// register and shared-memory BARs.
    ///
    /// The FDT must describe exactly one `pci-host-ecam-generic` host with one
    /// aligned bus-zero ECAM window and one non-prefetchable memory32 window.
    /// BAR0 and BAR2 must be assigned, naturally aligned, and contained in
    /// that window. Discovery temporarily disables memory decode to probe each
    /// BAR size, restores the original config dwords, and then enables decode.
    /// The advertised peer count, common size, and output size must all match
    /// `layout`; a zero output size means geometry is unavailable.
    pub fn discover(layout: IvshmemLayout) -> Result<Self, IvshmemError> {
        let host = configured_pci_host()?;
        let ecam = Mapping::device(host.ecam_base, ECAM_SIZE, "ECAM")?;
        let mut function = None;
        for device in 0..PCI_DEVICE_COUNT {
            let offset = usize::from(device) * PCI_FUNCTION_STRIDE;
            if ecam.read_u32(offset + PCI_ID_OFFSET)? == IVSHMEM_VENDOR_DEVICE
                && function.replace(device).is_some()
            {
                return Err(IvshmemError::AmbiguousEndpoint);
            }
        }
        let device = function.ok_or(IvshmemError::EndpointNotFound)?;
        let function_offset = usize::from(device) * PCI_FUNCTION_STRIDE;
        let class_revision = ecam.read_u32(function_offset + PCI_CLASS_REVISION_OFFSET)?;
        if class_revision != IVSHMEM_CLASS_REVISION {
            return Err(IvshmemError::ProfileMismatch {
                expected: IVSHMEM_CLASS_REVISION,
                actual: class_revision,
            });
        }
        if !has_axvisor_transport_capability(&ecam, function_offset)? {
            return Err(IvshmemError::CapabilityMismatch);
        }

        let (bar0_bus_address, bar0_size) =
            probe_memory_bar_size(&ecam, function_offset, PCI_BAR0_OFFSET, 0)?;
        let (bar2_bus_address, bar2_size) =
            probe_memory_bar_size(&ecam, function_offset, PCI_BAR2_OFFSET, 2)?;
        expect_bar_size(0, BAR0_SIZE as u64, bar0_size)?;
        expect_bar_size(2, layout.bar2_size as u64, bar2_size)?;
        let bar0_address = host.bar_cpu_address(bar0_bus_address, BAR0_SIZE, 0)?;
        let bar2_address = host.bar_cpu_address(bar2_bus_address, layout.bar2_size, 2)?;

        let command = ecam.read_u16(function_offset + PCI_COMMAND_OFFSET)?;
        ecam.write_u16(
            function_offset + PCI_COMMAND_OFFSET,
            command | PCI_COMMAND_MEMORY_ENABLE,
        )?;

        let registers = Mapping::device(bar0_address, BAR0_SIZE, "ivshmem BAR0")?;
        let peer_id = registers.read_u32(REGISTER_PEER_ID)?;
        let max_peers = registers.read_u32(REGISTER_MAX_PEERS)?;
        let common_size = registers.read_u32(REGISTER_COMMON_SIZE)?;
        let output_size = registers.read_u32(REGISTER_OUTPUT_SIZE)?;
        if max_peers != layout.max_peers {
            return Err(IvshmemError::PeerCountMismatch {
                advertised: max_peers,
                configured: layout.max_peers,
            });
        }
        let layout = layout.with_advertised_geometry(common_size, output_size)?;
        if peer_id >= max_peers {
            return Err(IvshmemError::PeerOutOfRange {
                peer: peer_id,
                max_peers,
            });
        }

        // BAR2 is CPU-owned Normal memory, not a Device aperture. Linux maps
        // the prefetchable BAR through resource2_wc, which is Normal
        // non-cacheable on AArch64; use the corresponding ArceOS mapping.
        let shared = Mapping::uncached(bar2_address, layout.bar2_size, "ivshmem BAR2")?;
        Ok(Self {
            _ecam: ecam,
            registers,
            shared,
            identity: IvshmemIdentity { device },
            bar2_bus_address,
            layout,
            peer_id,
            max_peers,
        })
    }

    /// Verified PCI function identity.
    pub const fn identity(&self) -> IvshmemIdentity {
        self.identity
    }

    /// This guest's peer slot.
    pub const fn peer_id(&self) -> u32 {
        self.peer_id
    }

    /// Peer count advertised by BAR0 and the configured layout.
    pub const fn max_peers(&self) -> u32 {
        self.max_peers
    }

    /// Configured BAR2 layout.
    pub const fn layout(&self) -> IvshmemLayout {
        self.layout
    }

    /// Assigned BAR2 address in PCI memory space.
    pub const fn bar2_bus_address(&self) -> u32 {
        self.bar2_bus_address
    }

    /// Reads the host-published state-table entry for `peer`.
    pub fn state(&self, peer: u32) -> Result<u32, IvshmemError> {
        self.layout.check_peer(peer)?;
        self.shared.read_u32(peer as usize * size_of::<u32>())
    }

    /// Observes one host-owned reset/detach epoch with acquire ordering.
    ///
    /// The epoch is in the read-only state page at
    /// `0x100 + peer_id * 8`. A caller must retain the previous nonzero value
    /// and discard stale Message V1 headers or queued data when it changes.
    pub fn peer_epoch(&self, peer: u32) -> Result<u64, IvshmemError> {
        self.layout.check_peer(peer)?;
        let offset = PEER_EPOCH_OFFSET
            .checked_add(peer as usize * PEER_EPOCH_STRIDE)
            .ok_or(IvshmemError::AccessOutOfRange {
                offset: PEER_EPOCH_OFFSET,
                width: PEER_EPOCH_STRIDE,
                region_size: STATE_TABLE_SIZE,
            })?;
        let pointer = self
            .shared
            .subrange(offset, size_of::<u64>())?
            .as_ptr()
            .cast::<u64>();
        // SAFETY: the state page is 4 KiB aligned, the ABI offset is 8-byte
        // aligned, subrange checked the eight-byte extent, and the mapping
        // outlives this load. Guests are read-only observers; the host owns
        // publication of the aligned epoch word.
        Ok(u64::from_le(unsafe {
            AtomicU64::from_ptr(pointer).load(Ordering::Acquire)
        }))
    }

    /// Publishes this peer's value through BAR0 State.
    ///
    /// The host copies the value into this peer's readable BAR2 state entry.
    pub fn publish_state(&self, value: u32) -> Result<(), IvshmemError> {
        self.registers.write_u32(REGISTER_STATE, value)
    }

    /// Rings `target`'s doorbell with a 16-bit vector.
    ///
    /// This performs a process-wide release fence before the Device register
    /// write, ordering prior BAR2 publications before the doorbell. A vector
    /// outside the device profile is accepted and is a hardware-defined no-op.
    pub fn ring_doorbell(&self, target: u32, vector: u16) -> Result<(), IvshmemError> {
        self.layout.check_peer(target)?;
        atomic::fence(Ordering::SeqCst);
        self.registers
            .write_u32(REGISTER_DOORBELL, (target << 16) | u32::from(vector))
    }

    /// Returns the raw BAR0 Event Status bitmap without clearing it.
    pub fn event_status(&self) -> Result<u32, IvshmemError> {
        self.registers.read_u32(REGISTER_EVENT_STATUS)
    }

    /// Clears every Event Status bit set in `events` with write-one-to-clear.
    pub fn acknowledge_events(&self, events: u32) -> Result<(), IvshmemError> {
        self.registers.write_u32(REGISTER_EVENT_STATUS, events)
    }

    /// Reads and clears the requested Event Status bits.
    ///
    /// This snapshots the masked bits and writes exactly that snapshot back as
    /// W1C. Callers that need an ISR-side acknowledgement instead can split
    /// [`Self::event_status`] and [`Self::acknowledge_events`] explicitly.
    pub fn take_events(&self, mask: u32) -> Result<u32, IvshmemError> {
        let pending = self.event_status()? & mask;
        if pending != 0 {
            self.acknowledge_events(pending)?;
        }
        Ok(pending)
    }

    /// Returns this peer's writable BAR2 output section.
    pub fn own_output(&self) -> Result<OutputSection<'_>, IvshmemError> {
        self.output(self.peer_id)
    }

    /// Returns one peer's BAR2 output section.
    ///
    /// Read operations work for every in-profile output. Write operations fail
    /// unless `peer` is this guest's own peer slot.
    /// Remote callers must use [`OutputSection::as_const_ptr`]; the mutable
    /// pointer is reserved for the owner selected by [`Self::own_output`].
    pub fn output(&self, peer: u32) -> Result<OutputSection<'_>, IvshmemError> {
        let offset = self.layout.output_offset(peer)?;
        let size = self.layout.output_size;
        Ok(OutputSection {
            base: self.shared.subrange(offset, size)?,
            owner: self.peer_id,
            peer,
            size,
            _mapping: PhantomData,
        })
    }
}

/// Borrowed view of one peer's BAR2 output section.
///
/// All offsets are relative to the start of the peer's own section. The view
/// cannot outlive its [`IvshmemDevice`]. The raw range is intended for a
/// consumer such as `axivc::IvshmemMessagePort::attach`.
///
/// Shared bytes may be accessed concurrently by the device and peer address
/// spaces. Callers must not create ordinary references or exclusive borrows
/// over the range. Use volatile accesses for testing and the aligned atomic
/// protocol operations owned by the message layer for runtime communication.
/// Raw writes are permitted only when [`Self::is_owned_by_self`] is true and
/// the caller is the section's unique writer. A remote peer's range must be
/// consumed through [`Self::as_const_ptr`] and read as `*const u8`.
pub struct OutputSection<'a> {
    base: NonNull<u8>,
    owner: u32,
    peer: u32,
    size: usize,
    _mapping: PhantomData<&'a Mapping>,
}

impl OutputSection<'_> {
    /// Whether local writes are permitted by the BAR2 ownership policy.
    pub const fn is_owned_by_self(&self) -> bool {
        self.peer == self.owner
    }

    /// Size of this output section in bytes.
    pub const fn len(&self) -> usize {
        self.size
    }

    /// Returns whether the output section has no bytes.
    pub const fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// Returns a mutable raw pointer to this section for the owning guest.
    ///
    /// The pointer remains valid for [`Self::len`] bytes while the owning
    /// device and this section borrow remain alive. Writes are valid only when
    /// [`Self::is_owned_by_self`] is true; readers of a remote section must use
    /// [`Self::as_const_ptr`].
    pub fn as_ptr(&self) -> *mut u8 {
        self.base.as_ptr()
    }

    /// Returns a read-only raw pointer to this section for any peer.
    pub fn as_const_ptr(&self) -> *const u8 {
        self.base.as_ptr()
    }

    /// Reads one aligned 32-bit word from this section.
    pub fn read_u32(&self, offset: usize) -> Result<u32, IvshmemError> {
        self.check_scalar(offset, size_of::<u32>())?;
        // SAFETY: the range and natural alignment were checked, and the
        // section borrow keeps the mapping alive.
        Ok(unsafe { core::ptr::read_volatile(self.base.as_ptr().add(offset).cast::<u32>()) })
    }

    /// Writes one aligned 32-bit word to this guest's own section.
    pub fn write_u32(&self, offset: usize, value: u32) -> Result<(), IvshmemError> {
        self.check_write_scalar(offset, size_of::<u32>())?;
        // SAFETY: ownership, range, and natural alignment were checked.
        unsafe {
            core::ptr::write_volatile(self.base.as_ptr().add(offset).cast::<u32>(), value);
        }
        Ok(())
    }

    /// Reads one aligned 64-bit word from this section.
    pub fn read_u64(&self, offset: usize) -> Result<u64, IvshmemError> {
        self.check_scalar(offset, size_of::<u64>())?;
        // SAFETY: the range and natural alignment were checked, and the
        // section borrow keeps the mapping alive.
        Ok(unsafe { core::ptr::read_volatile(self.base.as_ptr().add(offset).cast::<u64>()) })
    }

    /// Writes one aligned 64-bit word to this guest's own section.
    pub fn write_u64(&self, offset: usize, value: u64) -> Result<(), IvshmemError> {
        self.check_write_scalar(offset, size_of::<u64>())?;
        // SAFETY: ownership, range, and natural alignment were checked.
        unsafe {
            core::ptr::write_volatile(self.base.as_ptr().add(offset).cast::<u64>(), value);
        }
        Ok(())
    }

    /// Copies bytes out of this section.
    pub fn read_bytes(&self, offset: usize, output: &mut [u8]) -> Result<(), IvshmemError> {
        self.check_bounds(offset, output.len())?;
        for (index, byte) in output.iter_mut().enumerate() {
            // SAFETY: the complete destination range was checked once.
            *byte = unsafe { core::ptr::read_volatile(self.base.as_ptr().add(offset + index)) };
        }
        Ok(())
    }

    /// Copies bytes into this guest's own section.
    pub fn write_bytes(&self, offset: usize, input: &[u8]) -> Result<(), IvshmemError> {
        self.check_write_bounds(offset, input.len())?;
        for (index, byte) in input.iter().copied().enumerate() {
            // SAFETY: ownership and the complete destination range were
            // checked once.
            unsafe {
                core::ptr::write_volatile(self.base.as_ptr().add(offset + index), byte);
            }
        }
        Ok(())
    }

    fn check_write_bounds(&self, offset: usize, width: usize) -> Result<(), IvshmemError> {
        if !self.is_owned_by_self() {
            return Err(IvshmemError::NotOutputOwner { peer: self.peer });
        }
        self.check_bounds(offset, width)
    }

    fn check_write_scalar(&self, offset: usize, width: usize) -> Result<(), IvshmemError> {
        self.check_write_bounds(offset, width)?;
        self.check_alignment(offset, width)
    }

    fn check_scalar(&self, offset: usize, width: usize) -> Result<(), IvshmemError> {
        self.check_bounds(offset, width)?;
        self.check_alignment(offset, width)
    }

    fn check_bounds(&self, offset: usize, width: usize) -> Result<(), IvshmemError> {
        let end = offset
            .checked_add(width)
            .ok_or(IvshmemError::AccessOutOfRange {
                offset,
                width,
                region_size: self.size,
            })?;
        if end > self.size {
            return Err(IvshmemError::AccessOutOfRange {
                offset,
                width,
                region_size: self.size,
            });
        }
        Ok(())
    }

    fn check_alignment(&self, offset: usize, width: usize) -> Result<(), IvshmemError> {
        let address = (self.base.as_ptr() as usize)
            .checked_add(offset)
            .ok_or(IvshmemError::UnalignedAccess { offset, width })?;
        if width != 0 && !address.is_multiple_of(width) {
            return Err(IvshmemError::UnalignedAccess { offset, width });
        }
        Ok(())
    }
}

/// Errors reported while discovering or accessing the ivshmem endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IvshmemError {
    /// The boot FDT is unavailable.
    NoBootFdt,
    /// More than one generic ECAM host exists.
    MultiplePciHosts,
    /// The generic ECAM host does not expose the required single window shape.
    InvalidPciHost,
    /// No ivshmem function exists on bus zero.
    EndpointNotFound,
    /// More than one ivshmem function exists on bus zero.
    AmbiguousEndpoint,
    /// The function identity or register revision does not match.
    ProfileMismatch { expected: u32, actual: u32 },
    /// The function lacks a valid AxVisor 0x09 transport capability.
    CapabilityMismatch,
    /// A BAR is not an assigned memory32 BAR.
    InvalidMemoryBar { bar: u8 },
    /// A BAR is not naturally aligned for the requested size.
    MisalignedBar { bar: u8 },
    /// A BAR lies outside the FDT PCI memory window.
    BarOutsideWindow { bar: u8 },
    /// A BAR's advertised aperture differs from the required profile.
    BarSizeMismatch { bar: u8, expected: u64, actual: u64 },
    /// BAR0 advertises a different peer count from the caller's profile.
    PeerCountMismatch { advertised: u32, configured: u32 },
    /// BAR0 geometry is unavailable or incompatible with the explicit layout.
    InvalidGeometry { common_size: u32, output_size: u32 },
    /// A peer slot is outside the configured profile.
    PeerOutOfRange { peer: u32, max_peers: u32 },
    /// Mapping a physical aperture failed.
    MapFailed { region: &'static str },
    /// An access leaves its containing aperture or section.
    AccessOutOfRange {
        offset: usize,
        width: usize,
        region_size: usize,
    },
    /// A typed register or word access is not naturally aligned.
    UnalignedAccess { offset: usize, width: usize },
    /// A write targets another peer's output section.
    NotOutputOwner { peer: u32 },
}

impl fmt::Display for IvshmemError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoBootFdt => formatter.write_str("boot FDT is unavailable"),
            Self::MultiplePciHosts => formatter.write_str("boot FDT describes multiple PCI hosts"),
            Self::InvalidPciHost => formatter.write_str(
                "PCI host must expose one aligned bus-zero ECAM and one non-prefetchable Memory32 \
                 window",
            ),
            Self::EndpointNotFound => formatter.write_str("ivshmem endpoint not found on bus 0"),
            Self::AmbiguousEndpoint => formatter.write_str("multiple ivshmem endpoints found"),
            Self::ProfileMismatch { expected, actual } => write!(
                formatter,
                "ivshmem class/revision mismatch: expected {expected:#010x}, got {actual:#010x}"
            ),
            Self::CapabilityMismatch => {
                formatter.write_str("ivshmem AxVisor transport capability is missing or invalid")
            }
            Self::InvalidMemoryBar { bar } => {
                write!(formatter, "PCI BAR{bar} is not an assigned memory32 BAR")
            }
            Self::MisalignedBar { bar } => write!(formatter, "PCI BAR{bar} is not aligned"),
            Self::BarOutsideWindow { bar } => {
                write!(formatter, "PCI BAR{bar} exceeds the PCI memory window")
            }
            Self::BarSizeMismatch {
                bar,
                expected,
                actual,
            } => write!(
                formatter,
                "PCI BAR{bar} size is {actual:#x}, expected {expected:#x}"
            ),
            Self::PeerCountMismatch {
                advertised,
                configured,
            } => write!(
                formatter,
                "ivshmem max_peers={advertised}, configured profile expects {configured}"
            ),
            Self::InvalidGeometry {
                common_size,
                output_size,
            } => write!(
                formatter,
                "ivshmem geometry is unavailable or incompatible: common_size={common_size:#x} \
                 output_size={output_size:#x}"
            ),
            Self::PeerOutOfRange { peer, max_peers } => {
                write!(formatter, "ivshmem peer {peer} is outside 0..{max_peers}")
            }
            Self::MapFailed { region } => write!(formatter, "failed to map {region}"),
            Self::AccessOutOfRange {
                offset,
                width,
                region_size,
            } => write!(
                formatter,
                "access {offset:#x}..{:#x} leaves a {region_size:#x}-byte region",
                offset.saturating_add(*width)
            ),
            Self::UnalignedAccess { offset, width } => {
                write!(formatter, "{width}-byte access at {offset:#x} is unaligned")
            }
            Self::NotOutputOwner { peer } => {
                write!(formatter, "peer {peer} output is read-only")
            }
        }
    }
}

impl core::error::Error for IvshmemError {}

struct Mapping {
    ptr: NonNull<u8>,
    size: usize,
}

impl Mapping {
    fn device(base: usize, size: usize, region: &'static str) -> Result<Self, IvshmemError> {
        Self::from_virt(ax_mm::iomap(PhysAddr::from_usize(base), size), size, region)
    }

    fn uncached(base: usize, size: usize, region: &'static str) -> Result<Self, IvshmemError> {
        Self::from_virt(
            ax_mm::iomap_uncached(PhysAddr::from_usize(base), size),
            size,
            region,
        )
    }

    fn from_virt(
        mapping: Result<ax_hal::mem::VirtAddr, ax_mm::MmError>,
        size: usize,
        region: &'static str,
    ) -> Result<Self, IvshmemError> {
        let address = mapping.map_err(|_| IvshmemError::MapFailed { region })?;
        let ptr = NonNull::new(address.as_mut_ptr()).ok_or(IvshmemError::MapFailed { region })?;
        Ok(Self { ptr, size })
    }

    fn subrange(&self, offset: usize, width: usize) -> Result<NonNull<u8>, IvshmemError> {
        self.check_bounds(offset, width)?;
        // SAFETY: the checked range is contained in the live mapping.
        Ok(unsafe { NonNull::new_unchecked(self.ptr.as_ptr().add(offset)) })
    }

    fn check_bounds(&self, offset: usize, width: usize) -> Result<(), IvshmemError> {
        let end = offset
            .checked_add(width)
            .ok_or(IvshmemError::AccessOutOfRange {
                offset,
                width,
                region_size: self.size,
            })?;
        if end > self.size {
            return Err(IvshmemError::AccessOutOfRange {
                offset,
                width,
                region_size: self.size,
            });
        }
        Ok(())
    }

    fn check_scalar(&self, offset: usize, width: usize) -> Result<(), IvshmemError> {
        self.check_bounds(offset, width)?;
        let address = (self.ptr.as_ptr() as usize)
            .checked_add(offset)
            .ok_or(IvshmemError::UnalignedAccess { offset, width })?;
        if width != 0 && !address.is_multiple_of(width) {
            return Err(IvshmemError::UnalignedAccess { offset, width });
        }
        Ok(())
    }

    fn read_u16(&self, offset: usize) -> Result<u16, IvshmemError> {
        self.check_scalar(offset, size_of::<u16>())?;
        // SAFETY: the range check proves natural alignment and mapping bounds.
        Ok(unsafe { core::ptr::read_volatile(self.ptr.as_ptr().add(offset).cast::<u16>()) })
    }

    fn read_u8(&self, offset: usize) -> Result<u8, IvshmemError> {
        self.check_bounds(offset, size_of::<u8>())?;
        // SAFETY: the byte access is contained in the live mapping.
        Ok(unsafe { core::ptr::read_volatile(self.ptr.as_ptr().add(offset)) })
    }

    fn write_u16(&self, offset: usize, value: u16) -> Result<(), IvshmemError> {
        self.check_scalar(offset, size_of::<u16>())?;
        // SAFETY: the range check proves natural alignment and mapping bounds.
        unsafe { core::ptr::write_volatile(self.ptr.as_ptr().add(offset).cast::<u16>(), value) };
        Ok(())
    }

    fn read_u32(&self, offset: usize) -> Result<u32, IvshmemError> {
        self.check_scalar(offset, size_of::<u32>())?;
        // SAFETY: the range check proves natural alignment and mapping bounds.
        Ok(unsafe { core::ptr::read_volatile(self.ptr.as_ptr().add(offset).cast::<u32>()) })
    }

    fn write_u32(&self, offset: usize, value: u32) -> Result<(), IvshmemError> {
        self.check_scalar(offset, size_of::<u32>())?;
        // SAFETY: the range check proves natural alignment and mapping bounds.
        unsafe { core::ptr::write_volatile(self.ptr.as_ptr().add(offset).cast::<u32>(), value) };
        Ok(())
    }
}

struct PciHost {
    ecam_base: usize,
    memory_bus: u64,
    memory_cpu: u64,
    memory_size: u64,
}

impl PciHost {
    fn bar_cpu_address(
        &self,
        bus_address: u32,
        size: usize,
        bar: u8,
    ) -> Result<usize, IvshmemError> {
        if bus_address == 0 {
            return Err(IvshmemError::InvalidMemoryBar { bar });
        }
        let size = u64::try_from(size).map_err(|_| IvshmemError::BarOutsideWindow { bar })?;
        if !u64::from(bus_address).is_multiple_of(size) {
            return Err(IvshmemError::MisalignedBar { bar });
        }
        let offset = u64::from(bus_address)
            .checked_sub(self.memory_bus)
            .ok_or(IvshmemError::BarOutsideWindow { bar })?;
        if offset
            .checked_add(size)
            .is_none_or(|end| end > self.memory_size)
        {
            return Err(IvshmemError::BarOutsideWindow { bar });
        }
        let address = self
            .memory_cpu
            .checked_add(offset)
            .ok_or(IvshmemError::BarOutsideWindow { bar })?;
        usize::try_from(address).map_err(|_| IvshmemError::BarOutsideWindow { bar })
    }
}

fn has_axvisor_transport_capability(
    ecam: &Mapping,
    function_offset: usize,
) -> Result<bool, IvshmemError> {
    let status = ecam.read_u16(function_offset + PCI_STATUS_OFFSET)?;
    if status & PCI_STATUS_CAPABILITY_LIST == 0 {
        return Ok(false);
    }
    let mut offset =
        usize::from(ecam.read_u8(function_offset + PCI_CAPABILITY_POINTER_OFFSET)? & !0b11);
    for _ in 0..PCI_CAPABILITY_LIST_LIMIT {
        let header_end = offset
            .checked_add(2)
            .ok_or(IvshmemError::CapabilityMismatch)?;
        if offset == 0 || header_end > PCI_CONFIG_SPACE_SIZE {
            return Ok(false);
        }
        let id = ecam.read_u8(function_offset + offset)?;
        let next = usize::from(ecam.read_u8(function_offset + offset + 1)? & !0b11);
        if id == PCI_CAPABILITY_ID_VENDOR {
            let body_end = header_end
                .checked_add(AXVISOR_TRANSPORT_CAPABILITY_BODY_SIZE)
                .ok_or(IvshmemError::CapabilityMismatch)?;
            if body_end > PCI_CONFIG_SPACE_SIZE {
                return Ok(false);
            }
            let body = function_offset + header_end;
            let length = ecam.read_u8(body)?;
            let version = ecam.read_u8(body + 1)?;
            let magic = ecam.read_u32(body + 2)?;
            let protocol = ecam.read_u16(body + 6)?;
            let features = ecam.read_u32(body + 10)?;
            let epoch_offset = ecam.read_u32(body + 14)?;
            let epoch_stride = ecam.read_u32(body + 18)?;
            let common_size = ecam.read_u32(body + 22)?;
            let output_size = ecam.read_u32(body + 26)?;
            let max_peers = ecam.read_u32(body + 30)?;
            return Ok(length == AXVISOR_CAPABILITY_LENGTH
                && version == AXVISOR_CAPABILITY_VERSION
                && magic == AXVISOR_CAPABILITY_MAGIC
                && protocol == AXVISOR_TRANSPORT_PROTOCOL
                && features & AXVISOR_TRANSPORT_FEATURE_PEER_EPOCH != 0
                && epoch_offset as usize == PEER_EPOCH_OFFSET
                && epoch_stride as usize == PEER_EPOCH_STRIDE
                && common_size.is_multiple_of(STATE_TABLE_SIZE as u32)
                && output_size != 0
                && output_size.is_multiple_of(STATE_TABLE_SIZE as u32)
                && (1..=MAX_PROFILE_PEERS).contains(&max_peers));
        }
        if next == offset || (next != 0 && next < offset + 2) {
            return Ok(false);
        }
        offset = next;
    }
    Ok(false)
}

fn configured_pci_host() -> Result<PciHost, IvshmemError> {
    let fdt = ax_hal::dtb::get_fdt().ok_or(IvshmemError::NoBootFdt)?;
    let mut hosts = fdt.find_compatible(&["pci-host-ecam-generic"]);
    let node = hosts.next().ok_or(IvshmemError::InvalidPciHost)?;
    if hosts.next().is_some() {
        return Err(IvshmemError::MultiplePciHosts);
    }
    let mut registers = node.reg().ok_or(IvshmemError::InvalidPciHost)?;
    let ecam = registers.next().ok_or(IvshmemError::InvalidPciHost)?;
    if registers.next().is_some()
        || ecam.size != Some(ECAM_SIZE)
        || !ecam.address.is_multiple_of(ECAM_SIZE as u64)
    {
        return Err(IvshmemError::InvalidPciHost);
    }
    let pci = node.into_pci().ok_or(IvshmemError::InvalidPciHost)?;
    if pci.bus_range() != Some(0..0) {
        return Err(IvshmemError::InvalidPciHost);
    }
    let mut ranges = pci.ranges().map_err(|_| IvshmemError::InvalidPciHost)?;
    let memory = ranges.next().ok_or(IvshmemError::InvalidPciHost)?;
    if ranges.next().is_some()
        || memory.space != fdt_parser::PciSpace::Memory32
        || memory.prefetchable
        || memory.size == 0
    {
        return Err(IvshmemError::InvalidPciHost);
    }
    memory
        .bus_address
        .checked_add(memory.size)
        .filter(|end| *end <= 1 << 32)
        .ok_or(IvshmemError::InvalidPciHost)?;
    memory
        .cpu_address
        .checked_add(memory.size)
        .ok_or(IvshmemError::InvalidPciHost)?;
    Ok(PciHost {
        ecam_base: usize::try_from(ecam.address).map_err(|_| IvshmemError::InvalidPciHost)?,
        memory_bus: memory.bus_address,
        memory_cpu: memory.cpu_address,
        memory_size: memory.size,
    })
}

fn checked_memory_bar(value: u32, bar: u8) -> Result<u32, IvshmemError> {
    if value & PCI_BAR_IO_SPACE != 0 || value & PCI_BAR_TYPE_MASK != 0 {
        return Err(IvshmemError::InvalidMemoryBar { bar });
    }
    let address = value & PCI_BAR_ADDRESS_MASK;
    if address == 0 {
        return Err(IvshmemError::InvalidMemoryBar { bar });
    }
    Ok(address)
}

fn probe_memory_bar_size(
    ecam: &Mapping,
    function_offset: usize,
    bar_offset: usize,
    bar: u8,
) -> Result<(u32, u64), IvshmemError> {
    let command_offset = function_offset + PCI_COMMAND_OFFSET;
    let register_offset = function_offset + bar_offset;
    let original_command = ecam.read_u16(command_offset)?;
    let original_value = ecam.read_u32(register_offset)?;
    let original_address = checked_memory_bar(original_value, bar)?;

    ecam.write_u16(
        command_offset,
        original_command & !PCI_COMMAND_MEMORY_ENABLE,
    )?;
    let probe = ecam
        .write_u32(register_offset, u32::MAX)
        .and_then(|()| ecam.read_u32(register_offset));
    // Restore the complete BAR dword so probing does not clear attributes
    // such as the prefetchable bit advertised by BAR2.
    let restore_bar = ecam.write_u32(register_offset, original_value);
    let restore_command = ecam.write_u16(command_offset, original_command);
    restore_bar?;
    restore_command?;

    let size_mask = probe?;
    let address_mask = size_mask & PCI_BAR_ADDRESS_MASK;
    let size = u64::from((!address_mask).wrapping_add(1));
    Ok((original_address, size))
}

fn expect_bar_size(bar: u8, expected: u64, actual: u64) -> Result<(), IvshmemError> {
    if expected != actual {
        return Err(IvshmemError::BarSizeMismatch {
            bar,
            expected,
            actual,
        });
    }
    Ok(())
}
