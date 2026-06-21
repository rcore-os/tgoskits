// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Device factory catalog used during VM device initialization.

use alloc::format;

use ax_errno::ax_err_type;
use axdevice_base::{
    DeviceError, DeviceFactory, DeviceFactoryRegister, DeviceResult, EmuDeviceType,
};

/// Returns device factory registration entries collected from linker sections.
#[cfg(target_os = "none")]
pub fn linker_device_factory_registers() -> DeviceResult<&'static [DeviceFactoryRegister]> {
    unsafe extern "C" {
        fn __saxdevice_factory();
        fn __eaxdevice_factory();
    }

    let start = __saxdevice_factory as *const () as usize;
    let end = __eaxdevice_factory as *const () as usize;
    if start > end {
        return Err(DeviceError::from(ax_err_type!(
            BadState,
            format!("invalid axdevice factory section range: start={start:#x}, end={end:#x}")
        )));
    }

    let len = end - start;
    if len == 0 {
        return Ok(&[]);
    }

    let align = core::mem::align_of::<DeviceFactoryRegister>();
    if !start.is_multiple_of(align) {
        return Err(DeviceError::from(ax_err_type!(
            BadState,
            format!("misaligned axdevice factory section: start={start:#x}, align={align}")
        )));
    }

    let entry_size = core::mem::size_of::<DeviceFactoryRegister>();
    if !len.is_multiple_of(entry_size) {
        return Err(DeviceError::from(ax_err_type!(
            BadState,
            format!(
                "invalid axdevice factory section length: len={len:#x}, entry_size={entry_size}"
            )
        )));
    }

    let count = len / entry_size;
    unsafe { Ok(core::slice::from_raw_parts(start as *const _, count)) }
}

/// Returns an empty linker factory list for non-bare-metal host builds.
#[cfg(not(target_os = "none"))]
pub fn linker_device_factory_registers() -> DeviceResult<&'static [DeviceFactoryRegister]> {
    Ok(&[])
}

enum DeviceFactorySource<'a> {
    Factories(&'a [&'a dyn DeviceFactory]),
    Registers(&'a [DeviceFactoryRegister]),
}

/// A platform-provided catalog of device factories.
pub struct DeviceFactoryCatalog<'a> {
    source: DeviceFactorySource<'a>,
}

impl<'a> DeviceFactoryCatalog<'a> {
    /// Creates a catalog from a platform-provided factory slice.
    pub const fn new(factories: &'a [&'a dyn DeviceFactory]) -> Self {
        Self {
            source: DeviceFactorySource::Factories(factories),
        }
    }

    /// Creates a catalog from linker-collected factory registration entries.
    pub const fn from_registers(registers: &'a [DeviceFactoryRegister]) -> Self {
        Self {
            source: DeviceFactorySource::Registers(registers),
        }
    }

    /// Creates a catalog from the final image's linker-collected entries.
    pub fn from_linker() -> DeviceResult<DeviceFactoryCatalog<'static>> {
        let registers = linker_device_factory_registers()?;
        Ok(DeviceFactoryCatalog::from_registers(registers))
    }

    /// Finds the first factory handling the given emulated device type.
    pub fn find(&self, ty: EmuDeviceType) -> Option<&'a dyn DeviceFactory> {
        match self.source {
            DeviceFactorySource::Factories(factories) => {
                factories.iter().copied().find(|factory| factory.ty() == ty)
            }
            DeviceFactorySource::Registers(registers) => registers
                .iter()
                .find(|register| register.factory().ty() == ty)
                .map(DeviceFactoryRegister::factory),
        }
    }

    /// Finds a factory handling the type and reports duplicate registrations.
    pub fn find_unique(&self, ty: EmuDeviceType) -> DeviceResult<Option<&'a dyn DeviceFactory>> {
        let mut matched: Option<(&'a dyn DeviceFactory, &'static str)> = None;

        match self.source {
            DeviceFactorySource::Factories(factories) => {
                for factory in factories.iter().copied() {
                    if factory.ty() != ty {
                        continue;
                    }
                    if let Some((_, existing_name)) = matched {
                        return Err(duplicate_factory_error(ty, existing_name, "<anonymous>"));
                    }
                    matched = Some((factory, "<anonymous>"));
                }
            }
            DeviceFactorySource::Registers(registers) => {
                for register in registers {
                    let factory = register.factory();
                    if factory.ty() != ty {
                        continue;
                    }
                    if let Some((_, existing_name)) = matched {
                        return Err(duplicate_factory_error(ty, existing_name, register.name()));
                    }
                    matched = Some((factory, register.name()));
                }
            }
        }

        Ok(matched.map(|(factory, _)| factory))
    }
}

fn duplicate_factory_error(
    ty: EmuDeviceType,
    existing_name: &'static str,
    duplicate_name: &'static str,
) -> DeviceError {
    DeviceError::from(ax_err_type!(
        BadState,
        format!("duplicate device factories for {ty:?}: {existing_name} and {duplicate_name}")
    ))
}
