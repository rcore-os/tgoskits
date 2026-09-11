#![no_std]
#![no_main]
extern crate ax_std as std;

mod cache;

#[cfg(target_arch = "x86_64")]
mod ept;
#[cfg(target_arch = "x86_64")]
mod tlb;

#[unsafe(no_mangle)]
fn main() {
    use ax_cpu::{
        PhysAddr, VirtAddr,
        paging::{MappingFlags, PageTableEntry, Pte},
    };
    use ax_hal::paging::MapConfig;
    let mut flags = MappingFlags::READ | MappingFlags::WRITE;
    if !cfg!(all(target_arch = "aarch64", feature = "hv")) {
        flags |= MappingFlags::USER;
    }
    let source = PhysAddr::from_usize(0x4000_0000);
    let target = PhysAddr::from_usize(0x8000_0000);
    let address = VirtAddr::from_usize(0x4000_0000);
    // Exercise the CPU descriptor through the real generic walker and the
    // runtime allocator, including releasing the allocated intermediate tables.
    let mut table = ax_hal::paging::PageTable::new(ax_hal::paging::PagingAllocator).unwrap();
    table
        .map(&MapConfig {
            vaddr: address,
            paddr: source,
            size: 0x2000,
            pte: flags,
            allow_huge: false,
            flush: false,
        })
        .unwrap();
    let (physical, observed, size) = table.query(address + 0x321).unwrap();
    assert_eq!(physical, source + 0x321);
    assert_eq!(observed, flags);
    assert_eq!(size, 4096);
    table.unmap(address, 0x2000).unwrap();
    assert!(table.query(address).is_err());
    table
        .map(&MapConfig {
            vaddr: address,
            paddr: target,
            size: 0x2000,
            pte: observed,
            allow_huge: false,
            flush: false,
        })
        .unwrap();
    assert_eq!(table.query(address + 4096).unwrap().0, target + 4096);
    core::cfg_select! {
        target_arch = "x86_64" => {
            // Both requests use the architectural UC encoding on x86.
            let memory_types = [
                (MappingFlags::empty(), MappingFlags::empty()),
                (MappingFlags::DEVICE, MappingFlags::UNCACHED),
                (MappingFlags::UNCACHED, MappingFlags::UNCACHED),
            ];
        }
        target_arch = "riscv64" => {
            // The base Sv descriptor has no memory-type field. This image
            // does not enable a vendor memory-attribute extension.
            let memory_types = [(MappingFlags::empty(), MappingFlags::empty())];
        }
        _ => {
            let memory_types = [
                (MappingFlags::empty(), MappingFlags::empty()),
                (MappingFlags::DEVICE, MappingFlags::DEVICE),
                (MappingFlags::UNCACHED, MappingFlags::UNCACHED),
            ];
        }
    }
    for (requested, encoded) in memory_types {
        let access = MappingFlags::READ | MappingFlags::WRITE;
        let attributes = access | requested;
        let leaf = Pte::new_page(source, attributes, false);
        assert_eq!(leaf.config(false), access | encoded);
        assert_eq!(leaf.paddr(false), source);
    }
    table.unmap(address, 0x2000).unwrap();
    drop(table);
    #[cfg(target_arch = "x86_64")]
    tlb::run();
    #[cfg(target_arch = "x86_64")]
    ept::run();
    #[cfg(target_arch = "aarch64")]
    check_boot_permissions();
    #[cfg(target_arch = "aarch64")]
    check_stage2_permissions();
    #[cfg(target_arch = "aarch64")]
    check_native_regime();
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    check_boot_vectors();
    #[cfg(target_arch = "loongarch64")]
    check_boot_read_permission();
    #[cfg(all(target_arch = "loongarch64", feature = "boot-trap"))]
    check_boot_trap();
    cache::run();
    std::println!("CPU_PAGING_OK");
    std::process::exit(0);
}

#[cfg(target_arch = "loongarch64")]
fn check_boot_read_permission() {
    use someboot::{
        PageTableEntry, PhysAddr,
        mem::{PteConfig, mmu::ArchPte},
    };
    for read in [false, true] {
        let entry = ArchPte::new_page(
            PhysAddr::from_usize(0x4000_0000),
            PteConfig {
                read,
                ..PteConfig::default()
            },
            false,
        );
        assert!(entry.present());
        assert_eq!(
            entry.config(false).read,
            read,
            "boot read permission roundtrip"
        );
    }
}

#[cfg(target_arch = "aarch64")]
fn check_boot_permissions() {
    use someboot::{
        PageTableEntry, PhysAddr,
        mem::{PteConfig, mmu::ArchPte},
    };
    for writable in [false, true] {
        for executable in [false, true] {
            let requested = PteConfig {
                read: true,
                writable,
                executable,
                ..PteConfig::default()
            };
            let entry = ArchPte::new_page(PhysAddr::from_usize(0x4000_0000), requested, false);
            let observed = entry.config(false);
            #[cfg(feature = "hv")]
            {
                // SAFETY: both public descriptor types are repr(transparent)
                // over one initialized u64 and accept every descriptor bit pattern.
                // Decode boot output through the runtime CPU's EL2 interpretation.
                assert_eq!(
                    core::mem::size_of_val(&entry),
                    core::mem::size_of::<ax_cpu::paging::El2Pte>()
                );
                let runtime = unsafe {
                    core::ptr::read_unaligned(
                        (&entry as *const ArchPte).cast::<ax_cpu::paging::El2Pte>(),
                    )
                };
                assert_eq!(
                    runtime
                        .config(false)
                        .contains(ax_cpu::paging::MappingFlags::EXECUTE),
                    executable,
                    "boot descriptor must enforce the runtime EL2 execute permission"
                );
            }
            assert_eq!(
                observed.writable, writable,
                "boot write permission roundtrip"
            );
            assert_eq!(
                observed.executable, executable,
                "boot execute permission roundtrip"
            );
        }
    }
}

#[cfg(target_arch = "aarch64")]
fn check_stage2_permissions() {
    use ax_cpu::{
        PhysAddr,
        paging::{MappingFlags, PageTableEntry, Stage2Pte},
    };

    let physical = PhysAddr::from_usize(0x4000_0000);
    for writable in [false, true] {
        let mut requested = MappingFlags::READ;
        requested.set(MappingFlags::WRITE, writable);
        let entry = Stage2Pte::new_page(physical, requested, false);
        assert_eq!(
            entry.config(false),
            requested,
            "stage-2 write permission roundtrip"
        );
    }
    for memory in [
        MappingFlags::empty(),
        MappingFlags::DEVICE,
        MappingFlags::UNCACHED,
    ] {
        let requested = MappingFlags::READ | MappingFlags::WRITE | memory;
        let entry = Stage2Pte::new_page(physical, requested, false);
        assert_eq!(
            entry.config(false),
            requested,
            "stage-2 memory attribute roundtrip"
        );
    }
    for permissions in 0..8 {
        let requested = MappingFlags::from_bits_retain(permissions);
        let entry = Stage2Pte::new_page(physical, requested, false);
        assert_eq!(
            entry.present(),
            !requested.is_empty(),
            "stage-2 valid is independent of read permission"
        );
        assert_eq!(
            entry.config(false),
            requested,
            "stage-2 independent access permissions"
        );
    }
}

#[cfg(target_arch = "aarch64")]
fn check_native_regime() {
    use ax_cpu::paging::{El1Pte, El2Pte, MappingFlags, PageTableEntry, TableMeta};
    let root: u64;
    // SAFETY: this image has completed the selected privileged platform handoff.
    unsafe {
        if cfg!(feature = "hv") {
            assert_eq!(ax_cpu::registers::current_exception_level(), 2);
            core::arch::asm!("mrs {}, ttbr0_el2", out(reg) root, options(nostack));
        } else {
            assert_eq!(ax_cpu::registers::current_exception_level(), 1);
            core::arch::asm!("mrs {}, ttbr1_el1", out(reg) root, options(nostack));
        }
    }
    assert_eq!(
        ax_hal::KernelMmu::read_kernel_page_table().as_usize(),
        (root & 0x0000_ffff_ffff_f000) as usize
    );
    let tcr: u64;
    // SAFETY: read the active regime's translation configuration at its native EL.
    unsafe {
        if cfg!(feature = "hv") {
            core::arch::asm!("mrs {}, tcr_el2", out(reg) tcr, options(nomem, nostack));
        } else {
            core::arch::asm!("mrs {}, tcr_el1", out(reg) tcr, options(nomem, nostack));
        }
    }
    let configured_range = (tcr >> if cfg!(feature = "hv") { 16 } else { 32 }) & 7;
    let implemented_range = ax_cpu::capability::IdRegister::Mmfr0.read() & 15;
    assert!(
        configured_range <= implemented_range.min(5),
        "stage-one physical range exceeds this CPU or the 48-bit descriptor format"
    );
    let physical = ax_cpu::PhysAddr::from_usize(0x4000_0000);
    // Both formats remain available in both images. A feature must not change
    // the descriptor's permission interpretation.
    for execute in [false, true] {
        let mut flags = MappingFlags::READ | MappingFlags::USER;
        flags.set(MappingFlags::EXECUTE, execute);
        assert_eq!(
            El1Pte::new_page(physical, flags, false).config(false),
            flags
        );
        assert_eq!(
            El2Pte::new_page(physical, flags, false).config(false),
            flags & !MappingFlags::USER
        );
    }
    let address = ax_cpu::VirtAddr::from_usize(0x0000_8000_0000_0000);
    assert_eq!(
        ax_cpu::paging::El2PagingMeta::canonicalize_vaddr(address),
        address
    );
    if cfg!(feature = "hv") {
        assert_eq!(
            ax_hal::paging::ArchPagingMeta::canonicalize_vaddr(address),
            address
        );
    }
}

#[cfg(target_arch = "aarch64")]
fn check_boot_vectors() {
    let enabled = ax_cpu::interrupt::irqs_enabled();
    ax_cpu::interrupt::disable_irqs();
    let current = ax_cpu::registers::read_sp_el0();
    // SAFETY: this test temporarily installs the boot vectors on a mapped,
    // aligned runtime stack with IRQs masked. No guest owns the EL1 bank.
    // SVC returns at the following instruction with the saved PSTATE intact.
    unsafe {
        if cfg!(feature = "hv") {
            let saved_esr: u64;
            core::arch::asm!("mrs {}, esr_el1", out(reg) saved_esr, options(nostack));
            core::arch::asm!("msr esr_el1, xzr", options(nostack));
            ax_cpu::boot::El2::init_boot_trap();
            core::arch::asm!("svc #0", options(nostack));
            ax_cpu::boot::El2::init_trap();
            core::arch::asm!("msr esr_el1, {}", in(reg) saved_esr, options(nostack));
        } else {
            ax_cpu::boot::El1::init_boot_trap();
            core::arch::asm!("svc #0", options(nostack));
            ax_cpu::boot::El1::init_trap();
        }
    }
    assert_eq!(
        ax_cpu::registers::read_sp_el0(),
        current,
        "boot exception changed the CPU anchor"
    );
    assert!(!ax_cpu::interrupt::irqs_enabled());
    if enabled {
        ax_cpu::interrupt::enable_irqs();
    }
}

#[cfg(all(target_arch = "loongarch64", feature = "boot-trap"))]
fn check_boot_trap() {
    ax_cpu::interrupt::disable_irqs();
    // SAFETY: this integration test retains the actual boot policy, stack,
    // refill and CPU vectors. The deliberate break must reach its fatal
    // boot callback; this configuration expects that specific diagnostic.
    unsafe {
        ax_cpu::boot::install_boot_vectors(
            ax_cpu::boot::boot_vector(),
            ax_hal::mem::virt_to_phys(ax_cpu::boot::tlb_refill_entry().into()),
        );
        core::arch::asm!("break 0", options(nostack));
    }
    panic!("boot vector unexpectedly returned");
}

#[cfg(target_arch = "x86_64")]
fn check_boot_vectors() {
    let enabled = ax_cpu::interrupt::irqs_enabled();
    ax_cpu::interrupt::disable_irqs();
    let mut idtr = [0u8; 10];
    let vectors = ax_cpu::boot::BootVectorTable::new();
    // SAFETY: IRQ exclusion pins this kernel stack and preserves the runtime
    // IDT's mapping. SIDT/LIDT access exactly ten bytes. INT3 returns through
    // the real someboot callback, after which the runtime IDT is restored.
    unsafe {
        core::arch::asm!("sidt [{}]", in(reg) idtr.as_mut_ptr(), options(nostack));
        vectors.install();
        core::arch::asm!("int3");
        core::arch::asm!("lidt [{}]", in(reg) idtr.as_ptr(), options(nostack, readonly));
    }
    assert!(!ax_cpu::interrupt::irqs_enabled());
    if enabled {
        ax_cpu::interrupt::enable_irqs();
    }
}
