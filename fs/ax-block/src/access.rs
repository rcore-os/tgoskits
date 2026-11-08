//! Physical-device ownership, independent of filesystems and mount registries.
//!
//! Admission changes are short critical sections. Device I/O and cache
//! writeback run after releasing the admission lock. An in-flight request must
//! retain its owner's token through terminal completion, including cancellation
//! of the task waiting for it.

use alloc::{
    boxed::Box,
    sync::{Arc, Weak},
    vec::Vec,
};

use crate::{BlockDevice, BlockError, BlockRegion, BlockResult, DeviceRef, os::sync::Mutex};

/// One physical volume owner. Forked endpoints retain this same reservation.
/// Requests use absolute device sectors, checked against `region()` on every I/O.
pub struct VolumeLease {
    /// Dropped first, so final cache writeback still holds the reservation.
    backend: Box<dyn BlockDevice>,
    /// The shared reservation survives every derived endpoint.
    owner: Arc<AccessOwner>,
    /// Absolute bounds of this endpoint; derivation can only narrow them.
    region: BlockRegion,
    /// A derived endpoint cannot acquire write permission from a read-only view.
    access: VolumeAccess,
}

/// Exclusive unpublished device transaction, convertible to volume occupancy.
pub struct DiskMaintenance {
    /// Whole-device I/O remains excluded until this transaction is published.
    device: VolumeLease,
}

/// Permission control bounded by the access granted to the original endpoint.
pub struct VolumePermissions {
    /// Keeps the same reservation live across filesystem reconfiguration.
    owner: Arc<AccessOwner>,
    /// A read-only lease can never mint writable authority.
    writable: bool,
}

impl VolumePermissions {
    /// Revokes write access from every endpoint derived from this owner.
    /// The caller must finish exclusive recovery before publishing readers.
    pub fn restrict_read_only(&self) -> BlockResult<()> {
        let _domain = self.owner.domain.state.lock();
        let mut state = self.owner.state.lock();
        if state.pending != 0 {
            return Err(BlockError::ResourceBusy);
        }
        match state.kind {
            OwnerKind::Maintenance | OwnerKind::CacheIo => Err(BlockError::InvalidState),
            OwnerKind::Volume(_) => {
                state.kind = OwnerKind::Volume(VolumeAccess::ReadOnly);
                Ok(())
            }
        }
    }

    /// Restores originally granted writes only after overlapping owners exit.
    /// No device I/O runs under admission locks. Failure leaves access unchanged.
    pub fn restore_read_write(&self) -> BlockResult<()> {
        if !self.writable {
            return Err(BlockError::Unsupported);
        }
        let domain = self.owner.domain.state.lock();
        let mut state = self.owner.state.lock();
        if !matches!(state.kind, OwnerKind::Volume(_)) {
            return Err(BlockError::InvalidState);
        }
        if state.pending != 0 {
            return Err(BlockError::ResourceBusy);
        }
        for other in domain.owners.iter().filter_map(Weak::upgrade) {
            if Arc::ptr_eq(&other, &self.owner) {
                continue;
            }
            let other = other.state.lock();
            if overlaps(state.region, other.region) {
                return Err(BlockError::ResourceBusy);
            }
        }
        state.kind = OwnerKind::Volume(VolumeAccess::ReadWrite);
        Ok(())
    }
}

impl DeviceRef {
    /// Validates an observation without opening a second filesystem owner.
    pub fn validate_volume_descriptor(&self, descriptor: &VolumeDescriptor) -> BlockResult<()> {
        let state = self.access_domain().state.lock();
        self.access_domain().validate_descriptor(&state, descriptor)
    }
    /// Queries validated layout metadata without opening a mounted volume.
    /// The first query scans under a read lease; maintenance replaces the cache.
    pub fn volume_layout(
        self: &Arc<Self>,
        disk_id: crate::volume::DiskId,
    ) -> BlockResult<(VolumeDescriptor, Arc<crate::volume::VolumeScan>)> {
        let region = BlockRegion::from_num_blocks(self.device_info().num_blocks);
        {
            let state = self.access_domain().state.lock();
            if let Some(layout) = &state.layout {
                return Ok((
                    VolumeDescriptor {
                        domain: Arc::downgrade(self.access_domain()),
                        region,
                        generation: state.generation,
                    },
                    layout.clone(),
                ));
            }
        }
        let descriptor = self.describe_volume(region)?;
        let mut lease = self.open_volume(&descriptor, VolumeAccess::ReadOnly)?;
        let layout = Arc::new(
            crate::volume::scan_volumes(&mut VolumeIo::new(&mut lease), disk_id)
                .map_err(volume_error)?,
        );
        self.access_domain().state.lock().layout = Some(layout.clone());
        Ok((descriptor, layout))
    }

    /// Describes a region at the current layout generation without opening I/O.
    pub fn describe_volume(&self, region: BlockRegion) -> BlockResult<VolumeDescriptor> {
        if region.start_lba >= region.end_lba || region.end_lba > self.device_info().num_blocks {
            return Err(BlockError::InvalidRequest);
        }
        Ok(self.access_domain().describe(region))
    }

    /// Opens an observed volume, rejecting stale geometry and conflicting owners.
    pub fn open_volume(
        self: &Arc<Self>,
        descriptor: &VolumeDescriptor,
        access: VolumeAccess,
    ) -> BlockResult<VolumeLease> {
        if access == VolumeAccess::ReadWrite && self.device_info().read_only {
            return Err(BlockError::Unsupported);
        }
        let owner = self.access_domain().open(descriptor, access)?;
        let backend = crate::block::boxed_native_handle_block_device(self.clone(), owner.clone())?;
        Ok(VolumeLease {
            backend,
            owner,
            region: descriptor.region,
            access,
        })
    }

    /// Reserves the disk, then drains its shared cache outside admission locks.
    pub fn maintain(self: &Arc<Self>) -> BlockResult<DiskMaintenance> {
        if self.device_info().read_only {
            return Err(BlockError::Unsupported);
        }
        let region = BlockRegion::from_num_blocks(self.device_info().num_blocks);
        let owner = self.access_domain().maintain(region)?;
        let backend = crate::block::boxed_native_handle_block_device(self.clone(), owner.clone())?;
        #[cfg(feature = "cache")]
        let backend = {
            let mut backend = backend;
            backend.flush()?;
            backend
        };
        Ok(DiskMaintenance {
            device: VolumeLease {
                backend,
                owner,
                region,
                access: VolumeAccess::ReadWrite,
            },
        })
    }
}

impl VolumeLease {
    /// Controls publication and remount without exceeding this endpoint's grant.
    pub fn permissions(&self) -> VolumePermissions {
        VolumePermissions {
            owner: self.owner.clone(),
            writable: self.access == VolumeAccess::ReadWrite,
        }
    }
    /// Returns this endpoint's absolute bounds, with an exclusive end sector.
    pub fn region(&self) -> BlockRegion {
        self.region
    }

    /// Checks alignment, overflow and both endpoint and current owner bounds.
    fn check_io(&self, block: u64, bytes: usize, write: bool) -> BlockResult<RequestOwnership> {
        let size = self.backend.block_size();
        if size == 0 || !bytes.is_multiple_of(size) {
            return Err(BlockError::InvalidRequest);
        }
        let end = block
            .checked_add(u64::try_from(bytes / size).map_err(|_| BlockError::InvalidRequest)?)
            .ok_or(BlockError::InvalidRequest)?;
        self.check_region(
            BlockRegion {
                start_lba: block,
                end_lba: end,
            },
            write,
        )
    }

    /// Enforces the endpoint bounds even while its owner has whole-disk authority.
    fn check_region(&self, region: BlockRegion, write: bool) -> BlockResult<RequestOwnership> {
        if region.end_lba < region.start_lba
            || region.start_lba < self.region.start_lba
            || region.end_lba > self.region.end_lba
        {
            return Err(BlockError::InvalidRequest);
        }
        if write && self.access == VolumeAccess::ReadOnly {
            return Err(BlockError::Unsupported);
        }
        self.owner.request(region, write)
    }
}

impl BlockDevice for VolumeLease {
    fn name(&self) -> &str {
        self.backend.name()
    }
    fn num_blocks(&self) -> u64 {
        self.backend.num_blocks()
    }
    fn block_size(&self) -> usize {
        self.backend.block_size()
    }
    fn read_block(&mut self, block: u64, buffer: &mut [u8]) -> BlockResult {
        let _request = self.check_io(block, buffer.len(), false)?;
        self.backend.read_block(block, buffer)
    }
    #[cfg(feature = "cache")]
    fn write_block(&mut self, block: u64, buffer: &[u8]) -> BlockResult {
        let _request = self.check_io(block, buffer.len(), true)?;
        self.backend.write_block(block, buffer)
    }
    #[cfg(feature = "cache")]
    fn flush(&mut self) -> BlockResult {
        let _request = self.owner.request_flush()?;
        self.backend.flush()
    }
    #[cfg(feature = "write-zeroes")]
    fn physical_block_size(&self) -> usize {
        self.backend.physical_block_size()
    }
    fn is_read_only(&self) -> bool {
        self.access == VolumeAccess::ReadOnly
            || self.owner.read_only()
            || self.backend.is_read_only()
    }
    #[cfg(feature = "write-zeroes")]
    fn supports_flush(&self) -> bool {
        self.backend.supports_flush()
    }
    #[cfg(feature = "write-zeroes")]
    fn supports_fua(&self) -> bool {
        self.backend.supports_fua()
    }
    #[cfg(feature = "write-zeroes")]
    fn write_block_fua(&mut self, block: u64, buffer: &[u8]) -> BlockResult {
        let _request = self.check_io(block, buffer.len(), true)?;
        self.backend.write_block_fua(block, buffer)
    }
    #[cfg(feature = "write-zeroes")]
    fn write_zeroes(&mut self, ranges: &[BlockRegion]) -> BlockResult {
        let mut requests = Vec::new();
        requests
            .try_reserve_exact(ranges.len())
            .map_err(|_| BlockError::NoMemory)?;
        for range in ranges {
            requests.push(self.check_region(*range, true)?);
        }
        self.backend.write_zeroes(ranges)
    }
    #[cfg(feature = "write-zeroes")]
    fn fork_io(&self) -> BlockResult<Box<dyn BlockDevice>> {
        Ok(Box::new(Self {
            backend: self.backend.fork_io()?,
            owner: self.owner.clone(),
            region: self.region,
            access: self.access,
        }))
    }
}

impl DiskMaintenance {
    /// Borrows the exclusive whole-device endpoint for partition-table I/O.
    pub fn device_mut(&mut self) -> &mut dyn BlockDevice {
        &mut self.device
    }

    /// Reads and records the current table while maintenance excludes other owners.
    pub fn scan_layout(
        &mut self,
        disk_id: crate::volume::DiskId,
    ) -> crate::volume::Result<crate::volume::VolumeScan> {
        let scan = crate::volume::scan_volumes(&mut VolumeIo::new(&mut self.device), disk_id)?;
        self.device.owner.domain.state.lock().layout = Some(Arc::new(scan.clone()));
        Ok(scan)
    }

    /// Commits GPT/MBR growth and rereads the final bounds and partition identity.
    /// Layout observations are invalidated before any potentially partial write.
    #[cfg(feature = "write-zeroes")]
    pub fn grow_partition(
        &mut self,
        partition: crate::volume::PartitionId,
    ) -> crate::volume::Result<()> {
        use crate::volume::{DiskId, PartitionTableKind};
        let before = self.scan_layout(DiskId(0))?;
        let source = before
            .volumes
            .iter()
            .find(|volume| {
                volume.partition_id == partition
                    && matches!(
                        volume.table_kind,
                        PartitionTableKind::Gpt | PartitionTableKind::Mbr
                    )
            })
            .ok_or(crate::volume::Error::InvalidPartitionTable)?;
        let identity = source.partuuid.clone();
        let plan =
            crate::volume::partition_growth(&mut VolumeIo::new(&mut self.device), partition)?;
        // Discovery follows the valid primary, while growth can resume a
        // compatible enlarged backup. That backup must retain the observed
        // partition start and cannot shrink its bounds.
        if source.region.start_block != plan.current.start_block
            || source.region.num_blocks > plan.current.num_blocks
        {
            return Err(crate::volume::Error::InvalidPartitionTable);
        }
        // Retire all observations before the first possible partition-table write. A failed
        // write/flush may have committed one copy and cannot preserve old observations.
        self.device
            .owner
            .begin_layout_change()
            .map_err(|_| crate::volume::Error::Writer)?;
        let committed =
            crate::volume::grow_partition(&mut VolumeIo::new(&mut self.device), partition)?;
        if committed != plan.target {
            return Err(crate::volume::Error::InvalidPartitionTable);
        }
        let after = self.scan_layout(DiskId(0))?;
        if !after.volumes.iter().any(|volume| {
            volume.partition_id == partition
                && volume.table_kind == source.table_kind
                && volume.partuuid == identity
                && volume.region == committed
        }) {
            return Err(crate::volume::Error::InvalidPartitionTable);
        }
        Ok(())
    }

    /// Publishes final volume occupancy without releasing device exclusion.
    pub fn publish(self, region: BlockRegion, access: VolumeAccess) -> BlockResult<()> {
        self.device.owner.publish(region, access)
    }

    /// Owns an isolated host-image fixture under the same production admission rules.
    #[cfg(feature = "host-test")]
    pub fn from_test_backend(backend: Box<dyn BlockDevice>) -> Self {
        let region = BlockRegion::from_num_blocks(backend.num_blocks());
        let owner = AccessDomain::new()
            .maintain(region)
            .expect("fresh test backend has no owners");
        Self {
            device: VolumeLease {
                backend,
                owner,
                region,
                access: VolumeAccess::ReadWrite,
            },
        }
    }
}

use crate::volume::DeviceReader as VolumeIo;

/// Preserves allocation and metadata/I/O failure distinctions at the runtime boundary.
fn volume_error(error: crate::volume::Error) -> BlockError {
    match error {
        crate::volume::Error::NoMemory => BlockError::NoMemory,
        crate::volume::Error::Reader | crate::volume::Error::Writer => BlockError::Io,
        crate::volume::Error::Unsupported => BlockError::Unsupported,
        _ => BlockError::InvalidRequest,
    }
}

/// Access granted to one volume owner and all of its derived I/O endpoints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VolumeAccess {
    /// Concurrent readers are permitted, but no endpoint may write or recover.
    ReadOnly,
    /// One owner may modify the region; overlapping owners are excluded.
    ReadWrite,
}

/// An immutable layout observation; opening it checks the device generation.
#[derive(Clone)]
pub struct VolumeDescriptor {
    /// Admission domain that produced this observation.
    domain: Weak<AccessDomain>,
    /// Absolute physical sectors described by this observation.
    region: BlockRegion,
    /// Layout generation observed while producing the descriptor.
    generation: u64,
}

impl VolumeDescriptor {
    /// Returns the physical extent, with an exclusive end sector.
    pub fn region(&self) -> BlockRegion {
        self.region
    }

    /// Narrows an observed layout without refreshing its generation.
    pub fn subregion(&self, region: BlockRegion) -> BlockResult<Self> {
        if region.start_lba >= region.end_lba
            || region.start_lba < self.region.start_lba
            || region.end_lba > self.region.end_lba
        {
            return Err(BlockError::InvalidRequest);
        }
        Ok(Self {
            domain: self.domain.clone(),
            region,
            generation: self.generation,
        })
    }
}

/// Admission state shared by every reference to one physical device.
pub(crate) struct AccessDomain {
    /// Protects layout generation and all live ownership reservations.
    state: Mutex<DomainState>,
}

/// Layout and occupancy updated atomically before admitting a new owner.
struct DomainState {
    /// Advanced after a durable partition-table change.
    generation: u64,
    /// Last validated table at this generation, queryable while volumes are busy.
    layout: Option<Arc<crate::volume::VolumeScan>>,
    /// Weak entries never extend an owner's lifetime.
    owners: Vec<Weak<AccessOwner>>,
}

/// Reservation kind, including unpublished whole-device maintenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OwnerKind {
    Volume(VolumeAccess),
    Maintenance,
    CacheIo,
}

/// Mutable bounds and permission of a reservation.
struct OwnerState {
    /// Absolute sectors this reservation currently authorizes.
    region: BlockRegion,
    /// The permission under which new I/O may be admitted.
    kind: OwnerKind,
    /// Requests admitted under these bounds, including cancelled waiters.
    pending: usize,
}

/// Shared token retained by endpoints and requests through terminal completion.
pub(crate) struct AccessOwner {
    /// Authoritative per-device admission domain.
    domain: Arc<AccessDomain>,
    /// Bounds and permissions, atomically replaced during publication.
    state: Mutex<OwnerState>,
}

impl AccessDomain {
    /// Creates the one admission domain owned by a physical device runtime.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(DomainState {
                generation: 0,
                layout: None,
                owners: Vec::new(),
            }),
        })
    }

    /// Records geometry at the currently observed partition-table generation.
    pub(crate) fn describe(self: &Arc<Self>, region: BlockRegion) -> VolumeDescriptor {
        let state = self.state.lock();
        VolumeDescriptor {
            domain: Arc::downgrade(self),
            region,
            generation: state.generation,
        }
    }

    /// Admits a volume without allowing overlapping writers or maintenance.
    pub(crate) fn open(
        self: &Arc<Self>,
        descriptor: &VolumeDescriptor,
        access: VolumeAccess,
    ) -> BlockResult<Arc<AccessOwner>> {
        let mut state = self.state.lock();
        self.validate_descriptor(&state, descriptor)?;
        state.owners.retain(|owner| owner.strong_count() != 0);
        for existing in state.owners.iter().filter_map(Weak::upgrade) {
            let existing = existing.state.lock();
            if matches!(existing.kind, OwnerKind::Maintenance | OwnerKind::CacheIo)
                || (overlaps(existing.region, descriptor.region)
                    && (access == VolumeAccess::ReadWrite
                        || existing.kind == OwnerKind::Volume(VolumeAccess::ReadWrite)))
            {
                return Err(BlockError::ResourceBusy);
            }
        }
        state
            .owners
            .try_reserve(1)
            .map_err(|_| BlockError::NoMemory)?;
        let owner = Arc::new(AccessOwner {
            domain: self.clone(),
            state: Mutex::new(OwnerState {
                region: descriptor.region,
                kind: OwnerKind::Volume(access),
                pending: 0,
            }),
        });
        state.owners.push(Arc::downgrade(&owner));
        Ok(owner)
    }

    /// Reserves the complete device before any cache writeback or layout I/O.
    /// Live request tokens count as owners even after their waiter is dropped.
    pub(crate) fn maintain(self: &Arc<Self>, region: BlockRegion) -> BlockResult<Arc<AccessOwner>> {
        let mut state = self.state.lock();
        state.owners.retain(|owner| owner.strong_count() != 0);
        if !state.owners.is_empty() {
            return Err(BlockError::ResourceBusy);
        }
        state
            .owners
            .try_reserve(1)
            .map_err(|_| BlockError::NoMemory)?;
        let owner = Arc::new(AccessOwner {
            domain: self.clone(),
            state: Mutex::new(OwnerState {
                region,
                kind: OwnerKind::Maintenance,
                pending: 0,
            }),
        });
        state.owners.push(Arc::downgrade(&owner));
        Ok(owner)
    }

    /// Retains a whole-device cache operation without pinning a volume lease.
    /// Existing volume owners may coexist; maintenance cannot enter this domain.
    pub(crate) fn cache_io(self: &Arc<Self>, region: BlockRegion) -> BlockResult<Arc<AccessOwner>> {
        let mut state = self.state.lock();
        state.owners.retain(|owner| owner.strong_count() != 0);
        if state
            .owners
            .iter()
            .filter_map(Weak::upgrade)
            .any(|owner| owner.state.lock().kind == OwnerKind::Maintenance)
        {
            return Err(BlockError::ResourceBusy);
        }
        state
            .owners
            .try_reserve(1)
            .map_err(|_| BlockError::NoMemory)?;
        let owner = Arc::new(AccessOwner {
            domain: self.clone(),
            state: Mutex::new(OwnerState {
                region,
                kind: OwnerKind::CacheIo,
                pending: 0,
            }),
        });
        state.owners.push(Arc::downgrade(&owner));
        Ok(owner)
    }

    /// Rejects observations from another device or an older durable layout.
    fn validate_descriptor(
        &self,
        state: &DomainState,
        descriptor: &VolumeDescriptor,
    ) -> BlockResult<()> {
        if !core::ptr::eq(descriptor.domain.as_ptr(), self)
            || descriptor.generation != state.generation
            || descriptor.region.start_lba >= descriptor.region.end_lba
        {
            return Err(BlockError::InvalidRequest);
        }
        Ok(())
    }
}

/// One admitted request, retained until actual terminal completion.
/// It cannot be cloned: each admission increments and retires exactly once.
pub(crate) struct RequestOwnership {
    owner: Arc<AccessOwner>,
}

impl Drop for RequestOwnership {
    fn drop(&mut self) {
        let mut state = self.owner.state.lock();
        state.pending = state
            .pending
            .checked_sub(1)
            .expect("request ownership retires exactly once");
    }
}

impl AccessOwner {
    /// Selects the existing whole-disk owner for internal cache maintenance I/O.
    #[cfg(feature = "cache")]
    pub(crate) fn is_maintenance(&self) -> bool {
        self.state.lock().kind == OwnerKind::Maintenance
    }

    /// Atomically validates and retains one request under the current authority.
    pub(crate) fn request(
        self: &Arc<Self>,
        region: BlockRegion,
        write: bool,
    ) -> BlockResult<RequestOwnership> {
        let mut state = self.state.lock();
        check_owner_region(&state, region, write)?;
        state.pending = state
            .pending
            .checked_add(1)
            .ok_or(BlockError::InvalidState)?;
        Ok(RequestOwnership {
            owner: self.clone(),
        })
    }

    /// Retains a flush barrier, including for a read-only endpoint.
    pub(crate) fn request_flush(self: &Arc<Self>) -> BlockResult<RequestOwnership> {
        let mut state = self.state.lock();
        state.pending = state
            .pending
            .checked_add(1)
            .ok_or(BlockError::InvalidState)?;
        Ok(RequestOwnership {
            owner: self.clone(),
        })
    }
    /// Reads permission independently of any narrowed endpoint geometry.
    fn read_only(&self) -> bool {
        self.state.lock().kind == OwnerKind::Volume(VolumeAccess::ReadOnly)
    }
    /// Invalidates cached geometry before a potentially partial partition-table update.
    #[cfg(feature = "write-zeroes")]
    fn begin_layout_change(&self) -> BlockResult<()> {
        let mut domain = self.domain.state.lock();
        let state = self.state.lock();
        if state.kind != OwnerKind::Maintenance {
            return Err(BlockError::InvalidState);
        }
        if state.pending != 0 {
            return Err(BlockError::ResourceBusy);
        }
        domain.generation = domain
            .generation
            .checked_add(1)
            .ok_or(BlockError::InvalidState)?;
        domain.layout = None;
        Ok(())
    }

    /// Atomically converts whole-device maintenance into a mounted-volume owner.
    /// No interval without an owner exists between the two states.
    pub(crate) fn publish(&self, region: BlockRegion, access: VolumeAccess) -> BlockResult<()> {
        let _domain = self.domain.state.lock();
        let mut state = self.state.lock();
        if state.pending != 0 {
            return Err(BlockError::ResourceBusy);
        }
        if state.kind != OwnerKind::Maintenance
            || region.start_lba >= region.end_lba
            || region.start_lba < state.region.start_lba
            || region.end_lba > state.region.end_lba
        {
            return Err(BlockError::InvalidRequest);
        }
        state.region = region;
        state.kind = OwnerKind::Volume(access);
        Ok(())
    }
}

/// Checks bounds and permissions while the caller retains owner-state exclusion.
fn check_owner_region(state: &OwnerState, region: BlockRegion, write: bool) -> BlockResult<()> {
    if region.end_lba < region.start_lba
        || region.start_lba < state.region.start_lba
        || region.end_lba > state.region.end_lba
    {
        return Err(BlockError::InvalidRequest);
    }
    if write && state.kind == OwnerKind::Volume(VolumeAccess::ReadOnly) {
        return Err(BlockError::Unsupported);
    }
    Ok(())
}

/// Reports whether two nonempty physical ranges share a sector.
fn overlaps(left: BlockRegion, right: BlockRegion) -> bool {
    left.start_lba < right.end_lba && right.start_lba < left.end_lba
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Requests retain exclusion, and owner transitions cannot outrun them.
    #[test]
    fn overlapping_permissions_and_request_ownership_exclude_maintenance() {
        let domain = AccessDomain::new();
        let region = BlockRegion::new(8, 8);
        let volume = domain.describe(region);
        let first = domain.open(&volume, VolumeAccess::ReadOnly).unwrap();
        let second = domain.open(&volume, VolumeAccess::ReadOnly).unwrap();
        assert!(matches!(
            domain.open(&volume, VolumeAccess::ReadWrite),
            Err(BlockError::ResourceBusy)
        ));
        let request = first.request(region, false).unwrap();
        drop(first);
        drop(second);
        assert!(matches!(
            domain.maintain(BlockRegion::new(0, 64)),
            Err(BlockError::ResourceBusy)
        ));
        drop(request);
        let maintenance = domain.maintain(BlockRegion::new(0, 64)).unwrap();
        let outside = maintenance.request(BlockRegion::new(32, 8), true).unwrap();
        assert_eq!(
            maintenance.publish(region, VolumeAccess::ReadWrite),
            Err(BlockError::ResourceBusy)
        );
        assert!(matches!(
            domain.open(&volume, VolumeAccess::ReadOnly),
            Err(BlockError::ResourceBusy)
        ));
        drop(outside);
        maintenance
            .publish(region, VolumeAccess::ReadWrite)
            .unwrap();
        let write = maintenance.request(region, true).unwrap();
        assert_eq!(
            VolumePermissions {
                owner: maintenance.clone(),
                writable: true,
            }
            .restrict_read_only(),
            Err(BlockError::ResourceBusy)
        );
        drop(write);
        let permissions = VolumePermissions {
            owner: maintenance.clone(),
            writable: true,
        };
        permissions.restrict_read_only().unwrap();
        assert!(matches!(
            maintenance.request(region, true),
            Err(BlockError::Unsupported)
        ));
        assert!(maintenance.request(region, false).is_ok());
        let reading = maintenance.request(region, false).unwrap();
        assert_eq!(
            permissions.restore_read_write(),
            Err(BlockError::ResourceBusy)
        );
        drop(reading);
        let reader = domain.open(&volume, VolumeAccess::ReadOnly).unwrap();
        assert_eq!(
            permissions.restore_read_write(),
            Err(BlockError::ResourceBusy)
        );
        assert!(matches!(
            maintenance.request(region, true),
            Err(BlockError::Unsupported)
        ));
        let readonly_permissions = VolumePermissions {
            owner: reader.clone(),
            writable: false,
        };
        assert_eq!(
            readonly_permissions.restore_read_write(),
            Err(BlockError::Unsupported)
        );
        drop(readonly_permissions);
        drop(reader);
        permissions.restore_read_write().unwrap();
        assert!(maintenance.request(region, true).is_ok());
        assert!(matches!(
            maintenance.request(BlockRegion::new(32, 8), false),
            Err(BlockError::InvalidRequest)
        ));
        drop(permissions);
        drop(maintenance);
        assert!(domain.open(&volume, VolumeAccess::ReadWrite).is_ok());
    }

    /// Cache requests exclude maintenance without retaining an unrelated volume.
    #[test]
    fn cache_requests_coexist_with_volumes_and_block_new_admission_until_completion() {
        let domain = AccessDomain::new();
        let full = BlockRegion::new(0, 64);
        let descriptor = domain.describe(BlockRegion::new(8, 8));
        let volume = domain.open(&descriptor, VolumeAccess::ReadWrite).unwrap();
        let cache = domain.cache_io(full).unwrap();
        let request = cache.request_flush().unwrap();
        drop(cache);
        drop(volume);
        assert!(matches!(
            domain.open(&descriptor, VolumeAccess::ReadOnly),
            Err(BlockError::ResourceBusy)
        ));
        assert!(matches!(
            domain.maintain(full),
            Err(BlockError::ResourceBusy)
        ));
        drop(request);
        let maintenance = domain.maintain(full).unwrap();
        assert!(matches!(
            domain.cache_io(full),
            Err(BlockError::ResourceBusy)
        ));
        drop(maintenance);
        assert!(domain.open(&descriptor, VolumeAccess::ReadWrite).is_ok());
    }

    /// Layout writes retire stale views; publication retains the exclusive owner.
    #[test]
    #[cfg(feature = "write-zeroes")]
    fn layout_changes_and_publication_preserve_the_same_owner() {
        let domain = AccessDomain::new();
        let old = BlockRegion::new(8, 8);
        let expanded = BlockRegion::new(8, 24);
        let stale = domain.describe(old);
        let owner = domain.maintain(BlockRegion::new(0, 64)).unwrap();
        domain.state.lock().layout = Some(Arc::new(crate::volume::VolumeScan {
            volumes: Vec::new(),
            table_metadata: Vec::new(),
        }));
        let request = owner.request(old, true).unwrap();
        assert_eq!(owner.begin_layout_change(), Err(BlockError::ResourceBusy));
        assert_eq!(
            owner.publish(expanded, VolumeAccess::ReadOnly),
            Err(BlockError::ResourceBusy)
        );
        drop(request);
        owner.begin_layout_change().unwrap();
        assert!(
            domain.state.lock().layout.is_none(),
            "failed GPT writes cannot preserve cached layout"
        );
        let other = AccessDomain::new()
            .maintain(BlockRegion::new(0, 64))
            .unwrap();
        owner.publish(expanded, VolumeAccess::ReadOnly).unwrap();
        assert!(owner.request(expanded, false).is_ok());
        assert!(matches!(
            owner.request(expanded, true),
            Err(BlockError::Unsupported)
        ));
        assert!(matches!(
            domain.open(&stale, VolumeAccess::ReadOnly),
            Err(BlockError::InvalidRequest)
        ));
        assert!(matches!(
            domain.maintain(BlockRegion::new(0, 64)),
            Err(BlockError::ResourceBusy)
        ));
        drop(owner);
        assert!(
            domain
                .open(&domain.describe(expanded), VolumeAccess::ReadWrite)
                .is_ok()
        );
        assert!(matches!(
            other
                .domain
                .open(&domain.describe(old), VolumeAccess::ReadOnly),
            Err(BlockError::InvalidRequest)
        ));
    }
}
