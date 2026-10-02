//! Top-level AxVM orchestration.

extern crate alloc;

#[cfg(not(feature = "fs"))]
use alloc::vec::Vec;
#[cfg(feature = "fs")]
use alloc::{vec, vec::Vec};

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

    /// Start the default VM set without blocking the management console.
    #[cfg_attr(
        feature = "no-auto-start",
        expect(
            dead_code,
            reason = "only the auto-start boot path launches the default VMs"
        )
    )]
    pub fn launch_default_vms(&self) -> Vec<VMId> {
        let launched = self.runtime.launch_default_vms();
        // The auto-start path bypasses `Self::start_vm`, so it republishes the
        // console state itself: without this, browser input on an auto-started
        // guest's lane is dropped exactly as it is on a control-plane start.
        for vm_id in &launched {
            crate::guest_console::mark_running(*vm_id);
        }
        launched
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
    #[cfg(any(feature = "fs", feature = "http-axum"))]
    pub fn create_vm_from_toml(raw_cfg: &str) -> Result<VMId> {
        crate::config::init_guest_vm(raw_cfg).context("create VM from TOML configuration")
    }

    /// Create one VM from a configuration the control plane already built.
    pub fn create_vm_from_config(config: axvmconfig::GuestConfig) -> Result<VMId> {
        crate::config::init_guest_vm_from_config(config).context("create VM from form fields")
    }

    /// Make sure `vm_id` is registered, creating it from the VM pool if needed.
    ///
    /// A start request names a VM, not a config file: when the id has no
    /// registered VM yet, the pool entry claiming that id is created now. The
    /// pool is re-read here, so a config repaired on the host takes effect on
    /// the next start request. Returns `Ok(false)` when neither the registry
    /// nor the pool knows the id.
    #[cfg(feature = "fs")]
    pub fn ensure_registered(vm_id: VMId) -> Result<bool> {
        if Self::vm_by_id(vm_id).is_some() {
            return Ok(true);
        }

        let pool = crate::control::domain::pool::scan();
        let Some(entry) = pool.entry(vm_id) else {
            crate::control::domain::pool::log_issues(&pool);
            return Ok(false);
        };

        Self::create_vm_from_toml(entry.toml())
            .with_context(|| format!("create VM[{vm_id}] from VM pool entry `{}`", entry.path()))?;
        Ok(true)
    }

    /// Start a VM by ID.
    ///
    /// A successful start is published to the guest console mux, exactly like
    /// the shell's own `vm start`: the mux keeps its own `running` set and gates
    /// guest input (`guest_console::route_network_input`) and host-log
    /// formatting on it, so a start that only changes the VMM state silently
    /// drops every byte a browser sends to that guest's console lane.
    #[cfg(any(feature = "fs", feature = "http-axum"))]
    pub fn start_vm(vm_id: VMId) -> Result<()> {
        AxvmRuntime::start_vm(vm_id).with_context(|| format!("start VM[{vm_id}]"))?;
        crate::guest_console::mark_running(vm_id);
        Ok(())
    }

    /// Stop a VM by ID.
    ///
    /// The console mux stops treating the guest as running as soon as the
    /// shutdown request is accepted, so bytes typed during the transition are
    /// not queued for a guest that is already leaving.
    pub fn stop_vm(vm_id: VMId) -> Result<()> {
        AxvmRuntime::stop_vm(vm_id).with_context(|| format!("stop VM[{vm_id}]"))?;
        crate::guest_console::mark_stopped(vm_id);
        Ok(())
    }

    /// Pause a VM by ID.
    #[cfg(feature = "http-axum")]
    pub fn pause_vm(vm_id: VMId) -> Result<()> {
        AxvmRuntime::pause_vm(vm_id).with_context(|| format!("pause VM[{vm_id}]"))
    }

    /// Resume a VM by ID.
    ///
    /// See [`Self::start_vm`] for why a resume republishes the console state.
    pub fn resume_vm(vm_id: VMId) -> Result<()> {
        AxvmRuntime::resume_vm(vm_id).with_context(|| format!("resume VM[{vm_id}]"))?;
        crate::guest_console::mark_running(vm_id);
        Ok(())
    }

    /// Reset a VM by ID.
    ///
    /// A reset reboots the guest, so it re-enters the same running state as a
    /// start and has to be published the same way.
    pub fn reset_vm(vm_id: VMId) -> Result<()> {
        AxvmRuntime::reset_vm(vm_id).with_context(|| format!("reset VM[{vm_id}]"))?;
        crate::guest_console::mark_running(vm_id);
        Ok(())
    }

    /// Wake the primary vCPU so it can consume newly queued console input.
    pub fn notify_vm(vm_id: VMId) -> Result<()> {
        AxvmRuntime::notify_vm(vm_id).with_context(|| format!("notify VM[{vm_id}]"))
    }

    /// Remove a VM by ID.
    ///
    /// The browser console lane belongs to the registry entry, so it is freed
    /// with the entry: the next VM can take the slot, and the previous
    /// occupant's console session is stopped first (see
    /// [`crate::network_console::release_guest`]).
    pub fn remove_vm(vm_id: VMId) -> Option<AxVMRef> {
        let vm = AxvmRuntime::remove_vm(vm_id)?;
        #[cfg(feature = "browser-console")]
        crate::network_console::release_guest(vm_id);
        Some(vm)
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
        any(
            target_arch = "aarch64",
            target_arch = "x86_64",
            target_arch = "loongarch64"
        )
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

    #[cfg(not(all(
        feature = "fs",
        any(
            target_arch = "aarch64",
            target_arch = "x86_64",
            target_arch = "loongarch64"
        )
    )))]
    fn release_host_filesystem_for_guest_passthrough(&self) {}

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
