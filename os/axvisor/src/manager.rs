//! Top-level AxVM orchestration.

extern crate alloc;

#[cfg(feature = "fs")]
use alloc::vec;
use alloc::{format, string::String, vec::Vec};

use anyhow::{Context, Result};
use axvm::{AxVMRef, AxvmRuntime, DefaultVmRunReport, VMId};

/// AxVM top-level manager.
///
/// This type belongs to the hypervisor application layer. It owns the policy
/// for loading default VM configs, starting/stopping VMs, and serving shell
/// commands. The lower `axvm` crate only supplies VM/runtime primitives.
pub struct AxvmManager {
    runtime: AxvmRuntime,
    #[cfg(feature = "fs")]
    host_storage_handoff: Option<axvm::HostStorageHandoff>,
    guest_irq_route_lease: Option<axvm::GuestIrqRouteLease>,
    guest_irq_routes_revoked: Option<axvm::GuestIrqRoutesRevoked>,
}

impl AxvmManager {
    /// Initialize the AxVM runtime services.
    pub fn new() -> Result<Self> {
        Ok(Self {
            runtime: AxvmRuntime::new().context("initialize AxVM runtime")?,
            #[cfg(feature = "fs")]
            host_storage_handoff: None,
            guest_irq_route_lease: None,
            guest_irq_routes_revoked: None,
        })
    }

    /// Load and initialize the default VM set.
    ///
    /// # Errors
    ///
    /// Returns an error if host storage cannot be transferred safely before a
    /// configured passthrough guest starts.
    pub fn init_default_vms(&mut self) -> Result<()> {
        crate::config::init_guest_vms();
        self.runtime.init_vms();
        self.release_host_storage_for_guest_passthrough()?;
        Ok(())
    }

    /// Start the default VM set and wait until it exits.
    ///
    /// # Errors
    ///
    /// Returns an error when stopped guests cannot return storage ownership to
    /// the host without violating controller or filesystem invariants, or when
    /// any configured VM could not enter its first runtime generation.
    pub fn start_default_vms(&mut self) -> Result<()> {
        let run_report = self.runtime.start_default_vms();
        let cleanup_result = self.finish_default_guest_storage();
        finish_default_vm_run(run_report, cleanup_result)
    }

    fn finish_default_guest_storage(&mut self) -> Result<()> {
        self.ensure_default_guest_irq_routes_revoked()
            .context("revoke stopped default-guest passthrough IRQ routes")?;
        self.return_host_storage_after_guest_exit()
    }

    /// Create one VM from a TOML config string.
    pub fn create_vm_from_toml(raw_cfg: &str) -> Result<VMId> {
        crate::config::init_guest_vm(raw_cfg).context("create VM from TOML configuration")
    }

    /// Start a VM by ID.
    pub fn start_vm(vm_id: VMId) -> Result<()> {
        Self::ensure_interactive_operation_has_no_passthrough(vm_id, "start")?;
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
        Self::ensure_interactive_operation_has_no_passthrough(vm_id, "reset")?;
        AxvmRuntime::reset_vm(vm_id).with_context(|| format!("reset VM[{vm_id}]"))
    }

    /// Remove a VM by ID.
    pub fn remove_vm(vm_id: VMId) -> Result<Option<AxVMRef>> {
        Self::ensure_interactive_operation_has_no_passthrough(vm_id, "remove")?;
        Ok(AxvmRuntime::remove_vm(vm_id))
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

    fn ensure_interactive_operation_has_no_passthrough(
        vm_id: VMId,
        operation: &'static str,
    ) -> Result<()> {
        let vm =
            Self::vm_by_id(vm_id).ok_or_else(|| anyhow::anyhow!("VM[{vm_id}] was not found"))?;
        let uses_passthrough = vm.with_config(|config| {
            !config.pass_through_devices().is_empty()
                || !config.pass_through_addresses().is_empty()
                || !config.pass_through_ports().is_empty()
                || !config.pass_through_spis().is_empty()
        });
        if uses_passthrough {
            return Err(anyhow::anyhow!(
                "interactive {operation} of passthrough VM[{vm_id}] is unsupported without a \
                 retained host-device ownership transaction; configure it as a default guest"
            ));
        }
        Ok(())
    }

    #[cfg(feature = "fs")]
    fn release_host_storage_for_guest_passthrough(&mut self) -> Result<()> {
        if self.guest_irq_route_lease.is_some()
            || self.guest_irq_routes_revoked.is_some()
            || self.host_storage_handoff.is_some()
        {
            return Err(anyhow::anyhow!(
                "default-guest storage/IRQ ownership transaction is already active"
            ));
        }

        let prepared_handoff = axvm::begin_host_storage_handoff()
            .context("select and detach host storage for guest passthrough")?;
        let Some(mut handoff) = prepared_handoff else {
            let mut route_lease = axvm::GuestIrqRouteLease::new();
            let activation = axvm::activate_guest_irq_routes(&mut route_lease);
            self.guest_irq_route_lease = Some(route_lease);
            if let Err(activation_error) = activation {
                return self
                    .rollback_failed_guest_irq_activation_without_storage(activation_error.into());
            }
            return Ok(());
        };
        if let Err(commit_error) = axvm::commit_host_storage_handoff_to_guest(&mut handoff) {
            self.host_storage_handoff = Some(handoff);
            return Err(commit_error).context(
                "commit detached host storage to the guest; controller ownership remains \
                fail-closed",
            );
        }
        let mut route_lease = axvm::GuestIrqRouteLease::new();
        let activation = axvm::activate_guest_irq_routes(&mut route_lease);
        self.guest_irq_route_lease = Some(route_lease);
        if let Err(activation_error) = activation {
            return self.rollback_failed_guest_storage_activation(handoff, activation_error.into());
        }
        #[cfg(target_arch = "x86_64")]
        if let Err(preparation_error) =
            crate::config::prepare_x86_host_storage_passthrough(&handoff)
        {
            return self.rollback_failed_guest_storage_activation(handoff, preparation_error);
        }
        self.host_storage_handoff = Some(handoff);
        info!("Host storage cleanly detached before guest passthrough devices start");
        Ok(())
    }

    fn rollback_failed_guest_irq_activation_without_storage(
        &mut self,
        activation_error: anyhow::Error,
    ) -> Result<()> {
        match self.ensure_default_guest_irq_routes_revoked() {
            Ok(()) => {
                self.guest_irq_routes_revoked = None;
                Err(activation_error).context(
                "activate guest IRQ routes after storage selection; partial routes were revoked",
                )
            }
            Err(revoke_error) => Err(activation_error).context(format!(
                "activate guest IRQ routes after storage selection; route revocation failed and \
                 the retained lease remains fail-closed: {revoke_error}"
            )),
        }
    }

    #[cfg(feature = "fs")]
    fn rollback_failed_guest_storage_activation(
        &mut self,
        mut handoff: axvm::HostStorageHandoff,
        activation_error: anyhow::Error,
    ) -> Result<()> {
        if let Err(route_revoke_error) = self.ensure_default_guest_irq_routes_revoked() {
            self.host_storage_handoff = Some(handoff);
            return Err(activation_error).context(format!(
                "activate guest passthrough routes after controller commit; post-selection route \
                 revocation failed and ownership remains fail-closed: {route_revoke_error}"
            ));
        }
        let routes_revoked = self
            .guest_irq_routes_revoked
            .as_ref()
            .expect("successful route revocation publishes its retained proof");
        let revoked = match axvm::revoke_guest_storage_routes(&handoff, routes_revoked) {
            Ok(revoked) => revoked,
            Err(revoke_error) => {
                self.host_storage_handoff = Some(handoff);
                return Err(activation_error).context(format!(
                    "activate guest passthrough routes after controller commit; guest \
                     route revocation failed and host storage remains fail-closed: {revoke_error}"
                ));
            }
        };
        match axvm::return_host_storage_from_guest(&mut handoff, revoked) {
            Ok(()) => {
                self.guest_irq_routes_revoked = None;
                Err(activation_error).context(
                    "activate guest passthrough routes after controller commit; controller \
                     reinitialization and host filesystem remount completed",
                )
            }
            Err(return_error) => {
                self.host_storage_handoff = Some(handoff);
                Err(activation_error).context(format!(
                    "activate guest passthrough routes after controller commit; host \
                     storage return failed closed: {return_error}"
                ))
            }
        }
    }

    fn ensure_default_guest_irq_routes_revoked(&mut self) -> Result<()> {
        if self.guest_irq_routes_revoked.is_some() {
            return Ok(());
        }
        let Some(route_lease) = self.guest_irq_route_lease.as_mut() else {
            return Err(anyhow::anyhow!(
                "default-guest passthrough route lease is missing"
            ));
        };
        let routes_revoked = axvm::revoke_guest_irq_route_lease(route_lease)
            .context("revoke retained default-guest IRQ route lease")?;
        self.guest_irq_route_lease = None;
        self.guest_irq_routes_revoked = Some(routes_revoked);
        Ok(())
    }

    #[cfg(not(feature = "fs"))]
    fn release_host_storage_for_guest_passthrough(&mut self) -> Result<()> {
        if self.guest_irq_route_lease.is_some() || self.guest_irq_routes_revoked.is_some() {
            return Err(anyhow::anyhow!(
                "default-guest IRQ ownership transaction is already active"
            ));
        }
        let mut route_lease = axvm::GuestIrqRouteLease::new();
        let activation = axvm::activate_guest_irq_routes(&mut route_lease);
        self.guest_irq_route_lease = Some(route_lease);
        if let Err(error) = activation {
            return self.rollback_failed_guest_irq_activation_without_storage(error.into());
        }
        Ok(())
    }

    #[cfg(feature = "fs")]
    fn return_host_storage_after_guest_exit(&mut self) -> Result<()> {
        let Some(handoff) = self.host_storage_handoff.as_mut() else {
            self.guest_irq_routes_revoked = None;
            return Ok(());
        };
        let routes_revoked = self
            .guest_irq_routes_revoked
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("guest IRQ routes have not been revoked"))?;
        let revoked = axvm::revoke_guest_storage_routes(handoff, routes_revoked)
            .context("revoke and drain stopped guest storage routes")?;
        axvm::return_host_storage_from_guest(handoff, revoked)
            .context("return block controllers and remount the host filesystem")?;
        self.host_storage_handoff = None;
        self.guest_irq_routes_revoked = None;
        info!("Host storage returned and remounted after all default guests stopped");
        Ok(())
    }

    #[cfg(not(feature = "fs"))]
    fn return_host_storage_after_guest_exit(&mut self) -> Result<()> {
        self.guest_irq_routes_revoked = None;
        Ok(())
    }

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
            .map_err(|error| anyhow::anyhow!("open guest image file `{file_name}`: {error}"))
    }

    #[cfg(feature = "fs")]
    pub fn file_size(file_name: &str) -> Result<usize> {
        Self::open_file(file_name)?
            .metadata()
            .map_err(|error| {
                anyhow::anyhow!("read metadata for guest image file `{file_name}`: {error}")
            })
            .map(|metadata| metadata.size() as usize)
    }

    #[cfg(feature = "fs")]
    pub fn read_file_exact(file_name: &str, read_size: usize) -> Result<Vec<u8>> {
        use ax_std::io::Read;

        let mut file = Self::open_file(file_name)?;
        let mut buffer = vec![0u8; read_size];
        file.read_exact(&mut buffer).map_err(|error| {
            anyhow::anyhow!("read {read_size} bytes from guest image file `{file_name}`: {error}")
        })?;
        Ok(buffer)
    }

    #[cfg(feature = "fs")]
    pub fn read_file(file_name: &str) -> Result<Vec<u8>> {
        let size = Self::file_size(file_name)?;
        Self::read_file_exact(file_name, size)
    }
}

fn finish_default_vm_run(report: DefaultVmRunReport, cleanup_result: Result<()>) -> Result<()> {
    let start_failures = format_default_vm_start_failures(&report);
    match (cleanup_result, start_failures) {
        (Ok(()), None) => Ok(()),
        (Ok(()), Some(failures)) => Err(anyhow::anyhow!("default VM startup failed: {failures}")),
        (Err(cleanup_error), None) => Err(cleanup_error),
        (Err(cleanup_error), Some(failures)) => {
            Err(cleanup_error).context(format!("default VM startup also failed: {failures}"))
        }
    }
}

fn format_default_vm_start_failures(report: &DefaultVmRunReport) -> Option<String> {
    if report.all_started() {
        return None;
    }
    Some(
        report
            .start_failures()
            .iter()
            .map(|failure| format!("VM[{}]: {}", failure.vm_id(), failure.error()))
            .collect::<Vec<_>>()
            .join("; "),
    )
}
