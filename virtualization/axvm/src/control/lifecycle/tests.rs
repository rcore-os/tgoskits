//! Real-kernel lifecycle regressions sharing the vCPU runtime QEMU boot.

use std::{
    sync::{Mutex, Weak},
    time::{Duration, Instant},
};

use super::*;
use crate::{
    VmCreatePlan, VmHandle, VmManager,
    boot::{BootImageProvider, init_guest_boot_resources, prepare_guest_boot},
    config::{
        AxVCpuConfig, AxVMConfig, AxVMConfigParams, HostAddressAssignment, PhysCpuList,
        VMImageConfig,
    },
};

const FAILED_VM: usize = 99;
const ENTRY: usize = 0x4000_0000;
const MAILBOX: usize = ENTRY + 0x1000;
static FAIL_START: AtomicBool = AtomicBool::new(false);
static FAILED_SIGNALS: Mutex<Option<Weak<RunSignals>>> = Mutex::new(None);

pub(super) fn after_start(owner: &Owner) -> AxVmResult {
    if owner.vm.id() == FAILED_VM && FAIL_START.swap(false, Ordering::AcqRel) {
        let run = owner.run.as_ref().expect("fault after vCPU startup");
        assert!(run.participants.values().all(|member| member.started));
        *FAILED_SIGNALS.lock().unwrap() = Some(Arc::downgrade(&run.signals));
        return Err(AxVmError::invalid_state(
            "test startup",
            "injected failure after vCPU startup",
        ));
    }
    Ok(())
}

// Position-independent, stackless guest. CPU0 starts CPU1 when requested,
// publishes readiness in RAM and invokes PSCI SYSTEM_RESET on host command.
// The second CPU keeps executing until the lifecycle owner stops the whole VM.
core::arch::global_asm!(
    ".pushsection .rodata.axvm_lifecycle_guest, \"a\"",
    ".balign 4",
    ".global axvm_lifecycle_guest_start",
    "axvm_lifecycle_guest_start:",
    "mov x4, #2", // Patched to the requested CPU count in Images::read_file.
    "cmp x4, #1",
    "b.eq 1f",
    "mov x0, #3",
    "movk x0, #0xc400, lsl #16", // PSCI CPU_ON (64-bit).
    "mov x1, #1",
    "adr x2, 2f",
    "mov x3, #0",
    "hvc #0",
    "1: mov x5, #0",
    "b 3f",
    "2: mov x5, #1",
    "3: mov x6, #0x1000",
    "movk x6, #0x4000, lsl #16",
    "mov w7, #1",
    "add x8, x6, x5, lsl #2",
    "stlr w7, [x8]",
    "cbnz x5, 5f",
    "add x8, x6, #8",
    "4: ldar w7, [x8]",
    "cbz w7, 4b",
    "mov w0, #9",
    "movk w0, #0x8400, lsl #16", // PSCI SYSTEM_RESET.
    "hvc #0",
    "str w7, [x6, #12]", // A nonreturning reset must never reach this store.
    "5: b 5b",
    ".global axvm_lifecycle_guest_end",
    "axvm_lifecycle_guest_end:",
    ".popsection",
);

struct Images(usize);

impl BootImageProvider for Images {
    fn read_file(&self, _path: &str) -> AxVmResult<Vec<u8>> {
        unsafe extern "C" {
            static axvm_lifecycle_guest_start: u8;
            static axvm_lifecycle_guest_end: u8;
        }
        let start = core::ptr::addr_of!(axvm_lifecycle_guest_start);
        let end = core::ptr::addr_of!(axvm_lifecycle_guest_end);
        let length = end.addr() - start.addr();
        // SAFETY: both labels delimit the single immutable assembly section
        // above, retained by these references for the host kernel lifetime.
        // Copying its initialized bytes does not execute or modify host code.
        let mut image = unsafe { core::slice::from_raw_parts(start, length) }.to_vec();
        let cpu_count_instruction = 0xd280_0004 | ((self.0 as u32) << 5);
        image[..4].copy_from_slice(&cpu_count_instruction.to_le_bytes());
        Ok(image)
    }
}

fn create(id: usize, cpus: usize) -> (VmManager, VmHandle) {
    init_guest_boot_resources();
    let images = Arc::new(Images(cpus));
    let cpu_ids = (0..cpus).collect::<Vec<_>>();
    let guest = axvmconfig::GuestConfig::from_toml(&format!(
        "[base]\nid = {id}\nname = 'lifecycle-dma'\nguest_type = 'virtualized'\ncpu_num = \
         {cpus}\nphys_cpu_ids = {cpu_ids:?}\n[kernel]\nkernel_path = 'loop.bin'\nentry_point = \
         {ENTRY}\nkernel_load_addr = {ENTRY}\ndtb_load_addr = 0x40e00000\nmemory_regions = \
         [[{ENTRY}, 0x01000000, 7, 0]]\n"
    ))
    .unwrap();
    let mut catalog = crate::ConfiguredDeviceCatalog::new();
    crate::machine::register_devices(&mut catalog).unwrap();
    let mut config = AxVMConfig::new(AxVMConfigParams {
        id,
        name: guest.base.name.clone(),
        phys_cpu_ls: PhysCpuList::new(cpus, Some((0..cpus).collect()), None),
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
        // QEMU virt's empty virtio-mmio slot: no real device can DMA here.
        // Its identity mapping exercises the normal physical-DMA ownership gate.
        pass_through_addresses: vec![HostAddressAssignment {
            base_gpa: 0x0a00_3000,
            length: 0x1000,
        }],
        virtual_device_catalog: Arc::new(catalog),
        ..Default::default()
    });
    let boot = prepare_guest_boot(&mut config, guest, &*images).unwrap();
    config.set_memory_regions(boot.config().kernel.memory_regions.clone());
    let manager = VmManager::new().unwrap();
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
    (manager, vm)
}

fn assert_retained(before: &crate::VmSnapshot, after: &crate::VmSnapshot) {
    assert_eq!(
        after.memory.nested_page_table_root,
        before.memory.nested_page_table_root
    );
    assert_eq!(after.memory.total_bytes, before.memory.total_bytes);
    assert_eq!(after.memory.regions.len(), before.memory.regions.len());
    for (old, new) in before.memory.regions.iter().zip(&after.memory.regions) {
        assert_eq!(old.hva, new.hva, "DMA backing was replaced");
    }
    assert!(
        after.vcpu.iter().all(|cpu| cpu.host_base_policy.is_none()),
        "retained run still owns vCPU tasks"
    );
    assert_eq!(after.cpu.running_vcpu_count, 0);
}

mod cases {
    use super::*;

    #[axtest::axtest]
    fn failed_start_joins_execution_and_retains_dma_backing() {
        let (manager, vm) = create(FAILED_VM, 1);
        let before = vm.snapshot();
        FAIL_START.store(true, Ordering::Release);
        assert!(vm.start().unwrap().wait().is_err());
        let after = vm.snapshot();
        assert_retained(&before, &after);
        let signals = FAILED_SIGNALS
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .upgrade()
            .expect("DMA run retained");
        assert!(signals.interrupts_closed());
        assert!(signals.interrupts_quiet());
        assert_eq!(after.state, VmStatus::Failed);
        assert!(matches!(
            after.last_failure,
            Some(AxVmError::InvalidState {
                operation: "test startup",
                ..
            })
        ));
        assert!(
            vm.destroy().unwrap().wait().is_err(),
            "retained DMA ownership was discarded"
        );
        // The production contract deliberately retains the reservation and
        // backing until host reset. This QEMU boot owns their remaining lifetime.
        std::mem::forget(manager);
    }

    #[axtest::axtest]
    fn guest_reset_stops_all_cpus_and_retains_dma_backing() {
        for cpus in [1, 2] {
            let (manager, vm) = create(100 + cpus, cpus);
            let run = vm.start().unwrap().wait().unwrap();
            let memory = vm.guest_memory(run).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let mut ready = [0; 8];
                memory.read_bytes(MAILBOX.into(), &mut ready).unwrap();
                if (0..cpus).all(|cpu| ready[cpu * 4..cpu * 4 + 4] == 1u32.to_le_bytes())
                    && vm.snapshot().cpu.running_vcpu_count == cpus
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "guest CPUs did not start: {:?}",
                    vm.snapshot()
                );
                crate::host::task::yield_now();
            }
            let before = vm.snapshot();
            assert_eq!(before.cpu.running_vcpu_count, cpus);
            memory
                .write_bytes((MAILBOX + 8).into(), &1u32.to_le_bytes())
                .unwrap();
            while vm.snapshot().state != VmStatus::Stopped {
                assert!(
                    Instant::now() < deadline,
                    "guest reset stranded a partial VM: {:?}",
                    vm.snapshot()
                );
                crate::host::task::yield_now();
            }
            let after = vm.snapshot();
            assert_retained(&before, &after);
            assert!(after.last_failure.is_none());
            assert_eq!(after.run, Some(run), "DMA guest was restarted");
            assert!(memory.read_bytes(MAILBOX.into(), &mut [0; 4]).is_err());
            assert!(vm.start().unwrap().wait().is_err());
            assert!(vm.destroy().unwrap().wait().is_err());
            std::mem::forget(manager);
        }
    }
}
