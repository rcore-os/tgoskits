#![no_std]

#[macro_use]
extern crate alloc;
#[macro_use]
extern crate log;

use core::ptr::NonNull;

pub use fdt_edit::{Fdt, Phandle};
use register::{DriverRegister, ProbeLevel};
use spin::{Mutex, Once};

mod descriptor;
pub mod driver;
pub mod error;
mod id;
mod lock;
mod manager;
mod osal;

pub mod probe;
pub mod register;

pub use descriptor::*;
pub use driver::PlatformDevice;
pub use lock::*;
pub use manager::*;
pub use osal::*;
pub use probe::ProbeError;
pub use rdif_base::{DriverGeneric, KError, irq::IrqId};
pub use rdrive_macros::*;

use crate::{error::DriverError, probe::OnProbeError};

static CONTAINER: Once<Mutex<Manager>> = Once::new();

#[derive(Debug, Clone)]
pub enum Platform {
    Static,
    Fdt { addr: NonNull<u8> },
    Acpi(probe::acpi::AcpiRoot),
}

unsafe impl Send for Platform {}

#[derive(Debug, Clone, Copy)]
pub enum PlatformSource {
    Static,
    Fdt(NonNull<u8>),
    Acpi(probe::acpi::AcpiRoot),
}

unsafe impl Send for PlatformSource {}

pub(crate) fn container() -> &'static Mutex<Manager> {
    CONTAINER.get().expect("rdrive not init")
}

pub fn is_initialized() -> bool {
    CONTAINER.get().is_some()
}

pub fn init(platform: Platform) -> Result<(), DriverError> {
    match platform {
        Platform::Static => init_sources(&[PlatformSource::Static])?,
        Platform::Fdt { addr } => init_sources(&[PlatformSource::Fdt(addr)])?,
        Platform::Acpi(root) => init_sources(&[PlatformSource::Acpi(root)])?,
    }
    Ok(())
}

pub fn init_sources(sources: &[PlatformSource]) -> Result<(), DriverError> {
    for source in sources {
        match source {
            PlatformSource::Static => {}
            PlatformSource::Fdt(addr) => probe::fdt::check_addr(*addr)?,
            PlatformSource::Acpi(root) => probe::acpi::check_root(*root)?,
        }
    }

    for source in sources {
        match source {
            PlatformSource::Static => probe::static_::init()?,
            PlatformSource::Fdt(addr) => probe::fdt::init(*addr)?,
            PlatformSource::Acpi(root) => probe::acpi::init(*root)?,
        }
    }

    let m = Manager::new()?;
    CONTAINER.call_once(|| Mutex::new(m));
    Ok(())
}

pub(crate) fn edit<F, T>(f: F) -> T
where
    F: FnOnce(&mut Manager) -> T,
{
    let mut g = container().lock();
    f(&mut g)
}

pub(crate) fn read<F, T>(f: F) -> T
where
    F: FnOnce(&Manager) -> T,
{
    let g = container().lock();
    f(&g)
}

pub fn register_add(register: DriverRegister) {
    edit(|manager| manager.registers.add(register));
}

pub fn register_append(registers: &[DriverRegister]) {
    edit(|manager| manager.registers.append(registers))
}

pub fn probe_pre_kernel() -> Result<(), ProbeError> {
    let unregistered = edit(|manager| manager.unregistered())?;

    let ls = unregistered
        .iter()
        .filter(|one| matches!(one.level, ProbeLevel::PreKernel));

    probe_system(ls, true)?;

    Ok(())
}

fn probe_system<'a>(
    registers: impl Iterator<Item = &'a DriverRegister>,
    stop_if_fail: bool,
) -> Result<(), ProbeError> {
    for one in registers {
        probe_backend(one, probe::static_::try_probe_register(one), stop_if_fail)?;
        probe_backend(one, probe::fdt::try_probe_register(one), stop_if_fail)?;
        probe_backend(one, probe::acpi::try_probe_register(one), stop_if_fail)?;
    }

    Ok(())
}

fn probe_backend(
    register: &DriverRegister,
    results: Option<Result<Vec<Result<(), OnProbeError>>, ProbeError>>,
    stop_if_fail: bool,
) -> Result<(), ProbeError> {
    let Some(results) = results else {
        return Ok(());
    };

    for r in results? {
        match r {
            Ok(_) => {}
            Err(OnProbeError::NotMatch) => {}
            Err(e) => {
                if stop_if_fail {
                    return Err(e.into());
                } else {
                    warn!("Probe failed for [{}]: {}", register.name, e);
                }
            }
        }
    }

    Ok(())
}

pub fn probe_all(stop_if_fail: bool) -> Result<(), ProbeError> {
    let unregistered = edit(|manager| manager.unregistered())?;
    probe_system(unregistered.iter(), stop_if_fail)?;

    debug!("probe pci devices");
    probe::pci::probe_with(&unregistered, stop_if_fail)?;

    Ok(())
}

pub fn get_list<T: DriverGeneric>() -> Vec<Device<T>> {
    read(|manager| manager.dev_container.devices())
}

pub fn get<T: DriverGeneric>(id: DeviceId) -> Result<Device<T>, GetDeviceError> {
    read(|manager| manager.dev_container.get_typed(id))
}

pub fn get_one<T: DriverGeneric>() -> Option<Device<T>> {
    read(|manager| manager.dev_container.get_one())
}

pub fn fdt_phandle_to_device_id(phandle: Phandle) -> Option<DeviceId> {
    probe::fdt::try_system().and_then(|system| system.phandle_to_device_id(phandle))
}

pub fn with_fdt<T>(f: impl FnOnce(&Fdt) -> T) -> Option<T> {
    probe::fdt::try_system().map(|system| f(system.fdt()))
}

/// Macro for generating a driver module.
///
/// This macro automatically generates a driver registration module that creates a static
/// `DriverRegister` struct containing driver metadata (such as name, probe level, priority,
/// and probe types). The generated static variable is placed in the special linker section
/// `.driver.register` to be automatically discovered and registered by the driver manager
/// at runtime.
///
/// # Parameters
/// - `$i:ident`: Field identifier (e.g., `name`, `level`, `priority`, `probe_kinds`)
/// - `$t:expr`: Expression for the corresponding field value
///
/// # Generated Code
///
/// The macro generates a module containing a static `DriverRegister` that:
/// - Uses `#[link_section = ".driver.register"]` attribute to place it in a special linker section
/// - Uses `#[no_mangle]` and `#[used]` to prevent compiler optimization
/// - Contains all driver registration information
///
/// # Example
///
/// ```rust
/// #![feature(used_with_arg)]
///
/// use rdrive::{
///     PlatformDevice, driver::*, module_driver, probe::OnProbeError, register::FdtInfo,
/// };
///
/// struct ClkDriver {}
///
/// impl DriverGeneric for ClkDriver {
///     fn name(&self) -> &str {
///         "ClkDriver"
///     }
/// }
///
/// impl rdif_clk::Interface for ClkDriver {
///     fn perper_enable(&mut self) {}
///     fn get_rate(&self, _id: rdif_clk::ClockId) -> Result<u64, rdrive::KError> {
///         Ok(1000000)
///     }
///     fn set_rate(&mut self, _id: rdif_clk::ClockId, _rate: u64) -> Result<(), rdrive::KError> {
///         Ok(())
///     }
/// }
///
/// // Define probe function
/// fn probe_clk(fdt: FdtInfo<'_>, dev: PlatformDevice) -> Result<(), OnProbeError> {
///     // Implement specific device probing logic
///     dev.register(rdif_clk::Clk::new(ClkDriver {}));
///     Ok(())
/// }
///
/// // Use macro to generate driver registration module
/// module_driver! {
///     name: "CLK Driver",
///     level: ProbeLevel::PostKernel,
///     priority: ProbePriority::CLK,
///     probe_kinds: &[ProbeKind::Fdt {
///         compatibles: &["fixed-clock"],
///         // Use `probe_clk` above; this usage is because doctests cannot find the parent module.
///         on_probe: |fdt, dev|{
///             Ok(())
///         },
///     }],
/// }
/// ```
///
/// # Notes
///
/// - This macro can only be used once per driver module
/// - The generated module name is automatically derived from the driver name
/// - All fields must be properly set, especially the `probe_kinds` array
/// - Probe functions must implement the correct signature and error handling
#[macro_export]
macro_rules! module_driver {
    (
        $($i:ident : $t:expr),+,
    ) => {
        /// Auto-generated driver registration module.
        #[allow(unused)]
        $crate::__mod_maker!{
            pub mod some {
                use super::*;
                use $crate::register::*;

                /// Static instance of driver registration information.
                ///
                /// This static variable is placed in the `.driver.register` linker section
                /// so that the driver manager can automatically discover and load it during
                /// system startup.
                #[unsafe(link_section = ".driver.register")]
                #[unsafe(no_mangle)]
                #[used(linker)]
                pub static DRIVER: DriverRegister = DriverRegister {
                    $($i : $t),+
                };
            }
        }
    };
}
