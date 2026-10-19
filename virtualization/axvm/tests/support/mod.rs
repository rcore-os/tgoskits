//! Real VM assembly and bounded console observation shared by the kernel tests.

use std::{
    fs,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use axvm::{
    AxVmError, AxVmResult, ConfiguredDeviceCatalog, GuestPhysAddr, SchedulePolicy, SerialBackend,
    SerialBackendFactory, StopReason, VmCreatePlan, VmHandle, VmManager, VmSnapshot, VmStatus,
    boot::{BootImageProvider, boot_firmware_load_gpa, guest_boot_policy, prepare_guest_boot},
    config::{AxVCpuConfig, AxVMConfig, AxVMConfigParams, PhysCpuList, RamdiskInfo, VMImageConfig},
};
use axvmconfig::{GuestConfig, GuestType, HostDeviceAssignment};

const POLL: Duration = Duration::from_millis(1);
const MAX_CONSOLE_BYTES: usize = 2 * 1024 * 1024;
const CONSOLE_QUEUE_BYTES: usize = 64 * 1024;

pub fn case(result: Result<()>) -> axtest::AxTestResult {
    match result {
        Ok(()) => axtest::AxTestResult::Ok,
        Err(error) => {
            println!("AXVM_TEST_FAILED {error:#}");
            axtest::AxTestResult::Failed
        }
    }
}

/// Owns the manager until all vCPU and control tasks have been joined, including
/// an error returned by the workload or during partial VM creation.
pub fn with_vm(
    config_path: &str,
    policy: SchedulePolicy,
    workload: impl FnOnce(&mut Fixture) -> Result<()>,
) -> Result<()> {
    axvm::boot::init_guest_boot_resources();
    let manager = VmManager::new().context("enable host virtualization")?;
    let result = (|| {
        let serial = Arc::new(ConsoleFactory::default());
        let plan = prepare_plan(config_path, policy, serial.clone())?;
        let vm = manager.create(plan)?.wait().context("create VM")?;
        let mut fixture = Fixture {
            vm,
            serial,
            console_generation: None,
            transcript: String::new(),
            pending_line: String::new(),
            scratch: Vec::with_capacity(CONSOLE_QUEUE_BYTES),
        };
        ensure!(
            fixture
                .vm
                .snapshot()
                .vcpu
                .iter()
                .all(|cpu| cpu.host_base_policy.is_none()),
            "an inactive VM reports a host task"
        );
        let result = workload(&mut fixture);
        fixture.flush_console()?;
        result
    })();
    let cleanup = manager
        .shutdown()
        .context("destroy VMs and join control tasks");
    // Always observe cleanup even when the workload failed. A guest marker can
    // never finish the axtest case before this join has completed.
    if let Err(error) = cleanup {
        return Err(error.context(format!("workload outcome: {result:?}")));
    }
    ensure!(
        manager.list().is_empty(),
        "VM registry retained a destroyed instance"
    );
    result
}

struct Images;

impl BootImageProvider for Images {
    fn read_file(&self, path: &str) -> AxVmResult<Vec<u8>> {
        fs::read(path).map_err(|error| AxVmError::Boot {
            operation: "read test guest image",
            detail: format!("{path}: {error}"),
        })
    }
}

fn prepare_plan(
    path: &str,
    policy: SchedulePolicy,
    serial: Arc<ConsoleFactory>,
) -> Result<VmCreatePlan> {
    let raw = fs::read_to_string(path).with_context(|| format!("read guest config {path}"))?;
    let guest = GuestConfig::from_toml(&raw).context("parse guest configuration")?;
    let machine = axvm::machine::current_machine_profile(guest.base.cpu_num);
    let mut passthrough = guest.devices.unresolved_host_devices();
    if guest.base.guest_type == GuestType::Passthrough
        && passthrough.is_empty()
        && let Some(path) = machine.default_passthrough_device_path
    {
        passthrough.push(HostDeviceAssignment {
            name: path.into(),
            ..Default::default()
        });
    }
    let mut catalog = ConfiguredDeviceCatalog::new();
    axvm::machine::register_devices(&mut catalog)?;
    let mut config = AxVMConfig::new(AxVMConfigParams {
        id: guest.base.id,
        name: guest.base.name.clone(),
        phys_cpu_ls: PhysCpuList::new(
            guest.base.cpu_num,
            guest.base.phys_cpu_ids.clone(),
            guest.base.phys_cpu_sets.clone(),
        ),
        cpu_config: AxVCpuConfig {
            bsp_entry: GuestPhysAddr::from(guest.kernel.entry_point),
            ap_entry: GuestPhysAddr::from(guest.kernel.entry_point),
        },
        image_config: VMImageConfig {
            kernel_load_gpa: GuestPhysAddr::from(guest.kernel.kernel_load_addr),
            loaded_from_filesystem: true,
            bios_load_gpa: boot_firmware_load_gpa(&guest),
            dtb_load_gpa: guest.kernel.dtb_load_addr.map(GuestPhysAddr::from),
            ramdisk: guest.kernel.ramdisk_load_addr.map(|address| RamdiskInfo {
                load_gpa: GuestPhysAddr::from(address),
                size: None,
            }),
        },
        pass_through_devices: passthrough,
        excluded_devices: guest.devices.disabled_device_paths(),
        address_space_policy: guest.base.guest_type.address_space_policy(),
        memory_regions: guest.kernel.memory_regions.clone(),
        serial_profile: Some(machine.serial),
        serial_backend_factory: Some(serial),
        virtual_device_requests: guest.devices.virtual_device_requests().to_vec(),
        virtual_device_catalog: Arc::new(catalog),
        ..Default::default()
    });
    let boot = prepare_guest_boot(&mut config, guest, &Images).context("prepare guest boot")?;
    config.set_memory_regions(boot.config().kernel.memory_regions.clone());
    config.set_boot_policy(guest_boot_policy(boot.config(), &Images));
    Ok(VmCreatePlan {
        config,
        boot,
        images: Arc::new(Images),
        vcpu_schedule_policy: policy,
        cpu_freq_backend: None,
    })
}

#[derive(Debug)]
struct ConsoleQueue {
    bytes: Vec<u8>,
    overflowed: bool,
}

#[derive(Debug)]
struct ConsoleBuffer(ax_sync::RawSpinLock<ConsoleQueue>);

impl Default for ConsoleBuffer {
    fn default() -> Self {
        Self(ax_sync::RawSpinLock::new(ConsoleQueue {
            bytes: Vec::with_capacity(CONSOLE_QUEUE_BYTES),
            overflowed: false,
        }))
    }
}

impl SerialBackend for ConsoleBuffer {
    fn write(&self, bytes: &[u8]) {
        // UART callbacks may run with preemption/IRQs disabled. The queue is
        // allocated at graph construction and never grows in this callback.
        let mut queue = self.0.lock_irqsave();
        if bytes.len() <= CONSOLE_QUEUE_BYTES - queue.bytes.len() {
            queue.bytes.extend_from_slice(bytes);
        } else {
            queue.overflowed = true;
        }
    }

    fn read(&self, _buffer: &mut [u8]) -> usize {
        0
    }
}

#[derive(Debug, Default)]
struct ConsoleFactory(Mutex<Option<Arc<ConsoleBuffer>>>);

impl SerialBackendFactory for ConsoleFactory {
    fn create(&self) -> Arc<dyn SerialBackend> {
        let buffer = Arc::new(ConsoleBuffer::default());
        *self.0.lock().unwrap() = Some(buffer.clone());
        buffer
    }
}

pub struct Fixture {
    pub vm: VmHandle,
    serial: Arc<ConsoleFactory>,
    console_generation: Option<Arc<ConsoleBuffer>>,
    transcript: String,
    pending_line: String,
    scratch: Vec<u8>,
}

impl Fixture {
    pub fn flush_console(&mut self) -> Result<()> {
        let current = self.serial.0.lock().unwrap().clone();
        if let Some(current) = current {
            if self
                .console_generation
                .as_ref()
                .is_none_or(|old| !Arc::ptr_eq(old, &current))
            {
                self.transcript.clear();
                self.pending_line.clear();
                self.console_generation = Some(current.clone());
            }
            self.scratch.clear();
            let overflowed = {
                let mut queue = current.0.lock_irqsave();
                self.scratch.extend_from_slice(&queue.bytes);
                queue.bytes.clear();
                queue.overflowed
            };
            ensure!(!overflowed, "guest console queue overflowed");
            let bytes = &self.scratch;
            ensure!(
                self.transcript.len().saturating_add(bytes.len()) <= MAX_CONSOLE_BYTES,
                "guest exceeded bounded console capture"
            );
            let text = String::from_utf8_lossy(&bytes);
            self.transcript.push_str(&text);
            self.pending_line.push_str(&text);
            while let Some(end) = self.pending_line.find('\n') {
                let line: String = self.pending_line.drain(..=end).collect();
                print!("{line}");
            }
        }
        for failure in [
            "SOFTWARE VIRQ FAIL",
            "VCPU_PERF_FAIL",
            "GPIO_SET_ERROR",
            "BENCHER_GPIO_FAILED",
            "BENCHER_FIFO_FAILED",
            "MEASUREMENT_FAILED",
            "panicked at",
            "Kernel panic",
            "Unhandled exception",
        ] {
            ensure!(
                !self.transcript.contains(failure),
                "guest reported {failure}"
            );
        }
        Ok(())
    }

    pub fn wait(
        &mut self,
        timeout: Duration,
        condition: impl Fn(&VmSnapshot, &str) -> bool,
    ) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            self.flush_console()?;
            let snapshot = self.vm.snapshot();
            ensure!(
                snapshot.last_failure.is_none(),
                "VM failed: {:?}",
                snapshot.last_failure
            );
            if condition(&snapshot, &self.transcript) {
                return Ok(());
            }
            ensure!(
                !matches!(snapshot.state, VmStatus::Stopped | VmStatus::Destroyed),
                "guest stopped before its completion condition"
            );
            if Instant::now() >= deadline {
                bail!(
                    "guest condition timed out: state={:?} vcpus={:?}",
                    snapshot.state,
                    snapshot.vcpu
                );
            }
            thread::sleep(POLL);
        }
    }

    pub fn validate_console(&self, validate: impl FnOnce(&str) -> Result<()>) -> Result<()> {
        validate(&self.transcript)
    }

    pub fn check_policy(&self, policy: SchedulePolicy, active: usize) -> Result<()> {
        let snapshot = self.vm.snapshot();
        ensure!(
            snapshot.description.vcpu_schedule_policy == policy,
            "requested policy changed"
        );
        let policies: Vec<_> = snapshot
            .vcpu
            .iter()
            .filter_map(|cpu| cpu.host_base_policy)
            .collect();
        ensure!(
            policies.len() == active,
            "expected {active} owner tasks, observed {policies:?}"
        );
        ensure!(
            policies.iter().all(|actual| *actual == policy),
            "host task policy mismatch: {policies:?}"
        );
        Ok(())
    }

    pub fn stop(&mut self) -> Result<()> {
        self.vm
            .stop(StopReason::Clean)?
            .wait()
            .context("stop guest and join vCPU tasks")?;
        self.flush_console()?;
        let snapshot = self.vm.snapshot();
        ensure!(snapshot.state == VmStatus::Stopped, "guest did not stop");
        ensure!(
            snapshot
                .vcpu
                .iter()
                .all(|cpu| cpu.host_base_policy.is_none()),
            "stopped VM retained host tasks"
        );
        Ok(())
    }
}
