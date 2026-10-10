//! Top-level AxVM orchestration.

extern crate alloc;

use alloc::{string::String, vec, vec::Vec};

use anyhow::anyhow;
use anyhow::{Context, Result};
#[cfg(not(feature = "no-auto-start"))]
use axvm::VmStatus;
use axvm::{StopReason, VMId, VmHandle, VmManager, VmOperation};
use std::sync::{Arc, OnceLock};

/// Application policy and its instance-owned VM registry.
pub struct AxvmManager {
    runtime: VmManager,
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
        let plan = crate::config::prepare_guest_vm(raw_cfg)?;
        self.create_plan(plan)
    }

    pub fn create_plan(&self, plan: axvm::VmCreatePlan) -> Result<VmOperation<VmHandle>> {
        self.runtime
            .create(plan)
            .context("create VM from prepared configuration")
    }

    pub fn get(&self, vm_id: VMId) -> Option<VmHandle> {
        self.runtime.get(vm_id)
    }

    pub fn list(&self) -> Vec<VmHandle> {
        self.runtime.list()
    }

    pub fn stop_vm(&self, vm_id: VMId) -> Result<()> {
        self.require_vm(vm_id)?
            .stop(StopReason::Forced)?
            .wait()
            .context("stop VM")
    }

    pub fn start_vm(&self, vm_id: VMId) -> Result<()> {
        self.require_vm(vm_id)?
            .start()?
            .wait()
            .map(|_run| ())
            .context("start VM")
    }

    pub fn pause_vm(&self, vm_id: VMId) -> Result<()> {
        self.require_vm(vm_id)?.pause()?.wait().context("pause VM")
    }

    pub fn resume_vm(&self, vm_id: VMId) -> Result<()> {
        self.require_vm(vm_id)?
            .resume()?
            .wait()
            .context("resume VM")
    }

    pub fn reset_vm(&self, vm_id: VMId) -> Result<()> {
        self.require_vm(vm_id)?
            .reset()?
            .wait()
            .map(|_| ())
            .context("reset VM")
    }

    pub fn destroy_vm(&self, vm_id: VMId) -> Result<()> {
        let vm = self.require_vm(vm_id)?;
        vm.destroy()?.wait().context("destroy VM")?;
        vm.join_control_task().context("join VM control task")
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

    /// Read VM config files from an Axvisor-owned directory.
    pub fn filesystem_vm_configs(config_dir: &str) -> Vec<String> {
        let mut configs = Vec::new();

        debug!("Read VM config files from filesystem.");

        let entries = match ax_std::fs::read_dir(config_dir) {
            Ok(entries) => {
                info!("Find dir: {}", config_dir);
                entries
            }
            Err(_) => {
                info!("NOT find dir: {} in filesystem", config_dir);
                return configs;
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    warn!("Failed to read config directory entry: {e:?}");
                    continue;
                }
            };
            let path = entry.path();
            let path_str = path.as_str();
            debug!("Considering file: {}", path_str);
            if !path_str.ends_with(".toml") {
                continue;
            }

            let file_size = match Self::file_size(path_str) {
                Ok(file_size) => file_size,
                Err(e) => {
                    error!("Failed to get config file {path_str} metadata: {e:#}");
                    continue;
                }
            };
            info!("File {} size: {}", path_str, file_size);

            if file_size == 0 {
                warn!("File {} is empty", path_str);
                continue;
            }

            let buffer = match Self::read_file_exact(path_str, file_size) {
                Ok(buffer) => buffer,
                Err(e) => {
                    error!("Failed to read file {path_str}: {e:#}");
                    continue;
                }
            };

            match String::from_utf8(buffer) {
                Ok(content) => configs.push(content),
                Err(e) => error!("Config file {} is not valid UTF-8: {:?}", path_str, e),
            }
        }

        configs
    }

    fn open_file(file_name: &str) -> Result<ax_std::fs::File> {
        ax_std::fs::File::open(file_name)
            .map_err(|error| anyhow!("open guest image file `{file_name}`: {error}"))
    }

    pub fn file_size(file_name: &str) -> Result<usize> {
        Self::open_file(file_name)?
            .metadata()
            .map_err(|error| anyhow!("read metadata for guest image file `{file_name}`: {error}"))
            .map(|metadata| metadata.size() as usize)
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
