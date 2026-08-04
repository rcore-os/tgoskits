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

//! Type-safe services contributed by emulated devices.

use alloc::{sync::Arc, vec::Vec};
use core::{
    any::{Any, TypeId},
    marker::PhantomData,
};

use crate::{DeviceManagerError, DeviceManagerResult};

/// Declares whether a service key may have one or several providers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceCardinality {
    /// Exactly zero or one provider may be registered for this key.
    Single,
    /// Any number of providers may be registered for this key.
    Multiple,
}

/// Names and types one service made available by a device contribution.
///
/// Callers use a concrete key type rather than downcasting a device or
/// inspecting an untyped central enum. The service implementation itself may
/// be a trait object.
pub trait ServiceKey: Send + Sync + 'static {
    /// The service interface associated with this key.
    type Service: ?Sized + Send + Sync + 'static;

    /// Stable diagnostic name for this service class.
    const NAME: &'static str;

    /// Number of providers allowed for this key.
    const CARDINALITY: ServiceCardinality;
}

struct TypedService<K: ServiceKey> {
    service: Arc<K::Service>,
    _key: PhantomData<K>,
}

struct ServiceEntry {
    key: TypeId,
    name: &'static str,
    cardinality: ServiceCardinality,
    service: Arc<dyn Any + Send + Sync>,
}

/// VM-local registry of typed services.
///
/// Type erasure is private to this module. Consumers can only retrieve a
/// service through its concrete [`ServiceKey`], so service dependencies remain
/// visible in their type signatures.
#[derive(Default)]
pub struct DeviceServices {
    entries: Vec<ServiceEntry>,
}

impl DeviceServices {
    /// Creates an empty service registry.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Adds one service provider.
    ///
    /// # Errors
    ///
    /// Returns an error when a single-provider key already has a provider.
    pub fn provide<K: ServiceKey>(&mut self, service: Arc<K::Service>) -> DeviceManagerResult {
        self.validate_entry::<K>()?;
        let typed = Arc::new(TypedService::<K> {
            service,
            _key: PhantomData,
        });
        self.entries.push(ServiceEntry {
            key: TypeId::of::<K>(),
            name: K::NAME,
            cardinality: K::CARDINALITY,
            service: typed,
        });
        Ok(())
    }

    /// Returns the unique provider for `K`.
    ///
    /// # Errors
    ///
    /// Returns an error when no provider is available or the key allows more
    /// than one provider.
    pub fn require<K: ServiceKey>(&self) -> DeviceManagerResult<Arc<K::Service>> {
        if K::CARDINALITY == ServiceCardinality::Multiple {
            return Err(DeviceManagerError::InvalidInput {
                operation: "require device service",
                detail: alloc::format!(
                    "service '{}' permits multiple providers; use all() instead",
                    K::NAME
                ),
            });
        }

        self.entries
            .iter()
            .find(|entry| entry.key == TypeId::of::<K>())
            .and_then(service_from_entry::<K>)
            .ok_or_else(|| DeviceManagerError::ResourceNotFound {
                operation: "require device service",
                resource: K::NAME.into(),
            })
    }

    /// Returns every provider registered for `K`.
    pub fn all<K: ServiceKey>(&self) -> Vec<Arc<K::Service>> {
        self.entries
            .iter()
            .filter(|entry| entry.key == TypeId::of::<K>())
            .filter_map(service_from_entry::<K>)
            .collect()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn validate_merge(&self, incoming: &Self) -> DeviceManagerResult {
        for entry in &incoming.entries {
            if entry.cardinality == ServiceCardinality::Single
                && self
                    .entries
                    .iter()
                    .any(|existing| existing.key == entry.key)
            {
                return Err(DeviceManagerError::ResourceConflict {
                    operation: "register device service",
                    detail: alloc::format!(
                        "single-provider service '{}' is already registered",
                        entry.name
                    ),
                });
            }
            if incoming
                .entries
                .iter()
                .filter(|other| other.key == entry.key)
                .count()
                > 1
                && entry.cardinality == ServiceCardinality::Single
            {
                return Err(DeviceManagerError::ResourceConflict {
                    operation: "register device service",
                    detail: alloc::format!(
                        "device contribution registers service '{}' more than once",
                        entry.name
                    ),
                });
            }
        }
        Ok(())
    }

    pub(crate) fn append(&mut self, incoming: Self) {
        self.entries.extend(incoming.entries);
    }

    fn validate_entry<K: ServiceKey>(&self) -> DeviceManagerResult {
        if K::CARDINALITY == ServiceCardinality::Single
            && self
                .entries
                .iter()
                .any(|entry| entry.key == TypeId::of::<K>())
        {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "provide device service",
                detail: alloc::format!(
                    "single-provider service '{}' is already registered",
                    K::NAME
                ),
            });
        }
        Ok(())
    }
}

fn service_from_entry<K: ServiceKey>(entry: &ServiceEntry) -> Option<Arc<K::Service>> {
    entry
        .service
        .downcast_ref::<TypedService<K>>()
        .map(|typed| Arc::clone(&typed.service))
}

#[cfg(test)]
mod tests {
    use super::*;

    trait TestService: Send + Sync {
        fn value(&self) -> u32;
    }

    struct TestServiceKey;

    impl ServiceKey for TestServiceKey {
        type Service = dyn TestService;

        const NAME: &'static str = "test-service";
        const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
    }

    struct TestServiceProvider(u32);

    impl TestService for TestServiceProvider {
        fn value(&self) -> u32 {
            self.0
        }
    }

    #[test]
    fn require_returns_service_by_typed_key() {
        let mut services = DeviceServices::new();
        let provider: Arc<dyn TestService> = Arc::new(TestServiceProvider(7));
        services.provide::<TestServiceKey>(provider).unwrap();

        assert_eq!(services.require::<TestServiceKey>().unwrap().value(), 7);
    }

    #[test]
    fn single_provider_service_rejects_duplicate_registration() {
        let mut services = DeviceServices::new();
        services
            .provide::<TestServiceKey>(Arc::new(TestServiceProvider(1)))
            .unwrap();

        assert!(matches!(
            services.provide::<TestServiceKey>(Arc::new(TestServiceProvider(2))),
            Err(DeviceManagerError::ResourceConflict {
                operation: "provide device service",
                ..
            })
        ));
    }
}
