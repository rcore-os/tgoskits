//! Software identity carried by an address-space activation lease.

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
    pub const fn root(self) -> ax_memory_addr::PhysAddr {
        self.root
    }

    /// Projects the hardware state without transferring the activation lease.
    pub const fn hardware(self) -> ax_cpu::mmu::HardwareAddressSpace {
        ax_cpu::mmu::HardwareAddressSpace::new(self.root, self.hardware_tag)
    }
}

impl Default for InstalledAddressSpace {
    fn default() -> Self {
        Self::kernel(ax_memory_addr::PhysAddr::from_usize(0))
    }
}
