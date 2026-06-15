//! Top-level AxVM orchestration.

extern crate alloc;

#[cfg(not(feature = "fs"))]
use alloc::vec::Vec;
#[cfg(feature = "fs")]
use alloc::{string::String, vec, vec::Vec};

use ax_errno::AxResult;
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
    pub fn new() -> AxResult<Self> {
        #[cfg(target_arch = "loongarch64")]
        {
            ax_std::os::arceos::modules::ax_hal::loongarch64_hv_irq::register_virtual_irq_injector(
                inject_loongarch_platform_irq,
            );
            ax_std::os::arceos::modules::ax_hal::loongarch64_hv_irq::enable_external_irq_line();
        }
        Ok(Self {
            runtime: AxvmRuntime::new()?,
        })
    }

    /// Load and initialize the default VM set.
    pub fn init_default_vms(&self) {
        crate::config::init_guest_vms();
        #[cfg(target_arch = "loongarch64")]
        init_loongarch_passthrough_irq_routes();
        self.runtime.init_vms();
        self.release_host_filesystem_for_guest_passthrough();
    }

    /// Start the default VM set and wait until it exits.
    pub fn start_default_vms(&self) {
        self.runtime.start_default_vms();
    }

    /// Create one VM from a TOML config string.
    pub fn create_vm_from_toml(raw_cfg: &str) -> AxResult<VMId> {
        crate::config::init_guest_vm(raw_cfg)
    }

    /// Start a VM by ID.
    pub fn start_vm(vm_id: VMId) -> AxResult {
        AxvmRuntime::start_vm(vm_id)
    }

    /// Stop a VM by ID.
    pub fn stop_vm(vm_id: VMId) -> AxResult {
        AxvmRuntime::stop_vm(vm_id)
    }

    /// Resume a VM by ID.
    pub fn resume_vm(vm_id: VMId) -> AxResult {
        AxvmRuntime::resume_vm(vm_id)
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
                    error!("Failed to get config file {} metadata: {:?}", path_str, e);
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
                    error!("Failed to read file {}: {:?}", path_str, e);
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
    fn open_file(file_name: &str) -> AxResult<ax_std::fs::File> {
        ax_std::fs::File::open(file_name).map_err(|err| {
            ax_errno::ax_err_type!(
                NotFound,
                alloc::format!(
                    "Failed to open {}, err {:?}, please check your disk.img",
                    file_name,
                    err
                )
            )
        })
    }

    #[cfg(feature = "fs")]
    pub fn file_size(file_name: &str) -> AxResult<usize> {
        Self::open_file(file_name)?
            .metadata()
            .map_err(|err| {
                ax_errno::ax_err_type!(
                    Io,
                    alloc::format!(
                        "Failed to get metadate of file {}, err {:?}",
                        file_name,
                        err
                    )
                )
            })
            .map(|metadata| metadata.size() as usize)
    }

    #[cfg(feature = "fs")]
    pub fn read_file_exact(file_name: &str, read_size: usize) -> AxResult<Vec<u8>> {
        use ax_std::io::Read;

        let mut file = Self::open_file(file_name)?;
        let mut buffer = vec![0u8; read_size];
        file.read_exact(&mut buffer).map_err(|err| {
            ax_errno::ax_err_type!(
                Io,
                alloc::format!("Failed in reading from file {}, err {:?}", file_name, err)
            )
        })?;
        Ok(buffer)
    }

    #[cfg(feature = "fs")]
    pub fn read_file(file_name: &str) -> AxResult<Vec<u8>> {
        let size = Self::file_size(file_name)?;
        Self::read_file_exact(file_name, size)
    }
}

#[cfg(target_arch = "loongarch64")]
fn inject_loongarch_platform_irq(vm_id: usize, vcpu_id: usize, vector: usize, physical_irq: usize) {
    if let Err(err) = axvm::inject_vm_vcpu_external_interrupt(vm_id, vcpu_id, vector, physical_irq)
    {
        warn!(
            "failed to inject LoongArch platform IRQ {vector:#x}/physical {physical_irq:#x} for \
             VM[{vm_id}] VCpu[{vcpu_id}]: {err:?}"
        );
    }
}

#[cfg(target_arch = "loongarch64")]
fn init_loongarch_passthrough_irq_routes() {
    let mut matched = 0usize;
    for vm in axvm::get_vm_list() {
        let routes = crate::fdt::get_loongarch_guest_irq_routes(vm.id());
        if !routes.is_empty() {
            matched += 1;
            let vcpu_id = 0usize;
            info!(
                "Registering {} LoongArch passthrough IRQ route(s) for VM[{}]",
                routes.len(),
                vm.id()
            );
            for route in routes {
                ax_std::os::arceos::modules::ax_hal::loongarch64_hv_irq::register_guest_irq_route(
                    route.physical_irq,
                    vm.id(),
                    vcpu_id,
                    route.guest_vector,
                );
            }
            continue;
        }

        let passthrough = vm.with_config(|cfg| !cfg.pass_through_devices().is_empty());
        if !passthrough {
            continue;
        }

        matched += 1;
        warn!(
            "VM[{}] has passthrough devices but no LoongArch guest IRQ route parsed from DTB",
            vm.id()
        );
    }

    if matched > 1 {
        warn!("LoongArch passthrough IRQ routing matched {matched} VMs; check per-IRQ ownership");
    }
}
