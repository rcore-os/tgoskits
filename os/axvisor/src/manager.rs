//! Top-level AxVM orchestration.

extern crate alloc;

use alloc::{vec, vec::Vec};

use anyhow::anyhow;
use anyhow::{Context, Result};
use axvm::{AxVMRef, AxvmRuntime, VMId};

/// AxVM top-level manager.
///
/// This type belongs to the hypervisor application layer. It owns the policy
/// for loading default VM configs, starting/stopping VMs, and serving shell
/// commands. The lower `axvm` crate only supplies VM/runtime primitives.
pub struct AxvmManager {
    runtime: AxvmRuntime,
}

impl AxvmManager {
    /// Initialize the AxVM runtime services.
    pub fn new() -> Result<Self> {
        Ok(Self {
            runtime: AxvmRuntime::new().context("initialize AxVM runtime")?,
        })
    }

    /// Load and initialize the default VM set.
    pub fn init_default_vms(&self) -> Result<()> {
        crate::config::init_guest_vms()?;
        self.runtime.init_vms();
        self.release_host_filesystem_for_guest_passthrough();
        Ok(())
    }

    /// Start the default VM set without blocking the management console.
    #[cfg_attr(
        feature = "no-auto-start",
        expect(
            dead_code,
            reason = "only the auto-start boot path launches the default VMs"
        )
    )]
    pub fn launch_default_vms(&self) -> Vec<VMId> {
        self.runtime.launch_default_vms()
    }

    /// Wait until every running VM has stopped.
    #[cfg_attr(
        feature = "no-auto-start",
        expect(
            dead_code,
            reason = "only the auto-start boot path waits for default-VM completion"
        )
    )]
    pub fn wait_for_default_vms() {
        AxvmRuntime::wait_for_all_vms();
    }

    /// Create one VM from a TOML config string.
    pub fn create_vm_from_toml(raw_cfg: &str) -> Result<VMId> {
        crate::config::init_guest_vm(raw_cfg).context("create VM from TOML configuration")
    }

    /// Start a VM by ID.
    pub fn start_vm(vm_id: VMId) -> Result<()> {
        AxvmRuntime::start_vm(vm_id).with_context(|| format!("start VM[{vm_id}]"))
    }

    /// Stop a VM by ID.
    pub fn stop_vm(vm_id: VMId) -> Result<()> {
        AxvmRuntime::stop_vm(vm_id).with_context(|| format!("stop VM[{vm_id}]"))
    }

    /// Pause a VM by ID.
    #[cfg(feature = "http-axum")]
    pub fn pause_vm(vm_id: VMId) -> Result<()> {
        AxvmRuntime::pause_vm(vm_id).with_context(|| format!("pause VM[{vm_id}]"))
    }

    /// Resume a VM by ID.
    pub fn resume_vm(vm_id: VMId) -> Result<()> {
        AxvmRuntime::resume_vm(vm_id).with_context(|| format!("resume VM[{vm_id}]"))
    }

    /// Reset a VM by ID.
    pub fn reset_vm(vm_id: VMId) -> Result<()> {
        AxvmRuntime::reset_vm(vm_id).with_context(|| format!("reset VM[{vm_id}]"))
    }

    /// Wake the primary vCPU so it can consume newly queued console input.
    pub fn notify_vm(vm_id: VMId) -> Result<()> {
        AxvmRuntime::notify_vm(vm_id).with_context(|| format!("notify VM[{vm_id}]"))
    }

    /// Remove a VM by ID.
    pub fn remove_vm(vm_id: VMId) -> Option<AxVMRef> {
        AxvmRuntime::remove_vm(vm_id)
    }

    /// Run a closure with a VM by ID.
    pub fn with_vm<T>(vm_id: VMId, f: impl FnOnce(AxVMRef) -> T) -> Option<T> {
        AxvmRuntime::with_vm(vm_id, f)
    }

    /// Return the current VM list snapshot.
    pub fn vm_list() -> Vec<AxVMRef> {
        axvm::get_vm_list()
    }

    /// Return one VM by ID.
    pub fn vm_by_id(vm_id: VMId) -> Option<AxVMRef> {
        axvm::get_vm_by_id(vm_id)
    }

    #[cfg(all(any(
        target_arch = "aarch64",
        target_arch = "x86_64",
        target_arch = "loongarch64"
    )))]
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

    #[cfg(not(all(any(
        target_arch = "aarch64",
        target_arch = "x86_64",
        target_arch = "loongarch64"
    ))))]
    fn release_host_filesystem_for_guest_passthrough(&self) {}

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
