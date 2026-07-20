//! Top-level AxVM orchestration.

extern crate alloc;

#[cfg(not(feature = "fs"))]
use alloc::vec::Vec;
#[cfg(feature = "fs")]
use alloc::{string::String, vec, vec::Vec};

#[cfg(feature = "fs")]
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
    pub fn init_default_vms(&self) {
        crate::config::init_guest_vms();
        self.runtime.init_vms();
        self.release_host_filesystem_for_guest_passthrough();
    }

    /// Start the default VM set and wait until it exits.
    pub fn start_default_vms(&self) {
        self.runtime.start_default_vms();
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

    /// Resume a VM by ID.
    pub fn resume_vm(vm_id: VMId) -> Result<()> {
        AxvmRuntime::resume_vm(vm_id).with_context(|| format!("resume VM[{vm_id}]"))
    }

    /// Reset a VM by ID.
    pub fn reset_vm(vm_id: VMId) -> Result<()> {
        AxvmRuntime::reset_vm(vm_id).with_context(|| format!("reset VM[{vm_id}]"))
    }

    /// Remove a VM by ID.
    pub fn remove_vm(vm_id: VMId) -> Option<AxVMRef> {
        #[cfg(target_arch = "loongarch64")]
        unregister_loongarch_passthrough_irq_routes(vm_id);
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

    #[cfg(all(
        feature = "fs",
        any(target_arch = "x86_64", target_arch = "loongarch64")
    ))]
    fn release_host_filesystem_for_guest_passthrough(&self) {
        if !crate::config::host_filesystem_release_required() {
            return;
        }

        axvm::shutdown_host_filesystems().expect(
            "Failed to release host filesystem before guest passthrough devices take ownership",
        );
        #[cfg(target_arch = "x86_64")]
        crate::config::prepare_x86_host_fs_passthrough_devices();
        info!("Host filesystem cleanly unmounted before guest passthrough devices start");
    }

    #[cfg(not(all(
        feature = "fs",
        any(target_arch = "x86_64", target_arch = "loongarch64")
    )))]
    fn release_host_filesystem_for_guest_passthrough(&self) {}

    /// Read VM config files from an Axvisor-owned directory.
    #[cfg(feature = "fs")]
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

    #[cfg(feature = "fs")]
    fn open_file(file_name: &str) -> Result<ax_std::fs::File> {
        ax_std::fs::File::open(file_name)
            .map_err(|error| anyhow!("open guest image file `{file_name}`: {error}"))
    }

    #[cfg(feature = "fs")]
    pub fn file_size(file_name: &str) -> Result<usize> {
        Self::open_file(file_name)?
            .metadata()
            .map_err(|error| anyhow!("read metadata for guest image file `{file_name}`: {error}"))
            .map(|metadata| metadata.size() as usize)
    }

    #[cfg(feature = "fs")]
    pub fn read_file_exact(file_name: &str, read_size: usize) -> Result<Vec<u8>> {
        use ax_std::io::Read;

        let mut file = Self::open_file(file_name)?;
        let mut buffer = vec![0u8; read_size];
        file.read_exact(&mut buffer).map_err(|error| {
            anyhow!("read {read_size} bytes from guest image file `{file_name}`: {error}")
        })?;
        Ok(buffer)
    }

    #[cfg(feature = "fs")]
    pub fn read_file(file_name: &str) -> Result<Vec<u8>> {
        let size = Self::file_size(file_name)?;
        Self::read_file_exact(file_name, size)
    }
}

#[cfg(target_arch = "loongarch64")]
pub(crate) fn register_loongarch_passthrough_irq_routes(vm_id: VMId) {
    let routes = axvm::boot::guest_platform::loongarch64::get_guest_irq_routes(vm_id);
    if routes.is_empty() {
        if let Some(vm) = axvm::get_vm_by_id(vm_id) {
            let passthrough = vm.with_config(|cfg| !cfg.pass_through_devices().is_empty());
            if passthrough {
                warn!(
                    "VM[{vm_id}] has passthrough devices but no LoongArch guest IRQ route parsed"
                );
            }
        }
        return;
    }

    let vcpu_id = 0usize;
    info!(
        "Registering {} LoongArch passthrough IRQ route(s) for VM[{vm_id}]",
        routes.len()
    );
    for route in routes {
        axvm::register_loongarch_guest_irq_route(
            route.physical_irq,
            vm_id,
            vcpu_id,
            route.guest_vector,
        );
    }
}

#[cfg(target_arch = "loongarch64")]
fn unregister_loongarch_passthrough_irq_routes(vm_id: VMId) {
    axvm::unregister_loongarch_guest_irq_routes(vm_id);
}
