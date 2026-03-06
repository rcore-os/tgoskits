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

use alloc::boxed::Box;

use rdrive::{PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo};
use some_serial::{BSerial, ns16550, pl011};

use crate::iomap;

module_driver!(
    name: "common serial",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["arm,pl011", "snps,dw-apb-uart"],
            on_probe: probe
        }
    ],
);

fn probe(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    info!("Probing serial device: {}", info.node.name());
    let base_reg = info
        .node
        .reg()
        .and_then(|mut regs| regs.next())
        .ok_or(OnProbeError::other(alloc::format!(
            "[{}] has no reg",
            info.node.name()
        )))?;

    let mmio_size = base_reg.size.unwrap_or(0x1000);
    let mmio_base = iomap(base_reg.address, mmio_size)?;

    let clock_freq = info.node.clock_frequency().unwrap_or(24_000_000);

    let mut serial: Option<BSerial> = None;
    for c in info.node.compatibles() {
        if c == "arm,pl011" {
            serial = Some(Box::new(pl011::Pl011::new(mmio_base, clock_freq)));
            break;
        }

        if c == "snps,dw-apb-uart" {
            serial = Some(Box::new(ns16550::Ns16550::new_mmio(mmio_base, clock_freq)));
            break;
        }
    }
    if let Some(s) = serial {
        info!("Serial@{:#x} registered successfully", s.base());
        plat_dev.register(s);
    }

    Ok(())
}
