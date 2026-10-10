//! Top-level AxVM orchestration.

extern crate alloc;

use alloc::{vec, vec::Vec};

use anyhow::anyhow;
use anyhow::{Context, Result};
#[cfg(not(feature = "no-auto-start"))]
use axvm::VmStatus;
use axvm::{StopReason, VMId, VmHandle, VmManager, VmOperation};
use std::sync::{Arc, Mutex, OnceLock};

use crate::sync::MutexExt;

/// Application policy and its instance-owned VM registry.
pub struct AxvmManager {
    runtime: VmManager,
    /// Serializes VM creation completion and destruction with console-lane
    /// reclamation. A lane must not be reused between the runtime operation
    /// finishing and the old owner releasing it.
    lifecycle: Mutex<()>,
}

static APPLICATION_MANAGER: OnceLock<Arc<AxvmManager>> = OnceLock::new();

/// Returns the manager installed by the application before starting services.
pub fn manager() -> &'static AxvmManager {
    APPLICATION_MANAGER
        .get()
        .expect("application installs its manager before starting services")
}

impl AxvmManager {
    pub fn new() -> Result<Arc<Self>> {
        let manager = Arc::new(Self {
            runtime: VmManager::new().context("initialize AxVM runtime")?,
            lifecycle: Mutex::new(()),
        });
        APPLICATION_MANAGER
            .set(manager.clone())
            .map_err(|_| anyhow::anyhow!("application manager is already installed"))?;
        Ok(manager)
    }

    pub fn init_default_vms(&self) -> Result<()> {
        crate::config::init_guest_vms()?;
        self.release_host_filesystem_for_guest_passthrough();
        Ok(())
    }

    #[cfg(not(feature = "no-auto-start"))]
    pub fn launch_default_vms(&self) -> Vec<VMId> {
        let mut started = Vec::new();
        for vm in self.runtime.list() {
            match vm.start().and_then(VmOperation::wait) {
                Ok(_) => started.push(vm.key().vm_id()),
                Err(error) => error!("VM[{}] failed to start: {error}", vm.key().vm_id()),
            }
        }
        started
    }

    #[cfg(not(feature = "no-auto-start"))]
    pub fn wait_for_default_vms(&self) {
        while self.runtime.list().iter().any(|vm| {
            matches!(
                vm.snapshot().state,
                VmStatus::Running | VmStatus::Pausing | VmStatus::Paused | VmStatus::Stopping
            )
        }) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    pub fn create_vm_from_toml(&self, raw_cfg: &str) -> Result<VmOperation<VmHandle>> {
        let _lifecycle = self.lifecycle.lock_unpoisoned();
        self.create_vm_from_toml_locked(raw_cfg)
    }

    fn create_vm_from_toml_locked(&self, raw_cfg: &str) -> Result<VmOperation<VmHandle>> {
        let plan = crate::config::prepare_guest_vm(raw_cfg)?;
        self.create_plan_locked(plan)
    }

    /// Creates one VM and waits until its control task has finished setup.
    pub fn create_vm_from_toml_and_wait(&self, raw_cfg: &str) -> Result<VmHandle> {
        let _lifecycle = self.lifecycle.lock_unpoisoned();
        let operation = self.create_vm_from_toml_locked(raw_cfg)?;
        self.wait_for_created_vm_locked(operation)
    }

    pub fn create_plan(&self, plan: axvm::VmCreatePlan) -> Result<VmOperation<VmHandle>> {
        let _lifecycle = self.lifecycle.lock_unpoisoned();
        self.create_plan_locked(plan)
    }

    fn create_plan_locked(&self, plan: axvm::VmCreatePlan) -> Result<VmOperation<VmHandle>> {
        #[cfg(feature = "web")]
        let vm_id = plan.config.id();
        #[cfg(feature = "web")]
        let lane = crate::control::network_console::register_guest(vm_id, &plan.config.name())?;

        match self.runtime.create(plan) {
            Ok(operation) => Ok(operation),
            Err(error) => {
                #[cfg(feature = "web")]
                if matches!(
                    lane,
                    crate::control::network_console::LaneAllocation::Allocated
                ) {
                    crate::control::network_console::release_guest(vm_id);
                }
                Err(error).context("create VM from prepared configuration")
            }
        }
    }

    /// Waits for VM initialization and releases its browser lane on failure.
    pub fn wait_for_created_vm(&self, operation: VmOperation<VmHandle>) -> Result<VmHandle> {
        let _lifecycle = self.lifecycle.lock_unpoisoned();
        self.wait_for_created_vm_locked(operation)
    }

    fn wait_for_created_vm_locked(&self, operation: VmOperation<VmHandle>) -> Result<VmHandle> {
        #[cfg(feature = "web")]
        let vm_id = operation.id().vm().vm_id();
        match operation.wait() {
            Ok(vm) => Ok(vm),
            Err(error) => {
                #[cfg(feature = "web")]
                {
                    // AxVM keeps a failed creation in the registry so the
                    // caller can inspect its snapshot. Remove that failed
                    // entry before returning, otherwise releasing its lane
                    // would let a new VM claim the same route while the old
                    // numeric id is still reserved.
                    let cleanup_finished = match self.get(vm_id) {
                        None => true,
                        Some(vm) => match vm.destroy() {
                            Ok(destroy) => match destroy.wait() {
                                Ok(()) => match vm.join_control_task() {
                                    Ok(()) => true,
                                    Err(cleanup_error) => {
                                        warn!(
                                            "VM[{vm_id}] creation failed and control task did not join: {cleanup_error:#}"
                                        );
                                        false
                                    }
                                },
                                Err(cleanup_error) => {
                                    warn!(
                                        "VM[{vm_id}] creation failed and cleanup did not finish: {cleanup_error:#}"
                                    );
                                    false
                                }
                            },
                            Err(cleanup_error) => {
                                warn!(
                                    "VM[{vm_id}] creation failed and cleanup could not start: {cleanup_error:#}"
                                );
                                false
                            }
                        },
                    };
                    if cleanup_finished {
                        crate::control::network_console::release_guest(vm_id);
                    }
                }
                Err(error.into())
            }
        }
    }

    /// Ensures that `vm_id` is registered, creating it from the current pool
    /// entry when necessary. A pool entry is a candidate until a start request
    /// names it; no VM or console lane is reserved by merely listing the pool.
    #[cfg(feature = "web")]
    pub fn ensure_vm_from_pool(&self, vm_id: VMId) -> Result<bool> {
        let _lifecycle = self.lifecycle.lock_unpoisoned();
        if self.get(vm_id).is_some() {
            return Ok(true);
        }

        let pool = crate::control::domain::pool::scan();
        let Some(entry) = pool.entries().iter().find(|entry| entry.id() == vm_id) else {
            crate::control::domain::pool::log_issues(&pool);
            return Ok(false);
        };
        let operation = self
            .create_vm_from_toml_locked(entry.toml())
            .with_context(|| format!("create VM[{vm_id}] from VM pool entry `{}`", entry.path()))?;
        self.wait_for_created_vm_locked(operation)
            .with_context(|| {
                format!(
                    "initialize VM[{vm_id}] from VM pool entry `{}`",
                    entry.path()
                )
            })?;
        Ok(true)
    }

    pub fn get(&self, vm_id: VMId) -> Option<VmHandle> {
        self.runtime.get(vm_id)
    }

    pub fn list(&self) -> Vec<VmHandle> {
        self.runtime.list()
    }

    pub fn stop_vm(&self, vm_id: VMId) -> Result<()> {
        let _lifecycle = self.lifecycle.lock_unpoisoned();
        self.require_vm(vm_id)?
            .stop(StopReason::Forced)?
            .wait()
            .context("stop VM")
    }

    pub fn start_vm(&self, vm_id: VMId) -> Result<()> {
        let _lifecycle = self.lifecycle.lock_unpoisoned();
        self.require_vm(vm_id)?
            .start()?
            .wait()
            .map(|_run| ())
            .context("start VM")
    }

    pub fn pause_vm(&self, vm_id: VMId) -> Result<()> {
        let _lifecycle = self.lifecycle.lock_unpoisoned();
        self.require_vm(vm_id)?.pause()?.wait().context("pause VM")
    }

    pub fn resume_vm(&self, vm_id: VMId) -> Result<()> {
        let _lifecycle = self.lifecycle.lock_unpoisoned();
        self.require_vm(vm_id)?
            .resume()?
            .wait()
            .context("resume VM")
    }

    pub fn reset_vm(&self, vm_id: VMId) -> Result<()> {
        let _lifecycle = self.lifecycle.lock_unpoisoned();
        self.require_vm(vm_id)?
            .reset()?
            .wait()
            .map(|_| ())
            .context("reset VM")
    }

    pub fn destroy_vm(&self, vm_id: VMId) -> Result<()> {
        let _lifecycle = self.lifecycle.lock_unpoisoned();
        let vm = self.require_vm(vm_id)?;
        vm.destroy()?.wait().context("destroy VM")?;
        vm.join_control_task().context("join VM control task")?;
        #[cfg(feature = "web")]
        crate::control::network_console::release_guest(vm_id);
        Ok(())
    }

    pub fn notify_vm(&self, vm_id: VMId) -> Result<()> {
        self.require_vm(vm_id)?
            .notify_devices()
            .context("notify VM devices")
    }

    pub fn require_vm(&self, vm_id: VMId) -> Result<VmHandle> {
        self.get(vm_id)
            .ok_or_else(|| axvm::AxVmError::VmNotFound { vm_id }.into())
    }

    #[cfg(any(
        target_arch = "aarch64",
        target_arch = "x86_64",
        target_arch = "loongarch64"
    ))]
    fn release_host_filesystem_for_guest_passthrough(&self) {
        if !crate::config::host_filesystem_release_required() {
            return;
        }

        axvm::host::shutdown_filesystems().expect(
            "Failed to release host filesystem before guest passthrough devices take ownership",
        );
        axvm::host::prepare_block_passthrough_device();
        info!("Host filesystem cleanly unmounted before guest passthrough devices start");
    }

    #[cfg(not(any(
        target_arch = "aarch64",
        target_arch = "x86_64",
        target_arch = "loongarch64"
    )))]
    fn release_host_filesystem_for_guest_passthrough(&self) {}

    fn open_file(file_name: &str) -> Result<ax_std::fs::File> {
        ax_std::fs::File::open(file_name)
            .map_err(|error| anyhow!("open guest image file `{file_name}`: {error}"))
    }

    pub fn file_size(file_name: &str) -> Result<usize> {
        Self::open_file(file_name)?
            .metadata()
            .map_err(|error| anyhow!("read metadata for guest image file `{file_name}`: {error}"))
            .and_then(|metadata| {
                usize::try_from(metadata.size()).map_err(|_| {
                    anyhow!("guest image file `{file_name}` is too large for this target")
                })
            })
    }

    pub fn read_file_exact(file_name: &str, read_size: usize) -> Result<Vec<u8>> {
        use ax_std::io::Read;

        let mut file = Self::open_file(file_name)?;
        let mut buffer = vec![0u8; read_size];
        file.read_exact(&mut buffer).map_err(|error| {
            anyhow!("read {read_size} bytes from guest image file `{file_name}`: {error}")
        })?;
        Ok(buffer)
    }

    pub fn read_file(file_name: &str) -> Result<Vec<u8>> {
        let size = Self::file_size(file_name)?;
        Self::read_file_exact(file_name, size)
    }
}
