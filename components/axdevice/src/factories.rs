//! Factory implementations for all emulated devices.
//!
//! Each factory creates a [`DeviceBundle`] from its `EmulatedDeviceConfig`.
//! The factories are registered in `FactoryRegistry` and used to eliminate
//! the giant `match` in `AxVmDevices::init()`.
//!
//! ```rust,ignore
//! let mut factories = axbus::FactoryRegistry::new();
//! axdevice::factories::register_all_factories(&mut factories);
//!
//! for config in &vm_config.emu_devices {
//!     let bundle = factories.create(config.emu_type, config, &mut id_alloc)?;
//!     for dev in bundle.devices {
//!         router.register(Arc::from(dev))?;
//!     }
//!     if let Some(intc) = bundle.intc {
//!         router.register_intc(id, intc);
//!     }
//! }
//! ```

#![allow(unused_variables, unused_imports)]

use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::{any::Any, ops::Range};

use ax_memory_addr::PhysAddr;
use axaddrspace::GuestPhysAddr;
use axbus::{
    BusAccess, BusKind, BusResponse, DeviceBundle, DeviceFactory, DeviceId, EmulatedDeviceConfig,
    InterruptControllerOps, IrqLine, LegacyMmioAdapter, Resource, VirtualDevice,
};
use axdevice_base::{BaseDeviceOps, BaseMmioDeviceOps};
use axvmconfig::EmulatedDeviceType;

use crate::*;

// ── aarch64 GICv2 InterruptController ──────────────────────

/// Factory for ARM GICv2 virtual interrupt controller.
///
/// The `Vgic` serves as both an MMIO bus device and an interrupt controller,
/// so the bundle carries both roles via `DeviceBundle::with_intc`.
pub struct VgicFactory;

impl DeviceFactory for VgicFactory {
    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::InterruptController
    }

    fn create(
        &self,
        config: &EmulatedDeviceConfig,
        id_alloc: &mut dyn FnMut() -> DeviceId,
    ) -> axbus::Result<DeviceBundle> {
        #[cfg(target_arch = "aarch64")]
        {
            let vgic = Arc::new(arm_vgic::Vgic::new());
            let intc: Arc<dyn InterruptControllerOps> = vgic.clone();
            let dev: Arc<dyn BaseMmioDeviceOps> = vgic;
            let id = id_alloc();
            Ok(DeviceBundle::with_intc(
                Box::new(LegacyMmioAdapter::new(id, dev)),
                intc,
            ))
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            Err(axbus::DeviceError::BackendError(
                "GICv2 not supported on this arch".into(),
            ))
        }
    }
}

// ── aarch64 GICv3 Redistributor (GPPT) ─────────────────────

/// Factory for GICv3 partial passthrough redistributor.
///
/// A single config entry produces N VGicR instances (one per vCPU),
/// returned via `DeviceBundle::multi`.
pub struct VGicRFactory;

impl DeviceFactory for VGicRFactory {
    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::GPPTRedistributor
    }

    fn create(
        &self,
        config: &EmulatedDeviceConfig,
        id_alloc: &mut dyn FnMut() -> DeviceId,
    ) -> axbus::Result<DeviceBundle> {
        #[cfg(target_arch = "aarch64")]
        {
            const ERR: &str = "expect 3 args for GPPT redistributor (cpu_num, stride, pcpu_id)";
            let cpu_num = config.cfg_list.first().copied().expect(ERR);
            let stride = config.cfg_list.get(1).copied().expect(ERR);
            let pcpu_id = config.cfg_list.get(2).copied().expect(ERR);

            let mut devices: Vec<Box<dyn VirtualDevice>> = Vec::with_capacity(cpu_num);
            for i in 0..cpu_num {
                let addr = config.base_gpa + i * stride;
                let size = config.length;
                #[allow(clippy::arc_with_non_send_sync)]
                let dev: Arc<dyn BaseMmioDeviceOps> = Arc::new(arm_vgic::v3::vgicr::VGicR::new(
                    addr.into(),
                    Some(size),
                    pcpu_id + i,
                ));
                let id = id_alloc();
                devices.push(Box::new(LegacyMmioAdapter::new(id, dev)));

                info!(
                    "GPPT Redistributor initialized for vCPU {i} with base GPA {addr:#x} and \
                     length {size:#x}"
                );
            }
            Ok(DeviceBundle::multi(devices))
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
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
    ) -> axbus::Result<DeviceBundle> {
        #[cfg(target_arch = "aarch64")]
        {
            #[allow(clippy::arc_with_non_send_sync)]
            let dev: Arc<dyn BaseMmioDeviceOps> = Arc::new(arm_vgic::v3::vgicd::VGicD::new(
                config.base_gpa.into(),
                Some(config.length),
            ));
            let id = id_alloc();

            info!(
                "GPPT Distributor initialized with base GPA {base_gpa:#x} and length {length:#x}",
                base_gpa = config.base_gpa,
                length = config.length
            );

            Ok(DeviceBundle::single(Box::new(LegacyMmioAdapter::new(
                id, dev,
            ))))
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
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
    ) -> axbus::Result<DeviceBundle> {
        #[cfg(target_arch = "aarch64")]
        {
            let host_gits_base = config
                .cfg_list
                .first()
                .copied()
                .map(PhysAddr::from_usize)
                .expect("expect 1 arg for GPPT ITS (host_gits_base)");

            #[allow(clippy::arc_with_non_send_sync)]
            let dev: Arc<dyn BaseMmioDeviceOps> = Arc::new(arm_vgic::v3::gits::Gits::new(
                config.base_gpa.into(),
                Some(config.length),
                host_gits_base,
                false,
            ));
            let id = id_alloc();

            info!(
                "GPPT ITS initialized with base GPA {base_gpa:#x} and length {length:#x}, host \
                 GITS base {host_gits_base:#x}",
                base_gpa = config.base_gpa,
                length = config.length,
                host_gits_base = host_gits_base
            );

            Ok(DeviceBundle::single(Box::new(LegacyMmioAdapter::new(
                id, dev,
            ))))
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            Err(axbus::DeviceError::BackendError(
                "GICv3 ITS not supported on this arch".into(),
            ))
        }
    }
}

// ── riscv64 PLIC Partial Passthrough ────────────────────────

/// Factory for RISC-V PLIC partial passthrough global.
///
/// `VPlicGlobal` serves as both an MMIO bus device and an interrupt
/// controller, so the bundle carries both roles.
pub struct VPlicGlobalFactory;

impl DeviceFactory for VPlicGlobalFactory {
    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::PPPTGlobal
    }

    fn create(
        &self,
        config: &EmulatedDeviceConfig,
        id_alloc: &mut dyn FnMut() -> DeviceId,
    ) -> axbus::Result<DeviceBundle> {
        #[cfg(target_arch = "riscv64")]
        {
            let context_num = config
                .cfg_list
                .first()
                .copied()
                .expect("expect 1 arg for PPPT global (context_num)");

            let vplic = Arc::new(riscv_vplic::VPlicGlobal::new(
                config.base_gpa.into(),
                Some(config.length),
                context_num,
            ));
            let intc: Arc<dyn InterruptControllerOps> = vplic.clone();
            let dev: Arc<dyn BaseMmioDeviceOps> = vplic;
            let id = id_alloc();

            info!(
                "Partial PLIC Passthrough Global initialized with base GPA {:#x} and length {:#x}",
                config.base_gpa, config.length
            );

            Ok(DeviceBundle::with_intc(
                Box::new(LegacyMmioAdapter::new(id, dev)),
                intc,
            ))
        }
        #[cfg(not(target_arch = "riscv64"))]
        {
            Err(axbus::DeviceError::BackendError(
                "PLIC not supported on this arch".into(),
            ))
        }
    }
}

// ── Virtio MMIO stub device (native VirtualDevice) ─────────

/// A minimal Virtio MMIO device that reserves the MMIO region and reports
/// standard identification values (Magic, Version, Device ID, Vendor ID).
///
/// This is a **stub** — it allows the guest to detect the device in the
/// device tree but does not implement a functional Virtio backend. The
/// stub prevents address-space conflicts in the `DeviceRegistry` and
/// provides the correct identification layout so that FDT generation and
/// Linux driver enumeration work correctly.
///
/// This is the **first device migrated from `LegacyMmioAdapter` to native
/// `VirtualDevice`** — it demonstrates the migration path for other devices.
///
/// # Resources
///
/// - `Resource::Mmio`: the MMIO region `[base, base + length)`
/// - `Resource::Irq`: the declared IRQ line (if `irq > 0`)
///
/// # Future work
///
/// Replace with a full Virtio backend (transport over MMIO or PCI) that
/// implements queue processing, descriptor rings, and interrupt signaling
/// via `IrqSink`.
#[derive(Debug)]
struct VirtioMmioStub {
    /// Device ID assigned at registration time.
    id: DeviceId,
    /// Guest physical base address.
    base: usize,
    /// Length of the MMIO region in bytes.
    length: usize,
    /// Device type (0x1001 = block, 0x1000 = net, 0x1003 = console, etc.).
    device_id: u32,
    /// Pre-computed resource list (MMIO region + optional IRQ).
    resources: Vec<Resource>,
}

impl VirtioMmioStub {
    const VIRTIO_MMIO_MAGIC: u32 = 0x74726976; // "virt" little-endian

    fn new(id: DeviceId, base: usize, length: usize, device_id: u32, irq: u32) -> Self {
        let mut resources = Vec::with_capacity(2);
        resources.push(Resource::Mmio(base as u64..(base + length) as u64));
        if irq > 0 {
            resources.push(Resource::Irq(IrqLine(irq)));
        }
        Self {
            id,
            base,
            length,
            device_id,
            resources,
        }
    }

    /// Map a guest offset (relative to base) to a read value.
    fn read_register(&self, offset: usize) -> u64 {
        match offset {
            0x000 => Self::VIRTIO_MMIO_MAGIC as u64, // MagicValue
            0x004 => 2,                              // Version (modern MMIO)
            0x008 => self.device_id as u64,          // DeviceID
            0x00C => 0x1AF4,                         // VendorID (Red Hat)
            _ => 0,
        }
    }
}

impl VirtualDevice for VirtioMmioStub {
    fn id(&self) -> DeviceId {
        self.id
    }

    fn name(&self) -> &str {
        "virtio-mmio-stub"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn handle_access(&self, bus: BusKind, access: &BusAccess) -> BusResponse {
        if bus != BusKind::Mmio {
            return BusResponse::NoDevice {
                bus,
                addr: access.addr(),
            };
        }
        match access {
            BusAccess::Read { addr, .. } => {
                let offset = (*addr as usize).wrapping_sub(self.base);
                if offset < self.length {
                    BusResponse::Success(Some(self.read_register(offset)))
                } else {
                    BusResponse::NoDevice { bus, addr: *addr }
                }
            }
            BusAccess::Write { .. } => {
                // Stub accepts writes silently — no functional backend.
                BusResponse::Success(None)
            }
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ── Virtio block factory ──────────────────────────────────

/// Factory for Virtio Block device (stub, native VirtualDevice).
pub struct VirtioBlkFactory;

impl DeviceFactory for VirtioBlkFactory {
    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::VirtioBlk
    }

    fn create(
        &self,
        config: &EmulatedDeviceConfig,
        id_alloc: &mut dyn FnMut() -> DeviceId,
    ) -> axbus::Result<DeviceBundle> {
        let id = id_alloc();
        Ok(DeviceBundle {
            devices: vec![Box::new(VirtioMmioStub::new(
                id,
                config.base_gpa,
                config.length,
                0x1001, // Virtio ID = block
                config.irq_id as u32,
            )) as Box<dyn VirtualDevice>],
            intc: None,
        })
    }
}

// ── Virtio net factory ────────────────────────────────────

/// Factory for Virtio Network device (stub, native VirtualDevice).
pub struct VirtioNetFactory;

impl DeviceFactory for VirtioNetFactory {
    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::VirtioNet
    }

    fn create(
        &self,
        config: &EmulatedDeviceConfig,
        id_alloc: &mut dyn FnMut() -> DeviceId,
    ) -> axbus::Result<DeviceBundle> {
        let id = id_alloc();
        Ok(DeviceBundle {
            devices: vec![Box::new(VirtioMmioStub::new(
                id,
                config.base_gpa,
                config.length,
                0x1000, // Virtio ID = net
                config.irq_id as u32,
            )) as Box<dyn VirtualDevice>],
            intc: None,
        })
    }
}

// ── Virtio console factory ────────────────────────────────

/// Factory for Virtio Console device (stub, native VirtualDevice).
pub struct VirtioConsoleFactory;

impl DeviceFactory for VirtioConsoleFactory {
    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::VirtioConsole
    }

    fn create(
        &self,
        config: &EmulatedDeviceConfig,
        id_alloc: &mut dyn FnMut() -> DeviceId,
    ) -> axbus::Result<DeviceBundle> {
        let id = id_alloc();
        Ok(DeviceBundle {
            devices: vec![Box::new(VirtioMmioStub::new(
                id,
                config.base_gpa,
                config.length,
                0x1003, // Virtio ID = console
                config.irq_id as u32,
            )) as Box<dyn VirtualDevice>],
            intc: None,
        })
    }
}

// ── x86_64 InterruptController stub ────────────────────────

/// Stub factory for x86_64 `InterruptController` config entries.
///
/// On x86, the per-vCPU `EmulatedLocalApic` is created inside VmxVcpu/SvmVcpu,
/// and the VM-level `X86IntcAdapter` is registered directly in `vm.rs` using
/// the VM's own id. This factory returns an empty bundle to acknowledge the
/// config entry without producing any device.
pub struct VLapicFactory;

impl DeviceFactory for VLapicFactory {
    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::InterruptController
    }

    fn create(
        &self,
        config: &EmulatedDeviceConfig,
        id_alloc: &mut dyn FnMut() -> DeviceId,
    ) -> axbus::Result<DeviceBundle> {
        Ok(DeviceBundle::empty())
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

    // x86_64 stub (acknowledges InterruptController config, intc created in vm.rs)
    register_if!(target_arch = "x86_64", VLapicFactory);

    // — Virtio stubs (all architectures) —
    register_if!(all(), VirtioBlkFactory);
    register_if!(all(), VirtioNetFactory);
    register_if!(all(), VirtioConsoleFactory);
}
