//! LVZ translation and register transitions without VM policy.

use core::{marker::PhantomData, mem::offset_of};

use super::GuestContext;
use crate::{
    PhysAddr, VirtAddr,
    registers::{read_csr as read, write_csr as write},
    virtualization::VirtualizationError,
};

unsafe extern "C" {
    fn __ax_cpu_lvz_run_guest();
    fn __ax_cpu_lvz_guest_tlb_refill_vector();
    static __ax_cpu_lvz_exception_vectors: u8;
}

/// Running addresses of the three LVZ entries.
///
/// The owner translates each address into its executable direct-map alias
/// before creating [`EntryAddresses`]. Address-space layout belongs to the host.
pub fn entry_addresses() -> [VirtAddr; 3] {
    [
        VirtAddr::from_usize(__ax_cpu_lvz_run_guest as *const () as usize),
        VirtAddr::from_usize(__ax_cpu_lvz_guest_tlb_refill_vector as *const () as usize),
        VirtAddr::from_usize(core::ptr::addr_of!(__ax_cpu_lvz_exception_vectors) as usize),
    ]
}

/// Executable aliases that remain accessible under both host and guest roots.
#[derive(Clone, Copy, Debug)]
pub struct EntryAddresses {
    run: usize,
    refill: usize,
    vectors: usize,
}

impl EntryAddresses {
    /// Installs direct-map aliases corresponding to [`entry_addresses`].
    ///
    /// # Safety
    /// Each address must refer to the corresponding CPU entry, remain executable
    /// in direct-address mode, and stay valid throughout every associated binding.
    pub unsafe fn new(addresses: [VirtAddr; 3]) -> Result<Self, VirtualizationError> {
        let [run, refill, vectors] = addresses.map(VirtAddr::as_usize);
        if refill & 4095 != 0 || vectors & 4095 != 0 || run & 3 != 0 {
            return Err(VirtualizationError::InvalidVector);
        }
        Ok(Self {
            run,
            refill,
            vectors,
        })
    }
}

/// The vector class that returned control to the host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitKind {
    /// Synchronous exception, hypercall or nested translation fault.
    Synchronous,
    /// Local interrupt.
    Irq,
}

/// Machine exit information copied before host dispatch can change CSRs.
#[derive(Clone, Copy, Debug)]
pub struct Exit {
    /// Vector class selected by entry assembly.
    pub kind: ExitKind,
    /// Saved host ESTAT, including exception and interrupt status.
    pub status: usize,
    /// Saved faulting instruction.
    pub instruction: usize,
    /// Saved faulting guest PC.
    pub pc: usize,
}

#[derive(Debug)]
struct HostState {
    crmd: usize,
    prmd: usize,
    ksp: usize,
    gstat: usize,
    gctl: usize,
    gtlbc: usize,
    gintc: usize,
    vcpu_scratch: usize,
    temp_scratch: usize,
    debug_scratch: usize,
}

pub(super) struct LocalIrqs(bool);

impl LocalIrqs {
    pub(super) fn disable() -> Self {
        let enabled = crate::interrupt::irqs_enabled();
        crate::interrupt::disable_irqs();
        Self(enabled)
    }
}

impl Drop for LocalIrqs {
    fn drop(&mut self) {
        if self.0 {
            crate::interrupt::enable_irqs();
        }
    }
}

/// Register image and local binding for one LVZ execution context.
///
/// An active binding is CPU-local and must be explicitly released before its
/// storage is reclaimed. The unsafe binding contract excludes migration and
/// concurrent register-bank access; the object is neither Send nor Sync.
#[repr(C)]
#[derive(Debug)]
pub struct Vcpu {
    /// Saved guest integer and control registers.
    pub context: GuestContext,
    host_stack: usize,
    root: PhysAddr,
    entry: usize,
    host: Option<HostState>,
    local: PhantomData<*mut ()>,
    /// Guest floating-point, LSX/LASX, condition and control registers.
    pub fp: crate::registers::FpuState,
    pub(crate) host_fp: crate::registers::FpuState,
}

impl Default for Vcpu {
    fn default() -> Self {
        Self {
            context: GuestContext::default(),
            host_stack: 0,
            root: PhysAddr::from_usize(0),
            entry: 0,
            host: None,
            local: PhantomData,
            fp: crate::registers::FpuState::default(),
            host_fp: crate::registers::FpuState::default(),
        }
    }
}

impl Vcpu {
    /// Selects a four-level, 4 KiB nested page-table root.
    pub fn set_root(&mut self, root: PhysAddr) -> Result<(), VirtualizationError> {
        if root.as_usize() & 4095 != 0 {
            return Err(VirtualizationError::InvalidRoot);
        }
        self.root = root;
        Ok(())
    }

    /// Borrows the current CPU's LVZ register bank until [`Self::unbind`].
    ///
    /// # Safety
    /// The caller must hold exclusive, non-migrating ownership of this CPU's LVZ
    /// bank until unbind, retain this object and all entry aliases,
    /// and ensure the selected root and guest memory remain hardware-accessible.
    pub unsafe fn bind(&mut self, entries: EntryAddresses) -> Result<(), VirtualizationError> {
        let _irqs = LocalIrqs::disable();
        if self.host.is_some() {
            return Err(VirtualizationError::AlreadyEnabled);
        }
        // SAFETY: the caller exclusively owns the local CSR bank for this binding.
        unsafe {
            self.host = Some(HostState {
                crmd: read::<0>(),
                prmd: read::<1>(),
                ksp: read::<0x30>(),
                gstat: read::<0x50>(),
                gctl: read::<0x51>(),
                gtlbc: read::<0x15>(),
                gintc: read::<0x52>(),
                vcpu_scratch: read::<0x34>(),
                temp_scratch: read::<0x35>(),
                debug_scratch: read::<0x502>(),
            });
            self.context.host_pgdl = read::<0x19>();
            self.context.host_pgdh = read::<0x1a>();
            self.context.host_pwcl = read::<0x1c>();
            self.context.host_pwch = read::<0x1d>();
            self.context.host_stlbps = read::<0x1e>();
            self.context.host_tlbrentry = read::<0x88>();
            self.context.host_asid = read::<0x18>();
            self.context.host_eentry = read::<0xc>();
            self.context.host_ecfg = read::<4>();
        }
        self.context.guest_tlbrentry = entries.refill;
        self.context.guest_eentry = entries.vectors;
        self.entry = entries.run;
        // SAFETY: this binding owns the local bank; physical IRQ passthrough
        // starts disabled until its platform owner explicitly assigns inputs.
        unsafe { super::set_hwi_passthrough(0) };
        Ok(())
    }

    /// Enters the selected guest and restores the host translation before returning.
    ///
    /// # Safety
    /// The binding contract must still hold, the guest ID must be unique among
    /// locally cached guests, interrupts must be disabled, and the guest must not
    /// access host-owned memory. `state_address` must be an exclusive direct-map
    /// alias of this entire object, accessible while the nested root is installed.
    pub unsafe fn run(
        &mut self,
        guest_id: u8,
        state_address: VirtAddr,
    ) -> Result<Exit, VirtualizationError> {
        if self.host.is_none() {
            return Err(VirtualizationError::NotEnabled);
        }
        // SAFETY: the live binding excludes concurrent CSR access and migration.
        let kind = unsafe {
            update::<0x50>(0xff0004, ((guest_id as usize) << 16) | 4);
            update::<0x15>(0xff1000, ((guest_id as usize) << 16) | 0x1000);
            // Preserve the existing LVZ interception configuration: guest CSR,
            // CPUCFG and IOCSR traps are handled by the VM owner after exit.
            update::<0x51>(
                (3 << 4)
                    | (1 << 7)
                    | (1 << 9)
                    | (1 << 11)
                    | (1 << 13)
                    | (1 << 15)
                    | (3 << 20)
                    | (7 << 24),
                (1 << 4) | (2 << 20),
            );
            super::invalidate_guest_translations();
            update::<1>(4, 4);
            run_guest(state_address.as_usize() as *mut core::ffi::c_void)
        };
        Ok(Exit {
            kind: match kind {
                0 => ExitKind::Synchronous,
                1 => ExitKind::Irq,
                _ => unreachable!("LVZ entry returned an invalid vector class"),
            },
            status: self.context.host_estat,
            instruction: self.context.host_badi,
            pc: self.context.guest_exception_pc(),
        })
    }

    /// Restores the local CSR bank and ends the binding.
    ///
    /// # Safety
    /// This must execute on the bound CPU with interrupts disabled and no guest
    /// running. The caller must keep the binding's storage alive through return.
    pub unsafe fn unbind(&mut self) -> Result<(), VirtualizationError> {
        let _irqs = LocalIrqs::disable();
        let host = self.host.take().ok_or(VirtualizationError::NotEnabled)?;
        // SAFETY: this is the exclusive owner restoring its saved local bank.
        unsafe {
            write::<0xc>(self.context.host_eentry);
            write::<4>(self.context.host_ecfg);
            write::<0x19>(self.context.host_pgdl);
            write::<0x1a>(self.context.host_pgdh);
            write::<0x1c>(self.context.host_pwcl);
            write::<0x1d>(self.context.host_pwch);
            write::<0x1e>(self.context.host_stlbps);
            write::<0x88>(self.context.host_tlbrentry);
            write::<0x18>(self.context.host_asid);
            write::<0x30>(host.ksp);
            write::<0x50>(host.gstat);
            write::<0x51>(host.gctl);
            write::<0x15>(host.gtlbc);
            write::<0x52>(host.gintc);
            write::<0x34>(host.vcpu_scratch);
            write::<0x35>(host.temp_scratch);
            write::<0x502>(host.debug_scratch);
            write::<1>(host.prmd);
            core::arch::asm!("invtlb 0x0, $r0, $r0");
            write::<0>(host.crmd);
        }
        Ok(())
    }
}

unsafe fn update<const CSR: u16>(mask: usize, value: usize) {
    // SAFETY: the binding excludes interruption of this local read-modify-write.
    unsafe { write::<CSR>((read::<CSR>() & !mask) | value) };
}

#[unsafe(naked)]
unsafe extern "C" fn run_guest(_vcpu: *mut core::ffi::c_void) -> usize {
    core::arch::naked_asm!(
        "addi.d $sp, $sp, -14 * 8",
        "st.d $ra, $sp, 0",
        "st.d $s0, $sp, 8",
        "st.d $s1, $sp, 16",
        "st.d $s2, $sp, 24",
        "st.d $s3, $sp, 32",
        "st.d $s4, $sp, 40",
        "st.d $s5, $sp, 48",
        "st.d $s6, $sp, 56",
        "st.d $s7, $sp, 64",
        "st.d $s8, $sp, 72",
        "st.d $fp, $sp, 80",
        "st.d $tp, $sp, 88",
        "st.d $sp, $a0, {host_stack}",
        "move $s0, $a0",
        "li.d $a0, {host_fp}",
        "add.d $a0, $s0, $a0",
        "bl {save_fp}",
        "li.d $a0, {guest_fp}",
        "add.d $a0, $s0, $a0",
        "bl {restore_fp}",
        "move $a0, $s0",
        "ld.d $t0, $a0, {entry}",
        "ld.d $a1, $a0, {root}",
        "jirl $zero, $t0, 0",
        host_fp = const offset_of!(Vcpu, host_fp),
        guest_fp = const offset_of!(Vcpu, fp),
        save_fp = sym super::super::fp::save_fp_registers,
        restore_fp = sym super::super::fp::restore_fp_registers,
        host_stack = const offset_of!(Vcpu, host_stack),
        entry = const offset_of!(Vcpu, entry),
        root = const offset_of!(Vcpu, root),
    );
}
