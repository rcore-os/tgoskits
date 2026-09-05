#![cfg_attr(not(test), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(missing_docs)]
#![doc = include_str!("../README.md")]

#[cfg(all(feature = "uspace", feature = "tls"))]
compile_error!("ax-cpu userspace requires LinuxCurrent and cannot enable kernel TLS mode");

#[macro_use]
extern crate log;

#[macro_use]
extern crate ax_memory_addr;

#[macro_use]
pub mod trap;

pub use trap::TrapOrigin;

mod task_local;
pub use task_local::TaskLocalState;

pub mod cap;

pub mod paging;

/// Kernel task-local storage base owned by one execution context.
///
/// This value follows a task across CPUs. It must never be used as a CPU-local
/// anchor or initialized from an architecture per-CPU register.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KernelTlsBase(usize);

impl KernelTlsBase {
    /// Creates a kernel TLS base from its virtual address.
    pub const fn new(address: usize) -> Self {
        Self(address)
    }

    /// Returns the virtual address represented by this TLS base.
    pub const fn as_usize(self) -> usize {
        self.0
    }

    pub(crate) fn for_task_context(requested: Self) -> Self {
        if cfg!(feature = "tls") {
            requested
        } else {
            assert!(
                requested.0 == 0,
                "LinuxCurrent task contexts must not own a kernel TLS register"
            );
            Self(0)
        }
    }
}

/// Hardware tag policy carried with an installed userspace address space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstalledAddressSpaceMode {
    /// The architecture may retain translations distinguished by a hardware
    /// tag and software generation.
    Tagged,
    /// The architecture uses tag zero and flushes translations when changing
    /// address spaces.
    FullFlush,
}

/// Complete software identity installed with one hardware page-table root.
///
/// The root is intentionally private. Scheduler code moves this value as a
/// unit, while architecture code is the only layer allowed to project the
/// materialized root that is written to a register.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstalledAddressSpace {
    space_id: u64,
    root: ax_memory_addr::PhysAddr,
    hardware_tag: u16,
    tag_generation: u64,
    epoch: u64,
    mode: InstalledAddressSpaceMode,
}

impl InstalledAddressSpace {
    /// Constructs a validated userspace installation identity.
    ///
    /// Returns `None` for the reserved zero identity, a zero root, or a tag
    /// that is inconsistent with its installation mode.
    pub fn user(
        space_id: u64,
        root: ax_memory_addr::PhysAddr,
        hardware_tag: u16,
        tag_generation: u64,
        epoch: u64,
        mode: InstalledAddressSpaceMode,
    ) -> Option<Self> {
        if space_id == 0 || root.as_usize() == 0 {
            return None;
        }
        match mode {
            InstalledAddressSpaceMode::Tagged if hardware_tag == 0 => return None,
            InstalledAddressSpaceMode::FullFlush if hardware_tag != 0 => return None,
            InstalledAddressSpaceMode::Tagged | InstalledAddressSpaceMode::FullFlush => {}
        }
        Some(Self {
            space_id,
            root,
            hardware_tag,
            tag_generation,
            epoch,
            mode,
        })
    }

    /// Constructs the kernel-root context used by bootstrap and CPU-offline
    /// paths. It carries no userspace identity or reusable hardware tag.
    pub const fn kernel(root: ax_memory_addr::PhysAddr) -> Self {
        Self {
            space_id: 0,
            root,
            hardware_tag: 0,
            tag_generation: 0,
            epoch: 0,
            mode: InstalledAddressSpaceMode::FullFlush,
        }
    }

    /// Returns whether this value represents a userspace address space.
    pub const fn is_user(self) -> bool {
        self.space_id != 0
    }

    /// Returns the stable software address-space identity.
    pub const fn space_id(self) -> u64 {
        self.space_id
    }

    /// Returns the hardware address-space tag.
    pub const fn hardware_tag(self) -> u16 {
        self.hardware_tag
    }

    /// Returns the software generation associated with the hardware tag.
    pub const fn tag_generation(self) -> u64 {
        self.tag_generation
    }

    /// Returns the VMA/PTE publication epoch represented by this context.
    pub const fn epoch(self) -> u64 {
        self.epoch
    }

    /// Returns the hardware tag policy.
    pub const fn mode(self) -> InstalledAddressSpaceMode {
        self.mode
    }

    #[cfg(feature = "uspace")]
    pub(crate) const fn root(self) -> ax_memory_addr::PhysAddr {
        self.root
    }

    #[cfg(feature = "uspace")]
    pub(crate) fn validate_architecture_support(self) {
        #[cfg(any(
            target_arch = "x86_64",
            target_arch = "riscv32",
            target_arch = "riscv64",
            target_arch = "loongarch64"
        ))]
        debug_assert!(
            self.root.as_usize() != 0,
            "this architecture requires a materialized kernel or userspace root"
        );

        #[cfg(target_arch = "aarch64")]
        debug_assert!(
            !self.is_user() || self.root.as_usize() != 0,
            "a userspace identity always requires a materialized root"
        );
    }
}

impl Default for InstalledAddressSpace {
    fn default() -> Self {
        Self::kernel(ax_memory_addr::PhysAddr::from_usize(0))
    }
}

#[cfg(feature = "exception-table")]
mod exception_table;
#[cfg(feature = "uspace")]
mod user_access;
#[cfg(feature = "uspace")]
mod uspace_common;
#[cfg(feature = "uspace")]
pub use user_access::{
    UserAccessError, UserAccessType, UserAtomicError, UserAtomicU32Op, user_atomic_u32,
    user_read_u32,
};

cfg_if::cfg_if! {
    if #[cfg(target_arch = "x86_64")] {
        mod x86_64;
        pub use self::x86_64::*;
    } else if #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))] {
        mod riscv;
        pub use self::riscv::*;
    } else if #[cfg(target_arch = "aarch64")]{
        mod aarch64;
        pub use self::aarch64::*;
    } else if #[cfg(any(target_arch = "loongarch64"))] {
        mod loongarch64;
        pub use self::loongarch64::*;
    }
}
