//! Immutable per-MM user virtual-address policy.

use ax_memory_addr::{PAGE_SIZE_4K, VirtAddr, VirtAddrRange};

use crate::{StarryError, StarryResult, config};

/// User-visible address-space layout captured when an MM is created.
///
/// The platform supplies the hardware/page-table capability. Starry then
/// intersects it with its ABI policy. Keeping the result in `AddrSpace`
/// mirrors Linux's immutable MM context: a later syscall cannot observe a
/// different TASK_SIZE or stack ceiling from the one used by exec/fork.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UserVirtualAddressLayout {
    range: VirtAddrRange,
    stack_top: VirtAddr,
}

impl UserVirtualAddressLayout {
    /// Derives the default Starry ABI layout from the platform capability.
    pub fn platform_default() -> StarryResult<Self> {
        let platform = ax_runtime::hal::mem::virtual_address_space()
            .map_err(|_| StarryError::Unsupported)?;
        Self::from_platform_range(platform.user())
    }

    fn from_platform_range(platform: VirtAddrRange) -> StarryResult<Self> {
        let policy_end = config::USER_SPACE_BASE
            .checked_add(config::USER_SPACE_MAX_SIZE)
            .ok_or(StarryError::BadState)?;
        let start = platform
            .start
            .as_usize()
            .max(config::USER_SPACE_BASE);
        let end = platform.end.as_usize().min(policy_end);
        let range = VirtAddrRange::try_new(VirtAddr::from(start), VirtAddr::from(end))
            .filter(|range| !range.is_empty())
            .ok_or(StarryError::Unsupported)?;
        let stack_top = config::USER_STACK_TOP_MAX.min(end);
        let minimum_fixed_end = config::SIGNAL_TRAMPOLINE
            .checked_add(PAGE_SIZE_4K)
            .ok_or(StarryError::BadState)?;
        if stack_top < start.saturating_add(config::USER_STACK_SIZE)
            || minimum_fixed_end > end
        {
            return Err(StarryError::Unsupported);
        }
        Ok(Self {
            range,
            stack_top: VirtAddr::from(stack_top),
        })
    }

    /// Constructs an explicitly bounded address space for focused MM tests and
    /// internal isolated mappings. Its stack ceiling is the range end.
    pub(crate) fn from_range(base: VirtAddr, size: usize) -> StarryResult<Self> {
        let range = VirtAddrRange::try_from_start_size(base, size)
            .filter(|range| !range.is_empty())
            .ok_or(StarryError::InvalidInput)?;
        Ok(Self {
            range,
            stack_top: range.end,
        })
    }

    /// Complete half-open userspace range.
    pub const fn range(self) -> VirtAddrRange {
        self.range
    }

    /// Linux-style TASK_SIZE, the exclusive userspace upper bound.
    pub const fn task_size(self) -> VirtAddr {
        self.range.end
    }

    /// Highest address used for the initial fixed stack mapping.
    pub const fn stack_top(self) -> VirtAddr {
        self.stack_top
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    #[test]
    fn low_platform_capability_clips_task_size_and_stack() {
        let hardware_end = VirtAddr::from(1usize << 39);
        let layout = UserVirtualAddressLayout::from_platform_range(VirtAddrRange::new(
            VirtAddr::from(0),
            hardware_end,
        ))
        .unwrap();

        assert_eq!(layout.task_size(), hardware_end);
        assert_eq!(layout.stack_top(), hardware_end);
        assert_eq!(layout.range().start, VirtAddr::from(config::USER_SPACE_BASE));
    }
}
