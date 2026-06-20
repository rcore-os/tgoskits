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

use axdevice_base::{DeviceFactory, EmuDeviceType};

/// A platform-provided catalog of device factories.
pub struct DeviceFactoryCatalog<'a> {
    factories: &'a [&'a dyn DeviceFactory],
}

impl<'a> DeviceFactoryCatalog<'a> {
    /// Creates a catalog from a platform-provided factory slice.
    pub const fn new(factories: &'a [&'a dyn DeviceFactory]) -> Self {
        Self { factories }
    }

    /// Finds the factory handling the given emulated device type.
    pub fn find(&self, ty: EmuDeviceType) -> Option<&'a dyn DeviceFactory> {
        self.factories
            .iter()
            .copied()
            .find(|factory| factory.ty() == ty)
    }
}
