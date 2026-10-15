//! Top-level AxVM orchestration.

extern crate alloc;

#[cfg(feature = "web")]
use alloc::collections::BTreeSet;
use alloc::{vec, vec::Vec};

use anyhow::{Context, Result};
use anyhow::{anyhow, bail};
use axvisor::builtin::BuiltinCatalog;
use axvm::{StopReason, VMId, VmHandle, VmManager, VmOperation};
use std::sync::{Arc, Mutex, OnceLock};

use crate::sync::MutexExt;

/// A start request named an ID that is not registered and has no candidate
/// configuration in either the builtin catalog or the dynamic pool.
#[derive(Debug)]
pub struct VmCandidateNotFound {
    pub vm_id: VMId,
}

impl core::fmt::Display for VmCandidateNotFound {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "VM[{}] is neither registered nor in the builtin catalog or dynamic pool",
            self.vm_id
        )
    }
}

impl std::error::Error for VmCandidateNotFound {}

/// Application policy and its instance-owned VM registry.
pub struct AxvmManager {
    runtime: VmManager,
    /// Immutable configurations shipped under `/guest/builtin`.
    builtin: OnceLock<BuiltinCatalog>,
    /// Protected by `lifecycle`; records the one-way host filesystem handoff.
    host_filesystem_released: Mutex<bool>,
    /// Serializes VM creation completion and destruction with console-lane
    /// reclamation. A lane must not be reused between the runtime operation
    /// finishing and the old owner releasing it.
    lifecycle: Mutex<()>,
    cpu_freq_backend: Arc<dyn axvm::GuestCpuFreqBackend>,
}

static APPLICATION_MANAGER: OnceLock<Arc<AxvmManager>> = OnceLock::new();

/// Returns the manager installed by the application before starting services.
pub fn manager() -> &'static AxvmManager {
    APPLICATION_MANAGER
        .get()
        .expect("application installs its manager before starting services")
}

/// Refines the configuration-level host filesystem handoff decision with the
/// created VM's device snapshot. On a block root the handoff follows whether
/// the guest actually assigned the registered host block controller: the
/// resolved device names decide this more precisely than the raw TOML, and a
/// passthrough guest that leaves host storage shared must not trigger the
/// handoff. When ownership cannot be proven, the configured decision stands.
fn guest_took_host_storage(vm: &VmHandle, configured: bool) -> bool {
    if ax_fs_ng::root::root_kind() == Some(ax_fs_ng::root::RootKind::Block) {
        axvm::host::storage::guest_uses_host_block_controller(&vm.snapshot().description)
            .unwrap_or(configured)
    } else {
        configured
    }
}

impl AxvmManager {
    pub fn new() -> Result<Arc<Self>> {
        let manager = Arc::new(Self {
            runtime: VmManager::new().context("initialize AxVM runtime")?,
            builtin: OnceLock::new(),
            host_filesystem_released: Mutex::new(false),
            lifecycle: Mutex::new(()),
            cpu_freq_backend: Arc::new(crate::guest_cpufreq::GuestCpuFreqService::new()),
        });
        APPLICATION_MANAGER
            .set(manager.clone())
            .map_err(|_| anyhow::anyhow!("application manager is already installed"))?;
        Ok(manager)
    }

    /// Indexes builtin configurations only; it never creates or starts a VM.
    pub fn init_builtin_catalog(&self) -> Result<()> {
        let catalog = crate::config::load_builtin_catalog()?;
        self.builtin
            .set(catalog)
            .map_err(|_| anyhow!("builtin guest catalog is already initialized"))?;
        Ok(())
    }

    pub fn builtin_catalog(&self) -> &BuiltinCatalog {
        self.builtin
            .get()
            .expect("builtin guest catalog is initialized before services start")
    }

    fn create_vm_from_toml_locked(&self, raw_cfg: &str) -> Result<(VmOperation<VmHandle>, bool)> {
        let prepared = crate::config::prepare_guest_vm(raw_cfg, self.cpu_freq_backend.clone())?;
        let release_host_filesystem = prepared.release_host_filesystem;
        let operation = self.create_plan_locked(prepared.plan)?;
        Ok((operation, release_host_filesystem))
    }

    /// Creates one VM and waits until its control task has finished setup.
    pub fn create_vm_from_toml_and_wait(&self, raw_cfg: &str) -> Result<VmHandle> {
        let _lifecycle = self.lifecycle.lock_unpoisoned();
        let (operation, release_host_filesystem) = self.create_vm_from_toml_locked(raw_cfg)?;
        self.complete_created_vm_locked(operation, release_host_filesystem)
    }

    /// Allocates a free ID and completes creation under the same lifecycle
    /// lock. Candidate IDs stay available for their builtin or pool configs;
    /// failed creation follows the normal registry and console-lane rollback.
    #[cfg(feature = "web")]
    pub fn create_vm_with_auto_id_and_wait(
        &self,
        mut config: axvmconfig::GuestConfig,
    ) -> Result<VmHandle> {
        let _lifecycle = self.lifecycle.lock_unpoisoned();
        let mut occupied: BTreeSet<_> = self.list().iter().map(|vm| vm.vm_id()).collect();
        occupied.extend(self.builtin_catalog().entries().map(|entry| entry.id()));
        if !*self.host_filesystem_released.lock_unpoisoned() {
            let pool = crate::vm_pool::scan_with_builtin(self.builtin_catalog());
            occupied.extend(pool.entries().iter().map(|entry| entry.id()));
        }
        let mut id = 1;
        while occupied.contains(&id) {
            id = id.checked_add(1).context("no free VM ID")?;
        }
        config.base.id = id;
        let raw = toml::to_string(&config).context("serialize automatically numbered VM")?;
        let (operation, release_host_filesystem) = self.create_vm_from_toml_locked(&raw)?;
        self.complete_created_vm_locked(operation, release_host_filesystem)
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

    /// Waits for VM initialization and cleans up a failed creation in every
    /// build. A failed operation may retain its registry entry until an
    /// explicit destroy, so returning early would make a later retry collide
    /// with a stale VM ID or console lane.
    fn wait_for_created_vm_locked(&self, operation: VmOperation<VmHandle>) -> Result<VmHandle> {
        let vm_id = operation.id().vm().vm_id();
        match operation.wait() {
            Ok(vm) => Ok(vm),
            Err(error) => {
                // AxVM keeps a failed creation in the registry so the caller
                // can inspect its snapshot. Remove that failed entry before
                // returning, otherwise a retry collides with a stale VM ID.
                if let Err(cleanup_error) = self.cleanup_created_vms_locked(&[vm_id]) {
                    warn!("VM[{vm_id}] creation cleanup failed: {cleanup_error}");
                }
                Err(error.into())
            }
        }
    }

    fn complete_created_vm_locked(
        &self,
        operation: VmOperation<VmHandle>,
        release_host_filesystem: bool,
    ) -> Result<VmHandle> {
        let vm_id = operation.id().vm().vm_id();
        let vm = self.wait_for_created_vm_locked(operation)?;
        let release_host_filesystem = guest_took_host_storage(&vm, release_host_filesystem);
        if release_host_filesystem {
            if let Err(error) = self.release_host_filesystem_for_guest_passthrough_locked() {
                if let Err(cleanup_error) = self.cleanup_created_vms_locked(&[vm_id]) {
                    warn!("VM[{vm_id}] release cleanup failed: {cleanup_error}");
                }
                return Err(error);
            }
        }
        Ok(vm)
    }

    fn cleanup_created_vms_locked(&self, ids: &[VMId]) -> Result<()> {
        let mut first_error = None;
        for &vm_id in ids.iter().rev() {
            let Some(vm) = self.get(vm_id) else {
                continue;
            };
            let console_backend = crate::guest_console::backend_identity(vm_id);
            let destroyed = if let Err(error) = vm.destroy().and_then(VmOperation::wait) {
                warn!("VM[{vm_id}] startup rollback failed: {error}");
                if first_error.is_none() {
                    first_error = Some(anyhow!(error).context(format!("destroy VM[{vm_id}]")));
                }
                false
            } else {
                true
            };
            let joined = if let Err(error) = vm.join_control_task() {
                warn!("VM[{vm_id}] startup rollback join failed: {error}");
                if first_error.is_none() {
                    first_error = Some(anyhow!(error).context(format!("join VM[{vm_id}]")));
                }
                false
            } else {
                true
            };
            if !destroyed || !joined {
                continue;
            }
            #[cfg(feature = "web")]
            crate::control::network_console::release_guest(vm_id);
            if let Some(identity) = console_backend {
                crate::guest_console::remove_if_backend(identity);
            }
            crate::guest_console::mark_stopped(vm_id);
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Ensures that `vm_id` is registered, preferring the immutable builtin
    /// catalog and then the current dynamic pool. Listing a candidate never
    /// allocates a VM or reserves a console lane.
    pub fn ensure_vm(&self, vm_id: VMId) -> Result<VmHandle> {
        let _lifecycle = self.lifecycle.lock_unpoisoned();
        if let Some(vm) = self.get(vm_id) {
            return Ok(vm);
        }

        if let Some(entry) = self.builtin_catalog().get(vm_id) {
            let (operation, release_host_filesystem) = self
                .create_vm_from_toml_locked(entry.toml())
                .with_context(|| format!("create builtin VM[{vm_id}] from `{}`", entry.path()))?;
            return self
                .complete_created_vm_locked(operation, release_host_filesystem)
                .with_context(|| format!("initialize builtin VM[{vm_id}]"));
        }

        if *self.host_filesystem_released.lock_unpoisoned() {
            return Err(anyhow!(VmCandidateNotFound { vm_id })
                .context("host filesystem was released; dynamic VM assets are unavailable"));
        }
        let pool = crate::vm_pool::scan_with_builtin(self.builtin_catalog());
        let Some(entry) = pool.entries().iter().find(|entry| entry.id() == vm_id) else {
            crate::vm_pool::log_issues(&pool);
            return Err(anyhow!(VmCandidateNotFound { vm_id }));
        };
        let (operation, release_host_filesystem) = self
            .create_vm_from_toml_locked(entry.toml())
            .with_context(|| format!("create VM[{vm_id}] from VM pool entry `{}`", entry.path()))?;
        self.complete_created_vm_locked(operation, release_host_filesystem)
            .with_context(|| {
                format!(
                    "initialize VM[{vm_id}] from VM pool entry `{}`",
                    entry.path()
                )
            })
    }

    /// Creates and starts the selected builtin guests in command-line order.
    /// Every ID is validated before the first VM is created. Any guest created
    /// by this selector is destroyed when a later operation fails.
    pub fn start_selected_vms(&self, ids: &[VMId]) -> Result<()> {
        let _lifecycle = self.lifecycle.lock_unpoisoned();
        for &vm_id in ids {
            if self.builtin_catalog().get(vm_id).is_none() {
                bail!("VM[{vm_id}] selected by the host selector is not builtin");
            }
        }

        let mut created = Vec::new();
        let mut release_host_filesystem = false;
        let result = (|| {
            for &vm_id in ids {
                if self.get(vm_id).is_some() {
                    bail!("VM[{vm_id}] is already registered");
                }
                let entry = self
                    .builtin_catalog()
                    .get(vm_id)
                    .expect("builtin ID was validated above");
                let (operation, needs_release) = self
                    .create_vm_from_toml_locked(entry.toml())
                    .with_context(|| {
                        format!("create builtin VM[{vm_id}] from `{}`", entry.path())
                    })?;
                let vm = self
                    .wait_for_created_vm_locked(operation)
                    .with_context(|| format!("initialize builtin VM[{vm_id}]"))?;
                release_host_filesystem |= guest_took_host_storage(&vm, needs_release);
                created.push(vm_id);
            }
            if release_host_filesystem {
                self.release_host_filesystem_for_guest_passthrough_locked()?;
            }
            for &vm_id in ids {
                self.require_vm(vm_id)?
                    .start()?
                    .wait()
                    .with_context(|| format!("start builtin VM[{vm_id}]"))?;
                crate::guest_console::mark_running(vm_id);
            }
            Ok(())
        })();

        if let Err(error) = result {
            if let Err(cleanup_error) = self.cleanup_created_vms_locked(&created) {
                warn!("startup rollback encountered another error: {cleanup_error}");
            }
            return Err(error);
        }
        Ok(())
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
            .context("stop VM")?;
        crate::guest_console::mark_stopped(vm_id);
        Ok(())
    }

    pub fn start_vm(&self, vm_id: VMId) -> Result<()> {
        let _lifecycle = self.lifecycle.lock_unpoisoned();
        self.require_vm(vm_id)?
            .start()?
            .wait()
            .map(|_run| ())
            .context("start VM")?;
        crate::guest_console::mark_running(vm_id);
        Ok(())
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
            .context("resume VM")?;
        crate::guest_console::mark_running(vm_id);
        Ok(())
    }

    pub fn reset_vm(&self, vm_id: VMId) -> Result<()> {
        let _lifecycle = self.lifecycle.lock_unpoisoned();
        self.require_vm(vm_id)?
            .reset()?
            .wait()
            .map(|_| ())
            .context("reset VM")?;
        crate::guest_console::mark_running(vm_id);
        Ok(())
    }

    pub fn destroy_vm(&self, vm_id: VMId) -> Result<()> {
        let _lifecycle = self.lifecycle.lock_unpoisoned();
        let vm = self.require_vm(vm_id)?;
        let console_backend = crate::guest_console::backend_identity(vm_id);
        vm.destroy()?.wait().context("destroy VM")?;
        vm.join_control_task().context("join VM control task")?;
        #[cfg(feature = "web")]
        crate::control::network_console::release_guest(vm_id);
        if let Some(identity) = console_backend {
            crate::guest_console::remove_if_backend(identity);
        }
        crate::guest_console::mark_stopped(vm_id);
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
    fn release_host_filesystem_for_guest_passthrough_locked(&self) -> Result<()> {
        let mut released = self.host_filesystem_released.lock_unpoisoned();
        if *released {
            return Ok(());
        }

        axvm::host::shutdown_filesystems()
            .context("release host filesystem before guest passthrough")?;
        axvm::host::prepare_block_passthrough_device();
        *released = true;
        info!("Host filesystem cleanly unmounted before guest passthrough devices start");
        Ok(())
    }

    #[cfg(not(any(
        target_arch = "aarch64",
        target_arch = "x86_64",
        target_arch = "loongarch64"
    )))]
    fn release_host_filesystem_for_guest_passthrough_locked(&self) -> Result<()> {
        Ok(())
    }

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
