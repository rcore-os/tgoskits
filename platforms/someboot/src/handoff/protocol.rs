//! Version 4, pointer-free EFI configuration-table wire format.
//! Producers and consumers must be rebuilt together. Earlier versions are rejected.
use core::mem::size_of;

use super::{ApplicationMetadata, HandoffError, MAX_CMDLINE, MAX_METADATA, validate_cmdline};
use crate::boot_resources::{
    AllocationSnapshot, MAX_ALLOCATIONS, MAX_RESOURCES, PhysicalRange, ResourceSnapshot,
};

#[cfg(any(target_os = "uefi", efi))]
pub(crate) const HANDOFF_GUID: uefi::Guid = uefi::guid!("7b9fa931-a25b-d043-b13c-63435428c473");
const MAGIC: u64 = 0x5342_4841_4e44_4f46;
const VERSION: u32 = 4;
pub const MAX_REGIONS: usize = 128;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryRegion {
    pub range: PhysicalRange,
    /// 0: Free, 1: Ram, 2: KImage, 3: Reserved, 4: MMIO, 5: PerCpuData.
    pub kind: u32,
    pub reserved: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Header {
    magic: u64,
    version: u32,
    size: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct HandoffRecord {
    header: Header,
    // 0: firmware services are active; 1: inherited post-EBS reservations.
    boot_services_exited: u64,
    pub image: PhysicalRange,
    /// Caller image retired only after the receiver installs its own context.
    pub caller_image: PhysicalRange,
    pub archive: PhysicalRange,
    pub cmdline_len: u32,
    pub cmdline_bytes: [u8; MAX_CMDLINE + 1],
    pub resource_count: u32,
    pub resources: [ResourceSnapshot; MAX_RESOURCES],
    pub allocation_count: u32,
    pub allocations: [AllocationSnapshot; MAX_ALLOCATIONS],
    pub metadata_id: [u8; 16],
    pub metadata_version: u32,
    pub metadata_len: u32,
    pub metadata_bytes: [u8; MAX_METADATA],
    pub memory_map_len: u32,
    pub memory_map: [MemoryRegion; MAX_REGIONS],
}
impl Default for HandoffRecord {
    fn default() -> Self {
        Self::empty()
    }
}
impl HandoffRecord {
    pub const fn empty() -> Self {
        Self {
            header: Header {
                magic: MAGIC,
                version: VERSION,
                size: size_of::<Self>() as u32,
            },
            boot_services_exited: 1,
            image: PhysicalRange::new(0, 0),
            caller_image: PhysicalRange::new(0, 0),
            archive: PhysicalRange::new(0, 0),
            cmdline_len: 0,
            cmdline_bytes: [0; MAX_CMDLINE + 1],
            resource_count: 0,
            resources: [ResourceSnapshot {
                id: 0,
                range: PhysicalRange::new(0, 0),
                initialized: 0,
            }; MAX_RESOURCES],
            allocation_count: 0,
            allocations: [AllocationSnapshot {
                resource_id: 0,
                range: PhysicalRange::new(0, 0),
            }; MAX_ALLOCATIONS],
            metadata_id: [0; 16],
            metadata_version: 0,
            metadata_len: 0,
            metadata_bytes: [0; MAX_METADATA],
            memory_map_len: 0,
            memory_map: [MemoryRegion {
                range: PhysicalRange::new(0, 0),
                kind: 0,
                reserved: 0,
            }; MAX_REGIONS],
        }
    }
    /// Describes a firmware-owned invocation. Resources are still managed by
    /// Boot Services; this record does not replace the firmware memory map.
    pub fn for_firmware(spec: super::HandoffSpec<'_>) -> Result<Self, HandoffError> {
        spec.validate()?;
        if !spec.preserve.is_empty() {
            return Err(HandoffError::InvalidTarget);
        }
        let mut record = Self::empty();
        record.boot_services_exited = 0;
        record.image = spec.image;
        if let Some(cmdline) = spec.cmdline {
            record.set_cmdline(cmdline)?;
        }
        if let Some(metadata) = spec.metadata {
            record.set_metadata(metadata)?;
        }
        record.validate()?;
        Ok(record)
    }
    pub fn boot_services_exited(&self) -> bool {
        self.boot_services_exited == 1
    }

    pub fn set_cmdline(&mut self, cmdline: &str) -> Result<(), HandoffError> {
        validate_cmdline(cmdline)?;
        self.cmdline_bytes.fill(0);
        self.cmdline_bytes[..cmdline.len()].copy_from_slice(cmdline.as_bytes());
        self.cmdline_len = cmdline.len() as u32;
        Ok(())
    }
    pub fn cmdline(&self) -> Result<&str, HandoffError> {
        let bytes = self
            .cmdline_bytes
            .get(..self.cmdline_len as usize)
            .ok_or(HandoffError::InvalidCommandLine)?;
        let cmdline = core::str::from_utf8(bytes).map_err(|_| HandoffError::InvalidCommandLine)?;
        validate_cmdline(cmdline)?;
        Ok(cmdline)
    }
    pub fn set_metadata(&mut self, metadata: ApplicationMetadata<'_>) -> Result<(), HandoffError> {
        metadata.validate()?;
        self.metadata_id = metadata.id;
        self.metadata_version = metadata.version;
        self.metadata_bytes.fill(0);
        self.metadata_bytes[..metadata.bytes.len()].copy_from_slice(metadata.bytes);
        self.metadata_len = metadata.bytes.len() as u32;
        Ok(())
    }
    pub fn metadata(&self) -> Option<ApplicationMetadata<'_>> {
        if self.metadata_id == [0; 16] {
            return None;
        }
        Some(ApplicationMetadata {
            id: self.metadata_id,
            version: self.metadata_version,
            bytes: self.metadata_bytes.get(..self.metadata_len as usize)?,
        })
    }
    pub fn resource_slice(&self) -> Result<&[ResourceSnapshot], HandoffError> {
        self.resources
            .get(..self.resource_count as usize)
            .ok_or(HandoffError::InvalidProtocol)
    }
    pub fn allocation_slice(&self) -> Result<&[AllocationSnapshot], HandoffError> {
        self.allocations
            .get(..self.allocation_count as usize)
            .ok_or(HandoffError::InvalidProtocol)
    }
    pub fn validate(&self) -> Result<(), HandoffError> {
        validate_header(self.header)?;
        self.cmdline()?;
        if !self.image.valid()
            || !self.image.start.is_multiple_of(4096)
            || !self.image.len.is_multiple_of(4096)
        {
            return Err(HandoffError::InvalidTarget);
        }
        if self.metadata_id == [0; 16] {
            if self.metadata_len != 0 || self.metadata_version != 0 {
                return Err(HandoffError::InvalidMetadata);
            }
        } else {
            self.metadata()
                .ok_or(HandoffError::InvalidMetadata)?
                .validate()?;
        }
        if self.boot_services_exited == 0 {
            return if self.resource_count == 0
                && self.allocation_count == 0
                && self.memory_map_len == 0
                && self.archive == PhysicalRange::default()
            {
                Ok(())
            } else {
                Err(HandoffError::InvalidProtocol)
            };
        }
        if self.boot_services_exited != 1 {
            return Err(HandoffError::InvalidProtocol);
        }
        if !self.caller_image.valid() || self.caller_image.overlaps(self.image) {
            return Err(HandoffError::InvalidTarget);
        }
        let resources = self.resource_slice()?;
        for (i, r) in resources.iter().enumerate() {
            r.validate()?;
            if resources[..i]
                .iter()
                .any(|v| v.id == r.id || v.range.overlaps(r.range))
            {
                return Err(HandoffError::InvalidProtocol);
            }
        }
        let allocations = self.allocation_slice()?;
        for (i, allocation) in allocations.iter().enumerate() {
            if allocation.resource_id == 0
                || !allocation.range.valid()
                || !allocation.range.start.is_multiple_of(4096)
                || !allocation.range.len.is_multiple_of(4096)
                || allocations[..i]
                    .iter()
                    .any(|a| a.range.overlaps(allocation.range))
            {
                return Err(HandoffError::InvalidProtocol);
            }
            if let Some(resource) = resources.iter().find(|r| r.id == allocation.resource_id)
                && resource.range.overlaps(allocation.range)
                && allocation.range.start < resource.range.start + resource.initialized
            {
                return Err(HandoffError::InvalidProtocol);
            }
        }
        let allocated = |range: PhysicalRange| allocations.iter().any(|a| a.range.contains(range));
        if !allocated(self.image) {
            return Err(HandoffError::InvalidTarget);
        }
        if self.archive != PhysicalRange::default()
            && (!allocated(self.archive)
                || allocations
                    .iter()
                    .any(|a| a.range.contains(self.archive) && a.range.overlaps(self.image)))
        {
            return Err(HandoffError::InvalidProtocol);
        }
        let regions = self
            .memory_map
            .get(..self.memory_map_len as usize)
            .ok_or(HandoffError::InvalidProtocol)?;
        if regions.is_empty() {
            return Err(HandoffError::InvalidProtocol);
        }
        for (i, region) in regions.iter().enumerate() {
            // Physical address zero is valid in a memory map, unlike an allocation.
            if region.range.len == 0
                || region.range.end().is_none()
                || !region.range.start.is_multiple_of(4096)
                || !region.range.len.is_multiple_of(4096)
                || region.kind > 7
                || region.reserved != 0
            {
                return Err(HandoffError::InvalidProtocol);
            }
            let end = region.range.end().unwrap();
            if regions[..i]
                .iter()
                .any(|v| v.range.start < end && region.range.start < v.range.end().unwrap())
            {
                return Err(HandoffError::InvalidProtocol);
            }
        }
        // Every reserved byte, including unallocated arena capacity, must stay
        // outside the inheritor's general allocator. Current code may occupy
        // part of an arena; both KImage and Reserved are retained memory.
        for range in resources
            .iter()
            .map(|r| r.range)
            .chain(allocations.iter().map(|a| a.range))
        {
            let mut cursor = range.start;
            let end = range.end().unwrap();
            while cursor < end {
                let region = regions
                    .iter()
                    .find(|r| r.range.start <= cursor && cursor < r.range.end().unwrap())
                    .ok_or(HandoffError::InvalidProtocol)?;
                if !matches!(region.kind, 2 | 6 | 7) {
                    return Err(HandoffError::InvalidProtocol);
                }
                cursor = end.min(region.range.end().unwrap());
            }
        }
        if !regions
            .iter()
            .any(|r| r.range.start <= self.image.start && r.range.end() >= self.image.end())
        {
            return Err(HandoffError::InvalidTarget);
        }
        // Every receiver plan is a set of prefixes of these capacities. Each
        // prefix introduces at most two boundaries. Reserve that space before
        // publishing so a valid target plan cannot overflow during restoration.
        let receiver = self.receiver_map(&[])?;
        if receiver.len + 2 * resources.len() > MAX_REGIONS {
            return Err(HandoffError::InvalidProtocol);
        }
        Ok(())
    }

    pub(crate) fn receiver_map(
        &self,
        retained: &[ResourceSnapshot],
    ) -> Result<super::memory::BootMap, HandoffError> {
        use super::memory::{BOOT_DATA, BootMap, RECLAIMABLE};
        let mut map = BootMap::from_regions(
            self.memory_map
                .get(..self.memory_map_len as usize)
                .ok_or(HandoffError::InvalidProtocol)?,
        )?;
        for region in &mut map.regions[..map.len] {
            if matches!(region.kind, 2 | 5 | BOOT_DATA) {
                region.kind = RECLAIMABLE;
            }
        }
        map.coalesce();
        for resource in retained.iter().filter(|r| r.id != 0) {
            map.assign(resource.range, BOOT_DATA)?;
        }
        for allocation in self.allocation_slice()? {
            map.assign(allocation.range, BOOT_DATA)?;
        }
        Ok(map)
    }

    /// Checks the fixed-size header before borrowing the complete record.
    ///
    /// # Safety
    /// `pointer` must reference at least a readable Header in a trusted EFI
    /// allocation. After a valid header, its reported allocation must contain
    /// `size_of::<Self>()` readable, aligned bytes and stay immutable for `'a`.
    pub unsafe fn from_ptr<'a>(pointer: *const Self) -> Result<&'a Self, HandoffError> {
        if pointer.is_null() || !(pointer as usize).is_multiple_of(core::mem::align_of::<Self>()) {
            return Err(HandoffError::InvalidProtocol);
        }
        // SAFETY: only the fixed header is read before checking its ABI and size.
        let header = unsafe { pointer.cast::<Header>().read() };
        validate_header(header)?;
        // SAFETY: the caller guarantees the full allocation after header validation.
        let record = unsafe { &*pointer };
        record.validate()?;
        Ok(record)
    }
}
fn validate_header(header: Header) -> Result<(), HandoffError> {
    if header.version != VERSION {
        return Err(HandoffError::UnsupportedVersion);
    }
    if header.magic != MAGIC || header.size as usize != size_of::<HandoffRecord>() {
        return Err(HandoffError::InvalidProtocol);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> HandoffRecord {
        let mut r = HandoffRecord::empty();
        r.image = PhysicalRange::new(0x200000, 4096);
        r.caller_image = PhysicalRange::new(0x300000, 4096);
        r.resource_count = 1;
        r.resources[0] = ResourceSnapshot {
            id: 42,
            range: PhysicalRange::new(0x200000, 8192),
            initialized: 0,
        };
        r.allocation_count = 1;
        r.allocations[0] = AllocationSnapshot {
            resource_id: 42,
            range: r.image,
        };
        r.memory_map_len = 1;
        r.memory_map[0] = MemoryRegion {
            range: PhysicalRange::new(0x200000, 8192),
            kind: 6,
            reserved: 0,
        };
        r
    }
    #[test]
    fn validates_protocol_and_rejects_unknown_versions_and_unowned_bytes() {
        let mut r = record();
        assert!(r.validate().is_ok());
        r.memory_map[0].kind = 3;
        assert_eq!(r.validate(), Err(HandoffError::InvalidProtocol));
        r.memory_map[0].kind = 0;
        assert_eq!(r.validate(), Err(HandoffError::InvalidProtocol));
        r.memory_map[0].kind = 6;
        r.memory_map[0].range.len = 4096;
        assert_eq!(r.validate(), Err(HandoffError::InvalidProtocol));
        r.memory_map[0].range.len = 8192;
        r.header.version = 3;
        assert_eq!(r.validate(), Err(HandoffError::UnsupportedVersion));
        r.header.version = VERSION;
        r.image.len = 8192;
        assert_eq!(r.validate(), Err(HandoffError::InvalidTarget));
        r.image.len = 4096;
        r.resources[0].initialized = 4096;
        assert_eq!(r.validate(), Err(HandoffError::InvalidProtocol));
        r.image.start += 4096;
        r.allocations[0].range.start = r.image.start;
        assert!(r.validate().is_ok());
        r.resource_count = 9;
        assert_eq!(r.validate(), Err(HandoffError::InvalidProtocol));

        let mut firmware = HandoffRecord::for_firmware(super::super::HandoffSpec {
            preserve: &[],
            image: PhysicalRange::new(0x200000, 4096),
            cmdline: Some("boot=firmware"),
            initramfs: None,
            metadata: Some(ApplicationMetadata {
                id: [1; 16],
                version: 1,
                bytes: b"application",
            }),
        })
        .unwrap();
        assert!(!firmware.boot_services_exited());
        assert!(firmware.validate().is_ok());
        firmware.resource_count = 1;
        assert_eq!(firmware.validate(), Err(HandoffError::InvalidProtocol));
        firmware.resource_count = 0;
        firmware.boot_services_exited = 2;
        assert_eq!(firmware.validate(), Err(HandoffError::InvalidProtocol));
    }
    #[test]
    fn metadata_and_command_line_are_bounded() {
        let mut r = record();
        r.set_cmdline("hello=world").unwrap();
        assert_eq!(r.cmdline().unwrap(), "hello=world");
        assert_eq!(
            r.set_cmdline("bad\0args"),
            Err(HandoffError::InvalidCommandLine)
        );
        let data = [7; MAX_METADATA];
        r.set_metadata(ApplicationMetadata {
            id: [1; 16],
            version: 1,
            bytes: &data,
        })
        .unwrap();
        assert_eq!(r.metadata().unwrap().bytes, &data);
        assert!(r.validate().is_ok());
        r.metadata_len += 1;
        assert_eq!(r.validate(), Err(HandoffError::InvalidMetadata));
    }
}
