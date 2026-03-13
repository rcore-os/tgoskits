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

//! RISC-V Hypervisor Extension Registers for 64-bit Systems
//!
//! This module contains implementations of all hypervisor and virtual supervisor
//! registers for 64-bit RISC-V systems with the hypervisor extension.

/// Hypervisor counter enable register
pub mod hcounteren;
/// Hypervisor exception delegation register  
pub mod hedeleg;
/// Hypervisor guest address translation and protection register
pub mod hgatp;
/// Hypervisor guest external interrupt enable register
pub mod hgeie;
/// Hypervisor guest external interrupt pending register
pub mod hgeip;
/// Hypervisor interrupt delegation register
pub mod hideleg;
/// Hypervisor interrupt enable register
pub mod hie;
/// Hypervisor interrupt pending register
pub mod hip;
/// Hypervisor status register
pub mod hstatus;
/// Hypervisor time delta register
pub mod htimedelta;
/// Hypervisor time delta high register (for RV32)
pub mod htimedeltah;
/// Hypervisor trap instruction register
pub mod htinst;
/// Hypervisor trap value register
pub mod htval;
/// Hypervisor virtual interrupt pending register
pub mod hvip;
/// Virtual supervisor address translation and protection register
pub mod vsatp;
/// Virtual supervisor cause register
pub mod vscause;
/// Virtual supervisor exception program counter
pub mod vsepc;
/// Virtual supervisor interrupt enable register
pub mod vsie;
/// Virtual supervisor interrupt pending register
pub mod vsip;
/// Virtual supervisor scratch register
pub mod vsscratch;
/// Virtual supervisor status register
pub mod vsstatus;
/// Virtual supervisor trap value register
pub mod vstval;
/// Virtual supervisor trap vector register
pub mod vstvec;
