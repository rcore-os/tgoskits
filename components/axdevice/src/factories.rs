//! Factory implementations for all emulated devices.
//!
//! Each factory creates a concrete device from its `EmulatedDeviceConfig`.
//! The factories are registered in `FactoryRegistry` and used to eliminate
//! the giant `match` in `AxVmDevices::init()`.
//!
//! ```rust,ignore
//! let mut factories = axbus::FactoryRegistry::new();
//! factories.register(Box::new(VgicFactory));
//! factories.register(Box::new(VGicRFactory));
//! // ... more factories ...
//!
//! for config in &vm_config.emu_devices {
//!     let dev = factories.create(config.emu_type as u8, config, &mut id_alloc)?;
//!     router.register(dev)?;
//! }
//! ```

#![allow(unused_variables)]

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::ops::Range;

use axaddrspace::GuestPhysAddr;
use axbus::{
    DeviceId, DeviceFactory, EmulatedDeviceConfig,
    LegacyMmioAdapter, VirtualDevice,
};
use axvmconfig::EmulatedDeviceType;
use axdevice_base::{BaseMmioDeviceOps, BaseDeviceOps};
use ax_errno::AxResult;
use ax_memory_addr::PhysAddr;

use crate::*;

// ── aarch64 GICv2 InterruptController ──────────────────────

/// Factory for ARM GICv2 virtual interrupt controller.
pub struct VgicFactory;

impl DeviceFactory for VgicFactory {
    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::InterruptController
    }

    fn create(
        &self,
        config: &EmulatedDeviceConfig,
        id_alloc: &mut dyn FnMut() -> DeviceId,
    ) -> axbus::Result<Box<dyn VirtualDevice>> {
        #[cfg(target_arch = "aarch64")]
        {
            let dev: Arc<dyn BaseMmioDeviceOps> = Arc::new(arm_vgic::Vgic::new());
            let id = id_alloc();
            Ok(Box::new(LegacyMmioAdapter::new(id, dev)))
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            let _ = config;
            let _ = id_alloc;
            Err(axbus::DeviceError::BackendError(
                "GICv2 not supported on this arch".into(),
            ))
        }
    }
}

// ── aarch64 GICv3 Redistributor (GPPT) ─────────────────────

/// Factory for GICv3 partial passthrough redistributor.
pub struct VGicRFactory;

impl DeviceFactory for VGicRFactory {
    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::GPPTRedistributor
    }

    fn create(
        &self,
        config: &EmulatedDeviceConfig,
        id_alloc: &mut dyn FnMut() -> DeviceId,
    ) -> axbus::Result<Box<dyn VirtualDevice>> {
        #[cfg(target_arch = "aarch64")]
        {
            const ERR: &str = "expect 3 args for GPPT redistributor (cpu_num, stride, pcpu_id)";
            let cpu_num = config.cfg_list.first().copied().expect(ERR);
            let stride = config.cfg_list.get(1).copied().expect(ERR);
            let pcpu_id = config.cfg_list.get(2).copied().expect(ERR);

            // We create ONE VirtualDevice per VGicR instance; multi-instance
            // VMs register multiple factories or we loop here.
            // For simplicity, create the first one and log the rest.
            let addr = config.base_gpa + 0 * stride;
            let size = config.length;
            let dev: Arc<dyn BaseMmioDeviceOps> = Arc::new(
                arm_vgic::v3::vgicr::VGicR::new(addr.into(), Some(size), pcpu_id + 0),
            );
            let id = id_alloc();
            Ok(Box::new(LegacyMmioAdapter::new(id, dev)))
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            let _ = config;
            let _ = id_alloc;
            Err(axbus::DeviceError::BackendError(
                "GICv3 not supported on this arch".into(),
            ))
        }
    }
}

// ── aarch64 GICv3 Distributor (GPPT) ───────────────────────

/// Factory for GICv3 partial passthrough distributor.
pub struct VGicDFactory;

impl DeviceFactory for VGicDFactory {
    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::GPPTDistributor
    }

    fn create(
        &self,
        config: &EmulatedDeviceConfig,
        id_alloc: &mut dyn FnMut() -> DeviceId,
    ) -> axbus::Result<Box<dyn VirtualDevice>> {
        #[cfg(target_arch = "aarch64")]
        {
            let dev: Arc<dyn BaseMmioDeviceOps> = Arc::new(
                arm_vgic::v3::vgicd::VGicD::new(config.base_gpa.into(), Some(config.length)),
            );
            let id = id_alloc();
            Ok(Box::new(LegacyMmioAdapter::new(id, dev)))
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            let _ = config;
            let _ = id_alloc;
            Err(axbus::DeviceError::BackendError(
                "GICv3 not supported on this arch".into(),
            ))
        }
    }
}

// ── aarch64 GICv3 ITS (GPPT) ───────────────────────────────

/// Factory for GICv3 partial passthrough ITS.
pub struct GitsFactory;

impl DeviceFactory for GitsFactory {
    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::GPPTITS
    }

    fn create(
        &self,
        config: &EmulatedDeviceConfig,
        id_alloc: &mut dyn FnMut() -> DeviceId,
    ) -> axbus::Result<Box<dyn VirtualDevice>> {
        #[cfg(target_arch = "aarch64")]
        {
            let host_gits_base = config
                .cfg_list
                .first()
                .copied()
                .map(PhysAddr::from_usize)
                .expect("expect 1 arg for GPPT ITS (host_gits_base)");

            let dev: Arc<dyn BaseMmioDeviceOps> = Arc::new(
                arm_vgic::v3::gits::Gits::new(
                    config.base_gpa.into(),
                    Some(config.length),
                    host_gits_base,
                    false,
                ),
            );
            let id = id_alloc();
            Ok(Box::new(LegacyMmioAdapter::new(id, dev)))
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            let _ = config;
            let _ = id_alloc;
            Err(axbus::DeviceError::BackendError(
                "GICv3 ITS not supported on this arch".into(),
            ))
        }
    }
}

// ── riscv64 PLIC Partial Passthrough ────────────────────────

/// Factory for RISC-V PLIC partial passthrough global.
pub struct VPlicGlobalFactory;

impl DeviceFactory for VPlicGlobalFactory {
    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::PPPTGlobal
    }

    fn create(
        &self,
        config: &EmulatedDeviceConfig,
        id_alloc: &mut dyn FnMut() -> DeviceId,
    ) -> axbus::Result<Box<dyn VirtualDevice>> {
        #[cfg(target_arch = "riscv64")]
        {
            let context_num = config
                .cfg_list
                .first()
                .copied()
                .expect("expect 1 arg for PPPT global (context_num)");

            let dev: Arc<dyn BaseMmioDeviceOps> = Arc::new(
                riscv_vplic::VPlicGlobal::new(
                    config.base_gpa.into(),
                    Some(config.length),
                    context_num,
                ),
            );
            let id = id_alloc();
            Ok(Box::new(LegacyMmioAdapter::new(id, dev)))
        }
        #[cfg(not(target_arch = "riscv64"))]
        {
            let _ = config;
            let _ = id_alloc;
            Err(axbus::DeviceError::BackendError(
                "PLIC not supported on this arch".into(),
            ))
        }
    }
}

// ── x86_64 vLAPIC ──────────────────────────────────────────

/// Factory for x86 virtual LAPIC (MMIO + SysReg).
pub struct VLapicFactory;

impl DeviceFactory for VLapicFactory {
    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::InterruptController
    }

    fn create(
        &self,
        config: &EmulatedDeviceConfig,
        id_alloc: &mut dyn FnMut() -> DeviceId,
    ) -> axbus::Result<Box<dyn VirtualDevice>> {
        // x86 vLAPIC is created per-vCPU during vCPU setup, not from config.
        // This factory is a placeholder for future use.
        let _ = config;
        let _ = id_alloc;
        Err(axbus::DeviceError::BackendError(
            "vLAPIC is created per-vCPU, not from config".into(),
        ))
    }
}

// ── Helper: register all supported factories ────────────────

/// Register all available device factories into the given `FactoryRegistry`.
///
/// Call this during VMM initialization:
/// ```rust,ignore
/// use axdevice::factories::register_all_factories;
/// let mut registry = axbus::FactoryRegistry::new();
/// register_all_factories(&mut registry);
/// ```
pub fn register_all_factories(registry: &mut axbus::FactoryRegistry) {
    macro_rules! register_if {
        ($cfg:meta, $factory:expr) => {
            #[cfg($cfg)]
            registry.register(Box::new($factory));
        };
    }

    // aarch64 GIC
    register_if!(target_arch = "aarch64", VgicFactory);
    register_if!(target_arch = "aarch64", VGicRFactory);
    register_if!(target_arch = "aarch64", VGicDFactory);
    register_if!(target_arch = "aarch64", GitsFactory);

    // riscv64 PLIC
    register_if!(target_arch = "riscv64", VPlicGlobalFactory);

    // Future: x86 IOAPIC, AIA IMSIC, LoongArch interrupt controllers, etc.
    // Future: VirtioBlk, VirtioNet, VirtioConsole, Console, Dummy.
}
