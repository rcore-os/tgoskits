//! Real IPI completion and legacy SBI fault checks in the vCPU runtime boot.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use ax_cpu::virtualization::{GuestBinding, Vcpu};

use super::*;
use crate::{
    AxVmResult, VmCreatePlan, VmManager,
    boot::{BootImageProvider, init_guest_boot_resources, prepare_guest_boot},
    config::{AxVCpuConfig, AxVMConfig, AxVMConfigParams, PhysCpuList, VMImageConfig},
    host::{HostMemory, arceos::arceos_host, task},
};

struct Page(crate::HostPhysAddr);

impl Page {
    fn new() -> Self {
        let page = Self(arceos_host().alloc_frame().expect("test page"));
        // SAFETY: this fresh, exclusively owned frame is one writable page.
        unsafe { page.pointer().write_bytes(0, 512) };
        page
    }

    fn pointer(&self) -> *mut usize {
        arceos_host().phys_to_virt(self.0).as_usize() as *mut usize
    }

    fn pte(&self) -> usize {
        self.0.as_usize() >> 2
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        arceos_host().dealloc_frame(self.0);
    }
}

fn wait_until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "RFENCE rendezvous timed out");
        task::yield_now();
    }
}

#[axtest::axtest]
fn remote_fence_retires_cached_vsstage_leaf_before_returning() {
    let _manager = VmManager::new().unwrap();
    let caller = task::builder("rfence-caller".into())
        .affinity(task::cpu_set_from_raw_bits(1))
        .spawn(check_remote_fence)
        .unwrap();
    caller.join().unwrap();
}

fn check_remote_fence() {
    let root = Page::new();
    let middle = Page::new();
    let leaf = Page::new();
    let old = Page::new();
    let new = Page::new();
    // SAFETY: all pages are initialized and exclusively owned. Only the leaf
    // changes after publication; no Rust references alias hardware accesses.
    unsafe {
        root.pointer().add(1).write(middle.pte() | 1);
        middle.pointer().write(leaf.pte() | 1);
        leaf.pointer().write(old.pte() | 0xc7);
        old.pointer().write(0x123);
        new.pointer().write(0x456);
    }
    let phase = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(AtomicUsize::new(0));
    let remote_phase = phase.clone();
    let remote_observed = observed.clone();
    let root_pa = root.0.as_usize();
    let remote = task::builder("rfence-cached-owner".into())
        .affinity(task::cpu_set_from_raw_bits(2))
        .spawn(move || {
            let mut state: Vcpu = Vcpu::default();
            state.initialize_supervisor();
            state.vs_csrs.vsatp = (8usize << 60) | (root_pa >> 12);
            let _pin = ax_std::os::arceos::guard::PreemptGuard::new();
            // SAFETY: this fixed CPU owns enabled H and its VS bank; the caller
            // retains every page until this thread retires the binding. IRQs
            // remain enabled so the production synchronous IPI can execute.
            unsafe {
                let binding = GuestBinding::load(&state).unwrap();
                let saved: usize;
                core::arch::asm!("csrrs {}, hstatus, {}", out(reg) saved, in(reg) 1usize << 8, options(nostack));
                let read = || {
                    let value: usize;
                    core::arch::asm!(".option push", ".option arch, +h", "hlv.d {}, ({})", ".option pop", out(reg) value, in(reg) 0x4000_0000usize, options(nostack));
                    value
                };
                assert_eq!(read(), 0x123);
                remote_phase.store(1, Ordering::Release);
                // No guest entry, CSR rewrite or scheduling may implicitly
                // invalidate this live VS translation between the two reads.
                while remote_phase.load(Ordering::Acquire) != 2 {
                    core::hint::spin_loop();
                }
                remote_observed.store(read(), Ordering::Release);
                core::arch::asm!("csrw hstatus, {}", in(reg) saved, options(nostack));
                binding.unload(&mut state);
            }
        })
        .unwrap();
    wait_until(|| phase.load(Ordering::Acquire) == 1);
    // SAFETY: retain the same VS tables and replace only the mapped leaf. The
    // remote processor has acknowledged caching the old mapping above.
    unsafe { leaf.pointer().write_volatile(new.pte() | 0xc7) };
    let topology = HartTopology::new(&[(0, Some(1), 8), (1, Some(2), 9)]);
    let result = handle(
        RiscvSbiCall {
            extension: rfnc::EID_RFNC,
            function: rfnc::REMOTE_SFENCE_VMA,
            arguments: [1, 9, 0x4000_0000, 4096, 0, 0],
        },
        &topology,
        crate::percpu::enabled_cpu_mask(),
        execute,
    );
    phase.store(2, Ordering::Release);
    remote.join().unwrap();
    assert_eq!(result, SbiRet::success(0));
    assert_eq!(
        observed.load(Ordering::Acquire),
        0x456,
        "remote VS leaf remained cached"
    );
}

core::arch::global_asm!(include_str!("legacy_fault.S"));

struct Images;

impl BootImageProvider for Images {
    fn read_file(&self, _path: &str) -> AxVmResult<Vec<u8>> {
        unsafe extern "C" {
            static axvm_legacy_fault_start: u8;
            static axvm_legacy_fault_end: u8;
        }
        let start = core::ptr::addr_of!(axvm_legacy_fault_start);
        let length = core::ptr::addr_of!(axvm_legacy_fault_end).addr() - start.addr();
        // SAFETY: labels delimit one immutable, initialized assembly section;
        // only the copied bytes are later executed in the guest address space.
        Ok(unsafe { core::slice::from_raw_parts(start, length) }.to_vec())
    }
}

#[axtest::axtest]
fn legacy_remote_fence_redirects_mask_faults_to_the_ecall() {
    const ENTRY: usize = 0x4000_0000;
    const MAILBOX: usize = ENTRY + 0x10000;
    init_guest_boot_resources();
    let manager = VmManager::new().unwrap();
    let images = Arc::new(Images);
    let guest = axvmconfig::GuestConfig::from_toml(
        r#"[base]
id = 98
name = 'legacy-fence-fault'
cpu_num = 1
phys_cpu_ids = [8]
phys_cpu_sets = [1]
[kernel]
kernel_path = 'fault.bin'
entry_point = 0x40000000
kernel_load_addr = 0x40000000
dtb_load_addr = 0x40e00000
memory_regions = [[0x40000000, 0x1000000, 7, 0]]
"#,
    )
    .unwrap();
    let mut catalog = crate::ConfiguredDeviceCatalog::new();
    crate::machine::register_devices(&mut catalog).unwrap();
    let mut config = AxVMConfig::new(AxVMConfigParams {
        id: 98,
        name: guest.base.name.clone(),
        phys_cpu_ls: PhysCpuList::new(1, Some(vec![8]), Some(vec![1])),
        cpu_config: AxVCpuConfig {
            bsp_entry: ENTRY.into(),
            ap_entry: ENTRY.into(),
        },
        image_config: VMImageConfig {
            kernel_load_gpa: ENTRY.into(),
            loaded_from_filesystem: true,
            dtb_load_gpa: Some(0x40e0_0000.into()),
            ..Default::default()
        },
        memory_regions: guest.kernel.memory_regions.clone(),
        virtual_device_catalog: Arc::new(catalog),
        ..Default::default()
    });
    let boot = prepare_guest_boot(&mut config, guest, &*images).unwrap();
    config.set_memory_regions(boot.config().kernel.memory_regions.clone());
    let vm = manager
        .create(VmCreatePlan {
            config,
            boot,
            images,
            vcpu_schedule_policy: crate::SchedulePolicy::default(),
            cpu_freq_backend: None,
        })
        .unwrap()
        .wait()
        .unwrap();
    let run = vm.start().unwrap().wait().unwrap();
    let memory = vm.guest_memory(run).unwrap();
    let mut result = [0; 8];
    wait_until(|| {
        memory.read_bytes(MAILBOX.into(), &mut result).unwrap();
        usize::from_le_bytes(result) != 0
    });
    vm.stop(crate::StopReason::Clean).unwrap().wait().unwrap();
    vm.destroy().unwrap().wait().unwrap();
    assert_eq!(
        usize::from_le_bytes(result),
        1,
        "legacy mask fault was not redirected"
    );
}
