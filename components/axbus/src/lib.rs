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

//! Unified device & bus abstraction for ArceOS hypervisor (AxVisor).
//!
//! This crate provides the foundational abstractions for implementing virtual
//! devices in a hypervisor. It is designed for `no_std` environments and
//! supports multiple architectures.
//!
//! # Architecture
//!
//! ```text
//!  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐
//!  │  trait.rs    │    │  adapter.rs  │    │  registry.rs │
//!  │  (concepts)  │───▶│  (legacy →   │───▶│  (slotmap +  │
//!  │              │    │   VirtualDev)│    │   interval)  │
//!  └──────────────┘    └──────────────┘    └──────────────┘
//!                                               │
//!  ┌──────────────┐    ┌──────────────┐         │
//!  │  ivc.rs      │    │  factory.rs  │         │
//!  │  (IVC mgmt)  │    │  (factory    │◀────────┘
//!  │              │    │   registry)  │
//!  └──────────────┘    └──────────────┘
//!         │                   │
//!         ▼                   ▼
//!  ┌──────────────────────────────────┐
//!  │          router.rs               │
//!  │  (BusRouter: dispatch + intr)    │
//!  └──────────────────────────────────┘
//! ```

#![cfg_attr(not(test), no_std)]
#![allow(missing_docs)]

extern crate alloc;


mod adapter;
mod factory;
pub mod irq;
mod ivc;
mod registry;
mod router;
pub mod r#trait;
mod send_sync;

pub use adapter::{LegacyMmioAdapter, LegacyPortAdapter, LegacySysRegAdapter};
// Re-exports from adapter — all types used by axvisor

pub use factory::FactoryRegistry;
pub use ivc::IVCManager;
pub use registry::DeviceRegistry;
pub use router::BusRouter;
pub use irq::*;
pub use r#trait::*;
