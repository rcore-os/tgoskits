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

//! Device factory registry — eliminates the giant `match` in the old
//! `AxVmDevices::init()`.
//!
//! # Usage

use alloc::{boxed::Box, collections::BTreeMap, vec::Vec};

use axvmconfig::{EmulatedDeviceConfig, EmulatedDeviceType};

use crate::r#trait::*;

/// A registry of device factories, keyed by `EmulatedDeviceType`.
///
/// This replaces the old pattern:
/// ```text
/// match config.emu_type {
///     EmulatedDeviceType::InterruptController => { … }
///     EmulatedDeviceType::VirtioBlk => { … }
///     _ => warn!("unsupported"),
/// }
/// ```
///
/// With:
/// ```text
/// factories.get(config.emu_type)?.create(config, &mut id_alloc)
/// ```
///
/// No core code changes needed when adding a new device type!
/// Wrapper around `EmulatedDeviceType` that also implements `Ord`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TypeKey(EmulatedDeviceType);

impl From<EmulatedDeviceType> for TypeKey {
    fn from(t: EmulatedDeviceType) -> Self {
        Self(t)
    }
}

impl PartialOrd for TypeKey {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TypeKey {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        (self.0 as u8).cmp(&(other.0 as u8))
    }
}

pub struct FactoryRegistry {
    factories: BTreeMap<TypeKey, Box<dyn DeviceFactory>>,
}

impl FactoryRegistry {
    /// Create an empty factory registry.
    pub fn new() -> Self {
        Self {
            factories: BTreeMap::new(),
        }
    }

    /// Register a factory for a specific device type.
    ///
    /// If a factory already exists for this type, it is overwritten
    /// (last-registered wins — useful for injecting test doubles).
    pub fn register(&mut self, factory: Box<dyn DeviceFactory>) {
        let key: TypeKey = factory.emu_type().into();
        self.factories.insert(key, factory);
    }

    /// Check if a factory exists for the given device type.
    pub fn has_type(&self, emu_type: EmulatedDeviceType) -> bool {
        let key: TypeKey = emu_type.into();
        self.factories.contains_key(&key)
    }

    /// Create a device from its configuration, or return an error if no
    /// factory is registered.
    pub fn create(
        &self,
        emu_type: EmulatedDeviceType,
        config: &EmulatedDeviceConfig,
        id_alloc: &mut dyn FnMut() -> DeviceId,
    ) -> Result<DeviceBundle> {
        let key: TypeKey = emu_type.into();
        let factory = self.factories.get(&key).ok_or(DeviceError::NotFound)?;
        factory.create(config, id_alloc)
    }

    /// Create devices from a list of configurations.
    ///
    /// This is the main entry point that replaces the old `AxVmDevices::init()`.
    pub fn create_all(
        &self,
        configs: &[EmulatedDeviceConfig],
        id_alloc: &mut dyn FnMut() -> DeviceId,
    ) -> Vec<Result<DeviceBundle>> {
        configs
            .iter()
            .map(|cfg| self.create(cfg.emu_type, cfg, id_alloc))
            .collect()
    }

    /// Number of registered factory types.
    pub fn len(&self) -> usize {
        self.factories.len()
    }

    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
    }

    /// Iterate over all registered factory types.
    pub fn types(&self) -> impl Iterator<Item = EmulatedDeviceType> + '_ {
        self.factories.keys().map(|k| k.0)
    }
}

impl Default for FactoryRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(missing_docs, dead_code)]
mod tests {
    use core::any::Any;

    use axvmconfig::{EmulatedDeviceConfig, EmulatedDeviceType};

    use super::*;

    struct DummyFactory;

    impl DeviceFactory for DummyFactory {
        fn emu_type(&self) -> EmulatedDeviceType {
            EmulatedDeviceType::Dummy
        }

        fn create(
            &self,
            _config: &EmulatedDeviceConfig,
            id_alloc: &mut dyn FnMut() -> DeviceId,
        ) -> Result<DeviceBundle> {
            let id = id_alloc();
            Ok(DeviceBundle::single(Box::new(DummyDevice { id })))
        }
    }

    #[derive(Debug)]
    struct DummyDevice {
        id: DeviceId,
    }

    impl VirtualDevice for DummyDevice {
        fn id(&self) -> DeviceId {
            self.id
        }
        fn name(&self) -> &str {
            "dummy-factory"
        }
        fn resources(&self) -> &[Resource] {
            &[]
        }
        fn handle_access(&self, _bus: BusKind, _access: &BusAccess) -> BusResponse {
            BusResponse::Success(Some(0))
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[test]
    fn test_create_unknown_type() {
        // DummyFactory only registers as Dummy; VirtioBlk doesn't exist.
        // This tests the "factory not found" path.
        let mut reg = FactoryRegistry::new();
        let mut alloc = || DeviceId::from_u64(1);
        reg.register(Box::new(DummyFactory));
        let result = reg.create(
            EmulatedDeviceType::VirtioBlk,
            &EmulatedDeviceConfig::default(),
            &mut alloc,
        );
        assert!(matches!(result, Err(DeviceError::NotFound)));
    }

    fn test_register_and_create() {
        let mut reg = FactoryRegistry::new();
        reg.register(Box::new(DummyFactory));

        let mut counter = 0u64;
        let mut alloc = || {
            counter += 1;
            DeviceId::from_u64(counter)
        };

        let bundle = reg
            .create(
                EmulatedDeviceType::Dummy,
                &EmulatedDeviceConfig::default(),
                &mut alloc,
            )
            .unwrap();
        assert_eq!(bundle.devices[0].name(), "dummy-factory");
    }

    #[test]
    fn test_create_all() {
        let mut reg = FactoryRegistry::new();
        reg.register(Box::new(DummyFactory));

        // create_all uses EmulatedDeviceType from each config; the default config
        // has emu_type = Dummy, so DummyFactory handles it.
        let configs = alloc::vec![EmulatedDeviceConfig::default(); 3];
        let mut counter = 0u64;
        let mut alloc = || {
            counter += 1;
            DeviceId::from_u64(counter)
        };

        let results = reg.create_all(&configs, &mut alloc);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_has_type() {
        let mut reg = FactoryRegistry::new();
        assert!(!reg.has_type(EmulatedDeviceType::VirtioBlk));
        reg.register(Box::new(DummyFactory));
        assert!(reg.has_type(EmulatedDeviceType::Dummy));
    }

    #[test]
    fn test_empty_factory_defaults() {
        let reg = FactoryRegistry::default();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn test_overwrite_factory() {
        struct OtherFactory;
        impl DeviceFactory for OtherFactory {
            fn emu_type(&self) -> EmulatedDeviceType {
                EmulatedDeviceType::Dummy
            }
            fn create(
                &self,
                _c: &EmulatedDeviceConfig,
                _a: &mut dyn FnMut() -> DeviceId,
            ) -> Result<DeviceBundle> {
                Err(DeviceError::BackendError("from Other".into()))
            }
        }
        let mut reg = FactoryRegistry::new();
        reg.register(Box::new(DummyFactory));
        assert!(
            reg.create(
                EmulatedDeviceType::Dummy,
                &EmulatedDeviceConfig::default(),
                &mut || DeviceId(1)
            )
            .is_ok()
        );
        reg.register(Box::new(OtherFactory));
        let r = reg.create(
            EmulatedDeviceType::Dummy,
            &EmulatedDeviceConfig::default(),
            &mut || DeviceId(1),
        );
        assert!(matches!(r, Err(DeviceError::BackendError(_))));
    }
}
