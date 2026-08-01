//! Small capability boundaries implemented by the selected guest architecture.

use alloc::vec::Vec;

use crate::{AxVmError, AxVmResult};

/// Hardware capability used to route emulated device IRQs as physical SPIs.
pub(crate) trait PhysicalSpiPlatform {
    /// Resolves the physical GIC target for the VM's primary vCPU.
    fn physical_spi_target_mpidr(_vm: &crate::vm::AxVM) -> AxVmResult<Option<usize>> {
        Ok(None)
    }

    /// Runs an ownership transition while holding the platform interrupt controller.
    fn with_physical_spi_controller<T, F>(_operation: F) -> AxVmResult<Option<T>>
    where
        F: FnOnce(&mut dyn crate::vm::PassthroughSpiController) -> AxVmResult<T>,
    {
        Ok(None)
    }
}

pub(crate) fn build_passthrough_spi_registrations<P: PhysicalSpiPlatform>(
    vm: &crate::vm::AxVM,
) -> AxVmResult<Vec<crate::vm::PassthroughSpiRegistration>> {
    if vm.interrupt_mode() != crate::config::VMInterruptMode::Passthrough {
        return Ok(Vec::new());
    }

    let devices = vm.get_devices()?;
    if devices.virtio_nets().is_empty() {
        return Ok(Vec::new());
    }
    let Some(target_mpidr) = P::physical_spi_target_mpidr(vm)? else {
        return Ok(Vec::new());
    };

    let mut registrations = Vec::new();
    registrations
        .try_reserve_exact(devices.virtio_nets().len())
        .map_err(|_| AxVmError::OutOfMemory {
            operation: "preallocating emulated passthrough SPI registrations",
        })?;
    registrations.extend(
        devices.virtio_nets().iter().map(|device| {
            crate::vm::PassthroughSpiRegistration::new(0, device.irq(), target_mpidr)
        }),
    );
    Ok(registrations)
}

pub(crate) fn try_inject_passthrough_device_irq<P: PhysicalSpiPlatform>(
    vm: &crate::vm::AxVM,
    irq: usize,
) -> AxVmResult<bool> {
    if vm.interrupt_mode() != crate::config::VMInterruptMode::Passthrough {
        return Ok(false);
    }
    let Some(target_mpidr) = P::physical_spi_target_mpidr(vm)? else {
        return Ok(false);
    };
    let runtime = vm.with_runtime(|runtime| Ok(runtime.clone()))?;
    let signal = P::with_physical_spi_controller(|controller| {
        runtime.transition_passthrough_spi(
            0,
            core::ops::ControlFlow::Continue(crate::vm::PassthroughSpiSignalRequest {
                irq,
                target_mpidr,
            }),
            controller,
        )
    })?
    .ok_or_else(|| {
        AxVmError::invalid_state(
            "route passthrough device IRQ",
            "the architecture selected a physical SPI target without a controller",
        )
    })?;
    let crate::vm::PassthroughSpiTransitionResult::Signal(signal) = signal else {
        return Err(AxVmError::invalid_state(
            "route passthrough device IRQ",
            "physical SPI signal returned a non-signal transition result",
        ));
    };
    if signal == crate::vm::PassthroughSpiSignal::Queued {
        runtime.notify_all();
    }
    Ok(true)
}

/// Guest firmware preparation performed before common VM memory loading.
pub(crate) trait GuestBootPlatform {
    fn init_guest_boot_resources() {}

    fn prepare_guest_boot(
        _vm_config: &mut crate::config::AxVMConfig,
        _vm_create_config: &mut axvmconfig::AxVMCrateConfig,
        _provider: &dyn crate::boot::BootImageProvider,
    ) -> AxVmResult<Option<crate::boot::fdt::GuestDtbImage>> {
        Ok(None)
    }
}

/// Architecture-specific guest image planning layered over common byte loading.
pub(crate) trait BootImagePlatform {
    fn default_boot_firmware_load_gpa(
        _config: &axvmconfig::AxVMCrateConfig,
    ) -> Option<axvm_types::GuestPhysAddr> {
        None
    }

    fn load_images_from_memory(
        loader: &mut crate::boot::images::ImageLoaderCore<'_>,
        images: crate::boot::StaticVmImage,
    ) -> AxVmResult {
        loader.load_standard_images_from_memory(images, Self::load_guest_dtb)
    }

    #[cfg(any(feature = "fs", feature = "host-fs"))]
    fn load_images_from_filesystem(
        loader: &mut crate::boot::images::ImageLoaderCore<'_>,
    ) -> AxVmResult {
        loader.load_standard_images_from_filesystem(Self::load_guest_dtb)
    }

    fn load_guest_dtb(
        _loader: &crate::boot::images::ImageLoaderCore<'_>,
        _dtb: &crate::boot::fdt::GuestDtbImage,
    ) -> AxVmResult {
        Ok(())
    }

    fn is_x86_linux_image_config(
        _config: &axvmconfig::AxVMCrateConfig,
        _provider: &dyn crate::boot::BootImageProvider,
    ) -> bool {
        false
    }
}

/// Architecture-specific host timer policy used by the ArceOS adapter.
pub(crate) trait HostTimePlatform {
    fn set_oneshot_timer(deadline_ns: u64) {
        ax_std::os::arceos::modules::ax_hal::time::set_oneshot_timer(deadline_ns);
    }

    fn register_timer_callback() {}
}
